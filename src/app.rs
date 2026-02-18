use crate::config::{Config, Provider};
use crate::dlp::DlpScanner;
use crate::email::{
    EmailAccountCredentials, EmailGetMessageRequest, EmailListMessagesRequest, EmailMessageContent,
    EmailMessageMetadata, EmailPolicy, EmailService, EmailServiceError, ImapEmailService,
    normalize_sender_rule,
};
use crate::keys::{KeyManager, ResolvedKey};
use crate::proxy::ProxyClient;

use axum::Router;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use bytes::Bytes;
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, error, info, trace, warn};

#[derive(Debug, Clone)]
pub struct AppState {
    pub key_manager: Arc<KeyManager>,
    pub dlp_scanner: Arc<DlpScanner>,
    pub proxy_client: Arc<ProxyClient>,
    pub email_enabled: bool,
    pub email_policy: Option<EmailPolicy>,
    pub email_accounts: Arc<BTreeMap<String, EmailAccountCredentials>>,
    pub email_service: Arc<EmailService>,
}

impl AppState {
    pub fn from_config(config: &Config) -> Result<Self, String> {
        let mut upstream_urls = BTreeMap::new();
        upstream_urls.insert(Provider::Openai, config.upstream_url(Provider::Openai));
        upstream_urls.insert(
            Provider::Anthropic,
            config.upstream_url(Provider::Anthropic),
        );

        let key_mappings = config
            .key_map()
            .iter()
            .map(|(virtual_key, (real_key, provider))| {
                (
                    virtual_key.clone(),
                    ResolvedKey {
                        real_key: real_key.clone(),
                        provider: *provider,
                    },
                )
            })
            .collect();

        let email_policy = if config.email.enabled {
            config.email.mode.map(|mode| {
                let sender_rules = match mode {
                    crate::config::EmailMode::Allowlist => &config.email.allow_senders,
                    crate::config::EmailMode::Denylist => &config.email.deny_senders,
                }
                .iter()
                .map(|rule| normalize_sender_rule(rule))
                .collect();

                EmailPolicy {
                    mode,
                    sender_rules,
                    default_max_results: config.email.default_max_results,
                }
            })
        } else {
            None
        };

        let email_accounts: BTreeMap<String, EmailAccountCredentials> = if config.email.enabled {
            config
                .email
                .accounts
                .iter()
                .map(|account| {
                    Ok((
                        account.virtual_key.clone(),
                        EmailAccountCredentials {
                            email: account.email.clone(),
                            app_password: account.app_password.clone(),
                            imap_host: account.imap_host.clone(),
                            imap_port: account.imap_port,
                        },
                    ))
                })
                .collect::<Result<_, String>>()?
        } else {
            BTreeMap::new()
        };

        Ok(Self {
            key_manager: Arc::new(KeyManager::new(key_mappings)),
            dlp_scanner: Arc::new(
                DlpScanner::new(&config.dlp.patterns, config.dlp.scan_responses)
                    .expect("Failed to compile DLP patterns"),
            ),
            proxy_client: Arc::new(ProxyClient::with_upstream_urls(
                upstream_urls,
                config.upstream.anthropic_version.clone(),
            )),
            email_enabled: config.email.enabled,
            email_policy,
            email_accounts: Arc::new(email_accounts),
            email_service: Arc::new(EmailService::Imap(ImapEmailService::default())),
        })
    }
}

/// Maximum request body size (10 MiB).
const MAX_BODY_SIZE: usize = 10 * 1024 * 1024;

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/v1/email/messages", get(handle_email_secure_messages))
        .route("/v1/email/messages/{id}", get(handle_email_message_content))
        .route("/", any(handle_request))
        .route("/{*path}", any(handle_request))
        .layer(DefaultBodyLimit::max(MAX_BODY_SIZE))
        .with_state(state)
}

#[derive(Debug, Deserialize)]
struct EmailSecureMessagesQuery {
    folder: Option<String>,
    limit: Option<u32>,
    unread_only: Option<bool>,
    from: Option<String>,
    subject: Option<String>,
}

#[derive(Debug, Serialize)]
struct EmailSecureMessage {
    id: String,
    thread_id: Option<String>,
    from: String,
    subject: Option<String>,
    date: Option<String>,
    snippet: Option<String>,
    internal_date_ms: Option<i64>,
    labels: Vec<String>,
}

#[derive(Debug, Serialize)]
struct EmailSecureMessagesResponse {
    messages: Vec<EmailSecureMessage>,
    next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct EmailMessageContentPath {
    id: String,
}

#[derive(Debug, Serialize)]
struct EmailMessageContentResponse {
    metadata: EmailMessageMetadata,
    headers: BTreeMap<String, String>,
    text_body: Option<String>,
    html_body: Option<String>,
}

async fn handle_email_secure_messages(
    State(state): State<AppState>,
    Query(query): Query<EmailSecureMessagesQuery>,
    headers: axum::http::HeaderMap,
) -> Result<Response, Response> {
    let method = axum::http::Method::GET;
    let path = "/v1/email/messages";

    if !state.email_enabled {
        warn!(method = %method, path = %path, "Email endpoint is disabled");
        return Err(error_response(
            StatusCode::NOT_FOUND,
            "Email secure endpoint is disabled",
        ));
    }

    let auth_header = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            warn!(method = %method, path = %path, "Missing Authorization header");
            error_response(StatusCode::UNAUTHORIZED, "Missing Authorization header")
        })?;

    let virtual_key = KeyManager::extract_virtual_key(&auth_header)
        .map(|s| s.to_string())
        .ok_or_else(|| {
            warn!(method = %method, path = %path, "Invalid Authorization header format");
            error_response(
                StatusCode::UNAUTHORIZED,
                "Invalid Authorization header format. Expected: Bearer <key>",
            )
        })?;

    let account = state.email_accounts.get(&virtual_key).ok_or_else(|| {
        warn!(
            method = %method,
            path = %path,
            virtual_key = %virtual_key,
            "Virtual key is not authorized for Email"
        );
        error_response(StatusCode::UNAUTHORIZED, "Unknown API key")
    })?;

    let policy = state.email_policy.as_ref().ok_or_else(|| {
        error!(
            method = %method,
            path = %path,
            "Email endpoint enabled without an active sender policy"
        );
        error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Email policy configuration error",
        )
    })?;

    let max_results = query.limit.unwrap_or(policy.default_max_results);
    if max_results == 0 || max_results > 100 {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "limit must be between 1 and 100",
        ));
    }

    let folder = query
        .folder
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("INBOX")
        .to_string();

    let service_request = EmailListMessagesRequest {
        folder,
        max_results,
        unread_only: query.unread_only.unwrap_or(false),
        from_contains: query.from.clone(),
        subject_contains: query.subject.clone(),
    };

    let email_response = state
        .email_service
        .list_message_metadata(&virtual_key, account, &service_request)
        .await
        .map_err(|e| {
            error!(
                method = %method,
                path = %path,
                virtual_key = %virtual_key,
                error = %e,
                "Failed to fetch Email messages"
            );
            error_response(StatusCode::BAD_GATEWAY, "Failed to fetch Email messages")
        })?;

    let mut visible_messages = Vec::new();
    for message in email_response.messages {
        let Some(from_header) = message.from.as_deref() else {
            continue;
        };
        if !policy.sender_visible(from_header) {
            continue;
        }
        visible_messages.push(EmailSecureMessage {
            id: message.id,
            thread_id: message.thread_id,
            from: from_header.to_string(),
            subject: message.subject,
            date: message.date,
            snippet: message.snippet,
            internal_date_ms: message.internal_date_ms,
            labels: message.label_ids,
        });
    }

    let response = EmailSecureMessagesResponse {
        messages: visible_messages,
        next_page_token: email_response.next_page_token,
    };

    Ok(axum::Json(response).into_response())
}

async fn handle_email_message_content(
    State(state): State<AppState>,
    Path(path_params): Path<EmailMessageContentPath>,
    headers: axum::http::HeaderMap,
) -> Result<Response, Response> {
    let method = axum::http::Method::GET;
    let path = "/v1/email/messages/{id}";

    if !state.email_enabled {
        warn!(method = %method, path = %path, "Email endpoint is disabled");
        return Err(error_response(
            StatusCode::NOT_FOUND,
            "Email secure endpoint is disabled",
        ));
    }

    let auth_header = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            warn!(method = %method, path = %path, "Missing Authorization header");
            error_response(StatusCode::UNAUTHORIZED, "Missing Authorization header")
        })?;

    let virtual_key = KeyManager::extract_virtual_key(&auth_header)
        .map(|s| s.to_string())
        .ok_or_else(|| {
            warn!(method = %method, path = %path, "Invalid Authorization header format");
            error_response(
                StatusCode::UNAUTHORIZED,
                "Invalid Authorization header format. Expected: Bearer <key>",
            )
        })?;

    let account = state.email_accounts.get(&virtual_key).ok_or_else(|| {
        warn!(
            method = %method,
            path = %path,
            virtual_key = %virtual_key,
            "Virtual key is not authorized for Email"
        );
        error_response(StatusCode::UNAUTHORIZED, "Unknown API key")
    })?;

    let policy = state.email_policy.as_ref().ok_or_else(|| {
        error!(
            method = %method,
            path = %path,
            "Email endpoint enabled without an active sender policy"
        );
        error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Email policy configuration error",
        )
    })?;

    let message_id = path_params.id.trim();
    if message_id.is_empty() || message_id.parse::<u64>().is_err() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "invalid message id",
        ));
    }

    let service_request = EmailGetMessageRequest {
        folder: "INBOX".to_string(),
        message_id: message_id.to_string(),
    };

    let content: EmailMessageContent = state
        .email_service
        .get_message_content(&virtual_key, account, &service_request)
        .await
        .map_err(|error| match error {
            EmailServiceError::NotFound(_) => {
                error_response(StatusCode::NOT_FOUND, "Email message not found")
            }
            other => {
                error!(
                    method = %method,
                    path = %path,
                    virtual_key = %virtual_key,
                    message_id = %message_id,
                    error = %other,
                    "Failed to fetch Email message content"
                );
                error_response(
                    StatusCode::BAD_GATEWAY,
                    "Failed to fetch Email message content",
                )
            }
        })?;

    let from_header = content
        .metadata
        .from
        .as_deref()
        .or_else(|| content.headers.get("from").map(String::as_str));
    if from_header.is_none_or(|from| !policy.sender_visible(from)) {
        return Err(error_response(
            StatusCode::NOT_FOUND,
            "Email message not found",
        ));
    }

    let response = EmailMessageContentResponse {
        metadata: content.metadata,
        headers: content.headers,
        text_body: content.text_body,
        html_body: content.html_body,
    };

    Ok(axum::Json(response).into_response())
}

async fn handle_request(
    State(state): State<AppState>,
    request: Request,
) -> Result<Response, Response> {
    let start = Instant::now();
    let (parts, body) = request.into_parts();
    let method = parts.method;
    let uri = parts.uri;
    let path = uri.path();
    let headers = parts.headers;

    trace!(
        method = %method,
        path = %path,
        header_count = headers.len(),
        "Incoming request"
    );

    // 1. Extract and validate the virtual key
    let auth_header = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            warn!(method = %method, path = %path, "Missing Authorization header");
            error_response(StatusCode::UNAUTHORIZED, "Missing Authorization header")
        })?;

    let virtual_key = KeyManager::extract_virtual_key(&auth_header)
        .map(|s| s.to_string())
        .ok_or_else(|| {
            warn!(method = %method, path = %path, "Invalid Authorization header format");
            error_response(
                StatusCode::UNAUTHORIZED,
                "Invalid Authorization header format. Expected: Bearer <key>",
            )
        })?;

    let resolved = state.key_manager.resolve(&virtual_key).ok_or_else(|| {
        warn!(
            method = %method,
            path = %path,
            virtual_key = %virtual_key,
            "Unknown virtual key"
        );
        error_response(StatusCode::UNAUTHORIZED, "Unknown API key")
    })?;
    let real_key = resolved.real_key.clone();
    let provider = resolved.provider;

    debug!(
        method = %method,
        path = %path,
        virtual_key = %virtual_key,
        provider = ?provider,
        "Key resolved successfully"
    );

    // 2. Read the body
    let body_bytes: Bytes = body
        .collect()
        .await
        .map_err(|e| {
            error!(error = %e, "Failed to read request body");
            error_response(StatusCode::BAD_REQUEST, "Failed to read request body")
        })?
        .to_bytes();

    trace!(
        method = %method,
        path = %path,
        body_size = body_bytes.len(),
        "Request body read"
    );

    // 3. DLP scan on request body (block patterns reject, redact patterns mask)
    let body_bytes = if !body_bytes.is_empty() {
        let result = state.dlp_scanner.scan_and_redact(&body_bytes);
        if !result.blocked.is_empty() {
            warn!(
                method = %method,
                path = %path,
                virtual_key = %virtual_key,
                detections = ?result.blocked,
                "Sensitive data detected in request"
            );
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                &format!(
                    "Request blocked: sensitive data detected ({})",
                    result.blocked.join(", ")
                ),
            ));
        }
        if result.was_redacted {
            info!(
                method = %method,
                path = %path,
                virtual_key = %virtual_key,
                "PII redacted from request body before forwarding"
            );
            Bytes::from(result.redacted)
        } else {
            body_bytes
        }
    } else {
        body_bytes
    };

    // 4. Forward to upstream
    info!(
        method = %method,
        path = %path,
        virtual_key = %virtual_key,
        "Forwarding request to upstream"
    );

    let response = state
        .proxy_client
        .forward(
            method.clone(),
            &uri,
            headers,
            &real_key,
            body_bytes,
            provider,
        )
        .await
        .map_err(|e| {
            error!(
                method = %method,
                path = %path,
                virtual_key = %virtual_key,
                error = %e,
                "Proxy error"
            );
            e.into_response()
        })?;

    // 5. DLP scan on response body (redact all PII before returning to client)
    let response = if state.dlp_scanner.scan_responses() {
        trace!("Response DLP scanning enabled, checking response body");
        let is_streaming = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.contains("text/event-stream"));

        if !is_streaming {
            trace!("Non-streaming response, scanning body for PII");
            let (parts, body) = response.into_parts();
            let body = body
                .collect()
                .await
                .map_err(|e| {
                    error!(error = %e, "Failed to read response body for DLP scan");
                    error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to process response",
                    )
                })?
                .to_bytes();

            let (redacted, redacted_names) = state.dlp_scanner.redact_all(&body);
            if !redacted_names.is_empty() {
                warn!(
                    method = %method,
                    path = %path,
                    virtual_key = %virtual_key,
                    redacted_patterns = ?redacted_names,
                    "PII redacted from upstream response"
                );
                let redacted_bytes = Bytes::from(redacted);
                let mut parts = parts;
                // Remove stale content-length; axum/hyper will recalculate it
                parts.headers.remove("content-length");
                Response::from_parts(parts, Body::from(redacted_bytes))
            } else {
                Response::from_parts(parts, Body::from(body))
            }
        } else {
            warn!(
                method = %method,
                path = %path,
                virtual_key = %virtual_key,
                "Streaming response (SSE) — DLP scanning is not supported for streaming responses; \
                 PII in streamed content will not be redacted"
            );
            response
        }
    } else {
        trace!("Response DLP scanning disabled");
        response
    };

    let latency = start.elapsed();
    info!(
        method = %method,
        path = %path,
        virtual_key = %virtual_key,
        status = %response.status(),
        latency_ms = latency.as_millis(),
        "Request completed"
    );

    Ok(response)
}

fn error_response(status: StatusCode, message: &str) -> Response {
    let body = serde_json::json!({ "error": message });
    (status, axum::Json(body)).into_response()
}

#[cfg(test)]
mod tests;
