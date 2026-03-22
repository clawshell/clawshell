use crate::dlp::DlpScanner;
use axum::body::Body;
use bytes::{Bytes, BytesMut};
use futures_util::Stream;
use serde_json::Value;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tracing::warn;

#[derive(Debug, thiserror::Error)]
pub enum TranslateError {
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("missing field: {0}")]
    MissingField(&'static str),
}

/// Fields that are compatible between chat/completions and responses API.
const PASSTHROUGH_FIELDS: &[&str] = &["model", "stream", "temperature", "top_p", "stop"];

/// Fields that must be stripped from chat/completions requests (not supported by responses API).
const STRIP_FIELDS: &[&str] = &[
    "frequency_penalty",
    "presence_penalty",
    "logprobs",
    "top_logprobs",
    "logit_bias",
    "n",
    "response_format",
    "seed",
    "service_tier",
    "user",
];

/// Translate a `/v1/chat/completions` request body to `/v1/responses` format.
pub fn chat_completions_to_responses(body: &[u8]) -> Result<Vec<u8>, TranslateError> {
    let req: Value = serde_json::from_slice(body)?;
    let obj = req
        .as_object()
        .ok_or(TranslateError::MissingField("root object"))?;

    let messages = obj
        .get("messages")
        .and_then(Value::as_array)
        .ok_or(TranslateError::MissingField("messages"))?;

    let mut result = serde_json::Map::new();

    // Separate system messages → instructions, rest → input
    let mut system_parts: Vec<&str> = Vec::new();
    let mut input: Vec<Value> = Vec::new();

    for msg in messages {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
        if role == "system" {
            if let Some(content) = msg.get("content").and_then(Value::as_str) {
                system_parts.push(content);
            }
        } else {
            input.push(convert_message_content(msg.clone()));
        }
    }

    // Codex responses API requires `instructions` even when empty
    result.insert(
        "instructions".to_string(),
        Value::String(system_parts.join("\n")),
    );
    result.insert("input".to_string(), Value::Array(input));

    // Rename max_tokens → max_output_tokens
    if let Some(max_tokens) = obj.get("max_tokens") {
        result.insert("max_output_tokens".to_string(), max_tokens.clone());
    }

    // Pass through compatible fields
    for &field in PASSTHROUGH_FIELDS {
        if let Some(value) = obj.get(field) {
            result.insert(field.to_string(), value.clone());
        }
    }

    // Strip incompatible fields — they are simply not copied over.
    // (No action needed since we build a new object.)
    let _ = STRIP_FIELDS; // acknowledge the constant is used by design

    Ok(serde_json::to_vec(&Value::Object(result))?)
}

/// Convert a chat/completions message to a Responses API input item.
/// - Adds `type: "message"` (required by Responses API)
/// - For user messages: converts content `type: "text"` → `type: "input_text"`
/// - For assistant messages: converts content `type: "text"` → `type: "output_text"`
/// - Converts content `type: "image_url"` → `type: "input_image"`
/// - String content is left as-is (the Responses API accepts string content directly).
fn convert_message_content(mut msg: Value) -> Value {
    let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
    let is_assistant = role == "assistant";

    // Responses API requires "type": "message" on each input item
    if let Some(obj) = msg.as_object_mut() {
        if !obj.contains_key("type") {
            obj.insert("type".to_string(), Value::String("message".to_string()));
        }
    }

    let Some(content) = msg.get_mut("content") else {
        return msg;
    };
    let Some(parts) = content.as_array_mut() else {
        // String content — no conversion needed
        return msg;
    };
    for part in parts.iter_mut() {
        let Some(obj) = part.as_object_mut() else {
            continue;
        };
        match obj.get("type").and_then(Value::as_str) {
            Some("text") => {
                let text_type = if is_assistant {
                    "output_text"
                } else {
                    "input_text"
                };
                obj.insert("type".to_string(), Value::String(text_type.to_string()));
            }
            Some("image_url") => {
                obj.insert("type".to_string(), Value::String("input_image".to_string()));
            }
            _ => {}
        }
    }
    msg
}

/// Translate a `/v1/responses` response body to `/v1/chat/completions` format.
pub fn responses_to_chat_completion(body: &[u8]) -> Result<Vec<u8>, TranslateError> {
    let resp: Value = serde_json::from_slice(body)?;
    let obj = resp
        .as_object()
        .ok_or(TranslateError::MissingField("root object"))?;

    let id = obj
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("chatcmpl-translate");
    let model = obj
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("unknown");

    // Extract text content from output[].content[].text where type == "output_text"
    let mut content_parts: Vec<&str> = Vec::new();
    if let Some(output) = obj.get("output").and_then(Value::as_array) {
        for item in output {
            if item.get("type").and_then(Value::as_str) == Some("message") {
                if let Some(content) = item.get("content").and_then(Value::as_array) {
                    for part in content {
                        if part.get("type").and_then(Value::as_str) == Some("output_text") {
                            if let Some(text) = part.get("text").and_then(Value::as_str) {
                                content_parts.push(text);
                            }
                        }
                    }
                }
            }
        }
    }
    let content = content_parts.join("");

    // Map status → finish_reason
    let finish_reason = match obj.get("status").and_then(Value::as_str) {
        Some("completed") | None => "stop",
        Some("incomplete") => "length",
        Some("failed") => "stop",
        Some(_) => "stop",
    };

    // Map usage
    let usage = if let Some(u) = obj.get("usage") {
        serde_json::json!({
            "prompt_tokens": u.get("input_tokens").and_then(Value::as_u64).unwrap_or(0),
            "completion_tokens": u.get("output_tokens").and_then(Value::as_u64).unwrap_or(0),
            "total_tokens":
                u.get("input_tokens").and_then(Value::as_u64).unwrap_or(0)
                + u.get("output_tokens").and_then(Value::as_u64).unwrap_or(0)
        })
    } else {
        serde_json::json!({ "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0 })
    };

    let result = serde_json::json!({
        "id": id,
        "object": "chat.completion",
        "model": model,
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": content,
            },
            "finish_reason": finish_reason,
        }],
        "usage": usage,
    });

    Ok(serde_json::to_vec(&result)?)
}

/// Translate a single SSE line from Responses API format to chat.completion.chunk format.
///
/// Returns `Some(line(s))` for events that map to chat completions output,
/// or `None` for events that should be suppressed.
///
/// `response_id` and `model` are captured from early events and reused in later chunks.
pub fn translate_sse_line(
    line: &str,
    response_id: &mut Option<String>,
    model: &mut Option<String>,
) -> Option<String> {
    // Pass through [DONE]
    if line.starts_with("data: [DONE]") {
        return Some(line.to_string());
    }

    // Only process data: lines with JSON
    let json_str = line.strip_prefix("data: ")?;

    let event: Value = serde_json::from_str(json_str).ok()?;
    let event_type = event.get("type").and_then(Value::as_str)?;

    match event_type {
        "response.created" | "response.in_progress" => {
            // Capture response ID and model from these early events
            if let Some(resp) = event.get("response") {
                if let Some(id) = resp.get("id").and_then(Value::as_str) {
                    *response_id = Some(id.to_string());
                }
                if let Some(m) = resp.get("model").and_then(Value::as_str) {
                    *model = Some(m.to_string());
                }
            }
            None // suppress
        }

        "response.output_text.delta" => {
            let delta = event.get("delta").and_then(Value::as_str).unwrap_or("");
            let id = response_id.as_deref().unwrap_or("chatcmpl-translate");
            let m = model.as_deref().unwrap_or("unknown");
            let chunk = serde_json::json!({
                "id": id,
                "object": "chat.completion.chunk",
                "model": m,
                "choices": [{
                    "index": 0,
                    "delta": { "content": delta },
                    "finish_reason": null,
                }]
            });
            Some(format!(
                "data: {}",
                serde_json::to_string(&chunk).unwrap_or_default()
            ))
        }

        "response.completed" => {
            let id = response_id.as_deref().unwrap_or("chatcmpl-translate");
            let m = model.as_deref().unwrap_or("unknown");
            let final_chunk = serde_json::json!({
                "id": id,
                "object": "chat.completion.chunk",
                "model": m,
                "choices": [{
                    "index": 0,
                    "delta": {},
                    "finish_reason": "stop",
                }]
            });
            Some(format!(
                "data: {}\n\ndata: [DONE]",
                serde_json::to_string(&final_chunk).unwrap_or_default()
            ))
        }

        "response.failed" => {
            let id = response_id.as_deref().unwrap_or("chatcmpl-translate");
            let m = model.as_deref().unwrap_or("unknown");
            let final_chunk = serde_json::json!({
                "id": id,
                "object": "chat.completion.chunk",
                "model": m,
                "choices": [{
                    "index": 0,
                    "delta": {},
                    "finish_reason": "stop",
                }]
            });
            Some(format!(
                "data: {}\n\ndata: [DONE]",
                serde_json::to_string(&final_chunk).unwrap_or_default()
            ))
        }

        "response.incomplete" => {
            let id = response_id.as_deref().unwrap_or("chatcmpl-translate");
            let m = model.as_deref().unwrap_or("unknown");
            let final_chunk = serde_json::json!({
                "id": id,
                "object": "chat.completion.chunk",
                "model": m,
                "choices": [{
                    "index": 0,
                    "delta": {},
                    "finish_reason": "length",
                }]
            });
            Some(format!(
                "data: {}\n\ndata: [DONE]",
                serde_json::to_string(&final_chunk).unwrap_or_default()
            ))
        }

        // Suppress all structural/metadata events
        "response.output_text.done"
        | "response.content_part.added"
        | "response.content_part.done"
        | "response.output_item.added"
        | "response.output_item.done" => None,

        // Suppress any other unknown events
        _ => None,
    }
}

/// A stream adapter that wraps an axum Body and translates Responses API SSE events
/// to chat.completion.chunk format.
pub struct TranslateStream {
    inner: Pin<Box<dyn Stream<Item = Result<Bytes, axum::Error>> + Send>>,
    buffer: BytesMut,
    response_id: Option<String>,
    model: Option<String>,
    output_buffer: Vec<u8>,
}

impl std::fmt::Debug for TranslateStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TranslateStream")
            .field("buffer_len", &self.buffer.len())
            .field("response_id", &self.response_id)
            .field("model", &self.model)
            .finish()
    }
}

impl TranslateStream {
    pub fn new(body: Body) -> Self {
        use futures_util::StreamExt;
        use http_body_util::BodyStream;

        let stream = BodyStream::new(body).filter_map(|result| async move {
            match result {
                Ok(frame) => frame.into_data().ok().map(Ok),
                Err(e) => Some(Err(e)),
            }
        });

        Self {
            inner: Box::pin(stream),
            buffer: BytesMut::new(),
            response_id: None,
            model: None,
            output_buffer: Vec::new(),
        }
    }

    fn process_buffered_lines(&mut self) {
        loop {
            let Some(pos) = self.buffer.iter().position(|&b| b == b'\n') else {
                break;
            };

            let line_bytes = self.buffer.split_to(pos + 1);
            let line = String::from_utf8_lossy(&line_bytes).trim().to_string();

            if line.is_empty() {
                self.output_buffer.extend_from_slice(b"\n");
                continue;
            }

            let rid = &mut self.response_id;
            let mdl = &mut self.model;
            if let Some(translated) = translate_sse_line(&line, rid, mdl) {
                self.output_buffer.extend_from_slice(translated.as_bytes());
                self.output_buffer.extend_from_slice(b"\n\n");
            }
        }
    }
}

impl Stream for TranslateStream {
    type Item = Result<Bytes, axum::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            // First, drain any pending output
            if !this.output_buffer.is_empty() {
                let data = std::mem::take(&mut this.output_buffer);
                return Poll::Ready(Some(Ok(Bytes::from(data))));
            }

            // Poll the inner stream for more data
            match this.inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(chunk))) => {
                    this.buffer.extend_from_slice(&chunk);
                    this.process_buffered_lines();
                    // Loop to check if we produced output
                }
                Poll::Ready(Some(Err(e))) => return Poll::Ready(Some(Err(e))),
                Poll::Ready(None) => {
                    // Stream ended — process any remaining buffer
                    if !this.buffer.is_empty() {
                        let remaining = std::mem::take(&mut this.buffer);
                        let line = String::from_utf8_lossy(&remaining).trim().to_string();
                        if !line.is_empty() {
                            if let Some(translated) =
                                translate_sse_line(&line, &mut this.response_id, &mut this.model)
                            {
                                return Poll::Ready(Some(Ok(Bytes::from(format!(
                                    "{translated}\n\n"
                                )))));
                            }
                        }
                    }
                    return Poll::Ready(None);
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// Wrap a Body in a TranslateStream and return a new Body.
pub fn wrap_body_with_translate_stream(body: Body) -> Body {
    Body::from_stream(TranslateStream::new(body))
}

// ---------------------------------------------------------------------------
// DLP scanning for SSE streams
// ---------------------------------------------------------------------------

/// Apply DLP redaction to a single SSE `data:` line.
///
/// Parses the JSON, extracts `choices[0].delta.content`, runs redaction on it,
/// and patches the JSON back if any PII was found. Returns the (possibly
/// modified) line.
///
/// Lines that are not `data:` JSON or don't contain delta content are returned
/// unchanged.
pub fn redact_sse_data_line(line: &str, scanner: &DlpScanner) -> String {
    // Only process data: lines with JSON
    let Some(json_str) = line.strip_prefix("data: ") else {
        return line.to_string();
    };

    // Don't touch [DONE]
    if json_str.starts_with("[DONE]") {
        return line.to_string();
    }

    let Ok(mut event) = serde_json::from_str::<Value>(json_str) else {
        return line.to_string();
    };

    // Extract delta.content from choices[0]
    let Some(content) = event
        .get_mut("choices")
        .and_then(Value::as_array_mut)
        .and_then(|choices| choices.first_mut())
        .and_then(|choice| choice.get_mut("delta"))
        .and_then(|delta| delta.get_mut("content"))
    else {
        return line.to_string();
    };

    let Some(text) = content.as_str() else {
        return line.to_string();
    };

    let (redacted, redacted_names) = scanner.redact_all(text.as_bytes());
    if redacted_names.is_empty() {
        return line.to_string();
    }

    warn!(
        redacted_patterns = ?redacted_names,
        "PII redacted from streaming SSE chunk"
    );

    let redacted_str = String::from_utf8_lossy(&redacted);
    *content = Value::String(redacted_str.into_owned());
    format!(
        "data: {}",
        serde_json::to_string(&event).unwrap_or_else(|_| json_str.to_string())
    )
}

/// Stream adapter that applies DLP redaction to SSE data lines.
pub struct DlpSseStream {
    inner: Pin<Box<dyn Stream<Item = Result<Bytes, axum::Error>> + Send>>,
    buffer: BytesMut,
    scanner: Arc<DlpScanner>,
    output_buffer: Vec<u8>,
}

impl DlpSseStream {
    pub fn new(body: Body, scanner: Arc<DlpScanner>) -> Self {
        use futures_util::StreamExt;
        use http_body_util::BodyStream;

        let stream = BodyStream::new(body).filter_map(|result| async move {
            match result {
                Ok(frame) => frame.into_data().ok().map(Ok),
                Err(e) => Some(Err(e)),
            }
        });

        Self {
            inner: Box::pin(stream),
            buffer: BytesMut::new(),
            scanner,
            output_buffer: Vec::new(),
        }
    }

    fn process_buffered_lines(&mut self) {
        loop {
            let Some(pos) = self.buffer.iter().position(|&b| b == b'\n') else {
                break;
            };

            let line_bytes = self.buffer.split_to(pos + 1);
            let line = String::from_utf8_lossy(&line_bytes).trim().to_string();

            if line.is_empty() {
                self.output_buffer.extend_from_slice(b"\n");
                continue;
            }

            let redacted = redact_sse_data_line(&line, &self.scanner);
            self.output_buffer.extend_from_slice(redacted.as_bytes());
            self.output_buffer.extend_from_slice(b"\n");
        }
    }
}

impl Stream for DlpSseStream {
    type Item = Result<Bytes, axum::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            if !this.output_buffer.is_empty() {
                let data = std::mem::take(&mut this.output_buffer);
                return Poll::Ready(Some(Ok(Bytes::from(data))));
            }

            match this.inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(chunk))) => {
                    this.buffer.extend_from_slice(&chunk);
                    this.process_buffered_lines();
                }
                Poll::Ready(Some(Err(e))) => return Poll::Ready(Some(Err(e))),
                Poll::Ready(None) => {
                    if !this.buffer.is_empty() {
                        let remaining = std::mem::take(&mut this.buffer);
                        let line = String::from_utf8_lossy(&remaining).trim().to_string();
                        if !line.is_empty() {
                            let redacted = redact_sse_data_line(&line, &this.scanner);
                            return Poll::Ready(Some(Ok(Bytes::from(format!("{redacted}\n")))));
                        }
                    }
                    return Poll::Ready(None);
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// Wrap a Body in a DlpSseStream for streaming DLP redaction.
pub fn wrap_body_with_dlp_sse_stream(body: Body, scanner: Arc<DlpScanner>) -> Body {
    Body::from_stream(DlpSseStream::new(body, scanner))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chat_to_responses_basic() {
        let body = serde_json::json!({
            "model": "gpt-4o-mini",
            "messages": [
                {"role": "user", "content": "say hi"}
            ]
        });
        let result = chat_completions_to_responses(body.to_string().as_bytes()).unwrap();
        let parsed: Value = serde_json::from_slice(&result).unwrap();

        assert_eq!(parsed["model"], "gpt-4o-mini");
        assert_eq!(
            parsed["instructions"], "",
            "instructions should be empty when no system messages"
        );
        let input = parsed["input"].as_array().unwrap();
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "message");
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"], "say hi");
        assert!(parsed.get("messages").is_none());
    }

    #[test]
    fn test_chat_to_responses_with_system() {
        let body = serde_json::json!({
            "model": "gpt-4o",
            "messages": [
                {"role": "system", "content": "You are helpful."},
                {"role": "user", "content": "hello"}
            ]
        });
        let result = chat_completions_to_responses(body.to_string().as_bytes()).unwrap();
        let parsed: Value = serde_json::from_slice(&result).unwrap();

        assert_eq!(parsed["instructions"], "You are helpful.");
        let input = parsed["input"].as_array().unwrap();
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["role"], "user");
    }

    #[test]
    fn test_chat_to_responses_multiple_system() {
        let body = serde_json::json!({
            "model": "gpt-4o",
            "messages": [
                {"role": "system", "content": "Be concise."},
                {"role": "system", "content": "Use markdown."},
                {"role": "user", "content": "hello"}
            ]
        });
        let result = chat_completions_to_responses(body.to_string().as_bytes()).unwrap();
        let parsed: Value = serde_json::from_slice(&result).unwrap();

        assert_eq!(parsed["instructions"], "Be concise.\nUse markdown.");
    }

    #[test]
    fn test_chat_to_responses_max_tokens() {
        let body = serde_json::json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 100
        });
        let result = chat_completions_to_responses(body.to_string().as_bytes()).unwrap();
        let parsed: Value = serde_json::from_slice(&result).unwrap();

        assert_eq!(parsed["max_output_tokens"], 100);
        assert!(parsed.get("max_tokens").is_none());
    }

    #[test]
    fn test_chat_to_responses_strips_unsupported() {
        let body = serde_json::json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi"}],
            "frequency_penalty": 0.5,
            "presence_penalty": 0.5,
            "logprobs": true,
            "top_logprobs": 5,
            "logit_bias": {"123": 1},
            "n": 2,
            "response_format": {"type": "json_object"},
            "seed": 42,
            "service_tier": "default",
            "user": "user-123"
        });
        let result = chat_completions_to_responses(body.to_string().as_bytes()).unwrap();
        let parsed: Value = serde_json::from_slice(&result).unwrap();

        for field in STRIP_FIELDS {
            assert!(
                parsed.get(*field).is_none(),
                "field '{}' should be stripped",
                field
            );
        }
    }

    #[test]
    fn test_chat_to_responses_passthrough() {
        let body = serde_json::json!({
            "model": "gpt-4o-mini",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true,
            "temperature": 0.7,
            "top_p": 0.9,
            "stop": ["\n"]
        });
        let result = chat_completions_to_responses(body.to_string().as_bytes()).unwrap();
        let parsed: Value = serde_json::from_slice(&result).unwrap();

        assert_eq!(parsed["model"], "gpt-4o-mini");
        assert_eq!(parsed["stream"], true);
        assert_eq!(parsed["temperature"], 0.7);
        assert_eq!(parsed["top_p"], 0.9);
        assert_eq!(parsed["stop"], serde_json::json!(["\n"]));
    }

    #[test]
    fn test_responses_to_chat_completion_basic() {
        let body = serde_json::json!({
            "id": "resp_abc123",
            "model": "gpt-4o-mini",
            "status": "completed",
            "output": [{
                "type": "message",
                "content": [{
                    "type": "output_text",
                    "text": "Hello!"
                }]
            }],
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5
            }
        });
        let result = responses_to_chat_completion(body.to_string().as_bytes()).unwrap();
        let parsed: Value = serde_json::from_slice(&result).unwrap();

        assert_eq!(parsed["id"], "resp_abc123");
        assert_eq!(parsed["object"], "chat.completion");
        assert_eq!(parsed["model"], "gpt-4o-mini");
        let choice = &parsed["choices"][0];
        assert_eq!(choice["message"]["role"], "assistant");
        assert_eq!(choice["message"]["content"], "Hello!");
        assert_eq!(choice["finish_reason"], "stop");
    }

    #[test]
    fn test_responses_to_chat_completion_usage() {
        let body = serde_json::json!({
            "id": "resp_abc",
            "model": "gpt-4o",
            "status": "completed",
            "output": [{
                "type": "message",
                "content": [{"type": "output_text", "text": "hi"}]
            }],
            "usage": {
                "input_tokens": 50,
                "output_tokens": 25
            }
        });
        let result = responses_to_chat_completion(body.to_string().as_bytes()).unwrap();
        let parsed: Value = serde_json::from_slice(&result).unwrap();

        assert_eq!(parsed["usage"]["prompt_tokens"], 50);
        assert_eq!(parsed["usage"]["completion_tokens"], 25);
        assert_eq!(parsed["usage"]["total_tokens"], 75);
    }

    #[test]
    fn test_responses_to_chat_completion_incomplete() {
        let body = serde_json::json!({
            "id": "resp_inc",
            "model": "gpt-4o",
            "status": "incomplete",
            "output": [{
                "type": "message",
                "content": [{"type": "output_text", "text": "partial"}]
            }],
            "usage": { "input_tokens": 10, "output_tokens": 5 }
        });
        let result = responses_to_chat_completion(body.to_string().as_bytes()).unwrap();
        let parsed: Value = serde_json::from_slice(&result).unwrap();

        assert_eq!(parsed["choices"][0]["finish_reason"], "length");
    }

    #[test]
    fn test_sse_delta() {
        let event = serde_json::json!({
            "type": "response.output_text.delta",
            "delta": "Hello"
        });
        let line = format!("data: {}", event);
        let mut response_id = Some("resp_123".to_string());
        let mut model = Some("gpt-4o-mini".to_string());
        let result = translate_sse_line(&line, &mut response_id, &mut model).unwrap();

        assert!(result.starts_with("data: "));
        let json_str = result.strip_prefix("data: ").unwrap();
        let parsed: Value = serde_json::from_str(json_str).unwrap();

        assert_eq!(parsed["object"], "chat.completion.chunk");
        assert_eq!(parsed["id"], "resp_123");
        assert_eq!(parsed["model"], "gpt-4o-mini");
        assert_eq!(parsed["choices"][0]["delta"]["content"], "Hello");
        assert!(parsed["choices"][0]["finish_reason"].is_null());
    }

    #[test]
    fn test_sse_completed() {
        let event = serde_json::json!({
            "type": "response.completed",
            "response": {"id": "resp_456", "status": "completed"}
        });
        let line = format!("data: {}", event);
        let mut response_id = Some("resp_456".to_string());
        let mut model = Some("gpt-4o".to_string());
        let result = translate_sse_line(&line, &mut response_id, &mut model).unwrap();

        // Should contain a final chunk with finish_reason: "stop" and then [DONE]
        assert!(result.contains("\"finish_reason\":\"stop\""));
        assert!(result.contains("data: [DONE]"));
    }

    #[test]
    fn test_sse_meta_suppressed() {
        let mut response_id = None;
        let mut model = None;

        let created = serde_json::json!({
            "type": "response.created",
            "response": {"id": "resp_789", "model": "gpt-4o"}
        });
        let result =
            translate_sse_line(&format!("data: {}", created), &mut response_id, &mut model);
        assert!(result.is_none());
        assert_eq!(response_id.as_deref(), Some("resp_789"));
        assert_eq!(model.as_deref(), Some("gpt-4o"));

        let in_progress = serde_json::json!({
            "type": "response.in_progress",
            "response": {"id": "resp_789"}
        });
        let result = translate_sse_line(
            &format!("data: {}", in_progress),
            &mut response_id,
            &mut model,
        );
        assert!(result.is_none());

        // Structural events should also be suppressed
        let content_part = serde_json::json!({"type": "response.content_part.added"});
        let result = translate_sse_line(
            &format!("data: {}", content_part),
            &mut response_id,
            &mut model,
        );
        assert!(result.is_none());
    }

    #[test]
    fn test_sse_done_passthrough() {
        let mut response_id = None;
        let mut model = None;
        let result = translate_sse_line("data: [DONE]", &mut response_id, &mut model);
        assert_eq!(result, Some("data: [DONE]".to_string()));
    }

    #[test]
    fn test_chat_to_responses_multipart_content_types() {
        let body = serde_json::to_vec(&serde_json::json!({
            "model": "gpt-4o",
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "What is in this image?"},
                        {"type": "image_url", "image_url": {"url": "https://example.com/img.png"}}
                    ]
                }
            ]
        }))
        .unwrap();

        let result = chat_completions_to_responses(&body).unwrap();
        let parsed: Value = serde_json::from_slice(&result).unwrap();
        assert_eq!(parsed["input"][0]["type"], "message");
        let content = parsed["input"][0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "input_text");
        assert_eq!(content[0]["text"], "What is in this image?");
        assert_eq!(content[1]["type"], "input_image");
    }

    #[test]
    fn test_chat_to_responses_string_content_unchanged() {
        let body = serde_json::to_vec(&serde_json::json!({
            "model": "gpt-4o",
            "messages": [
                {"role": "user", "content": "hello"}
            ]
        }))
        .unwrap();

        let result = chat_completions_to_responses(&body).unwrap();
        let parsed: Value = serde_json::from_slice(&result).unwrap();
        assert_eq!(parsed["input"][0]["type"], "message");
        assert_eq!(parsed["input"][0]["role"], "user");
        assert_eq!(parsed["input"][0]["content"], "hello");
    }

    // -----------------------------------------------------------------------
    // DLP SSE redaction tests
    // -----------------------------------------------------------------------

    fn test_dlp_scanner() -> DlpScanner {
        use crate::config::{DlpAction, DlpPattern};
        DlpScanner::new(
            &[
                DlpPattern {
                    name: "email".to_string(),
                    regex: r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b".to_string(),
                    action: DlpAction::Redact,
                },
                DlpPattern {
                    name: "ssn".to_string(),
                    regex: r"\b\d{3}-\d{2}-\d{4}\b".to_string(),
                    action: DlpAction::Block,
                },
            ],
            true,
        )
        .unwrap()
    }

    #[test]
    fn test_redact_sse_data_line_with_pii() {
        let scanner = test_dlp_scanner();
        let line = r#"data: {"id":"chatcmpl-1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"Contact user@example.com for info"},"finish_reason":null}]}"#;
        let result = redact_sse_data_line(line, &scanner);
        assert!(
            result.starts_with("data: "),
            "Should still be an SSE data line"
        );
        assert!(
            result.contains("[REDACTED:email]"),
            "Email should be redacted"
        );
        assert!(
            !result.contains("user@example.com"),
            "Original email should be gone"
        );
    }

    #[test]
    fn test_redact_sse_data_line_clean() {
        let scanner = test_dlp_scanner();
        let line = r#"data: {"id":"chatcmpl-1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"Hello world"},"finish_reason":null}]}"#;
        let result = redact_sse_data_line(line, &scanner);
        assert_eq!(result, line, "Clean content should pass through unchanged");
    }

    #[test]
    fn test_redact_sse_data_line_done() {
        let scanner = test_dlp_scanner();
        let result = redact_sse_data_line("data: [DONE]", &scanner);
        assert_eq!(result, "data: [DONE]");
    }

    #[test]
    fn test_redact_sse_data_line_no_delta_content() {
        let scanner = test_dlp_scanner();
        let line = r#"data: {"id":"chatcmpl-1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#;
        let result = redact_sse_data_line(line, &scanner);
        assert_eq!(
            result, line,
            "Lines without delta.content pass through unchanged"
        );
    }

    #[test]
    fn test_redact_sse_data_line_non_data_line() {
        let scanner = test_dlp_scanner();
        let result = redact_sse_data_line("event: message", &scanner);
        assert_eq!(
            result, "event: message",
            "Non-data lines pass through unchanged"
        );
    }
}
