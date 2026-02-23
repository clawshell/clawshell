use super::{OAuthError, OAuthProvider, OAuthTokens};
use async_trait::async_trait;
use axum::http::header::AUTHORIZATION;
use axum::http::HeaderMap;
use chrono::Utc;
use std::collections::BTreeMap;
use tracing::{debug, info, warn};

const DEFAULT_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const DEFAULT_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const DEFAULT_SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/cloud-platform",
    "https://www.googleapis.com/auth/userinfo.email",
    "https://www.googleapis.com/auth/userinfo.profile",
    "https://www.googleapis.com/auth/cclog",
    "https://www.googleapis.com/auth/experimentsandconfigs",
];

const ENDPOINT_PRODUCTION: &str = "https://cloudcode-pa.googleapis.com";
const ENDPOINT_DAILY: &str = "https://daily-cloudcode-pa.sandbox.googleapis.com";
const ENDPOINT_ALT: &str = "https://codeassist.googleapis.com/v1";

#[derive(Debug)]
pub struct AntigravityProvider {
    client_id: String,
    client_secret: String,
    auth_url: String,
    token_url: String,
    scopes: Vec<String>,
    http_client: reqwest::Client,
    endpoints: Vec<String>,
    default_project_id: Option<String>,
}

impl AntigravityProvider {
    pub fn new(
        client_id: Option<&str>,
        auth_url: Option<&str>,
        token_url: Option<&str>,
        scopes: Option<&[String]>,
    ) -> Self {
        Self::new_with_secret(client_id, None, auth_url, token_url, scopes)
    }

    pub fn new_with_secret(
        client_id: Option<&str>,
        client_secret: Option<&str>,
        auth_url: Option<&str>,
        token_url: Option<&str>,
        scopes: Option<&[String]>,
    ) -> Self {
        // Load .env file if present (ignored if missing)
        let _ = dotenvy::dotenv();

        // Env vars take priority, then explicit constructor arguments.
        // No hardcoded defaults — credentials must come from env, .env file, or config.
        let resolved_client_id = std::env::var("GOOGLE_OAUTH_CLIENT_ID")
            .ok()
            .or_else(|| client_id.map(String::from))
            .expect("GOOGLE_OAUTH_CLIENT_ID env var or client_id argument is required");
        let resolved_client_secret = std::env::var("GOOGLE_OAUTH_CLIENT_SECRET")
            .ok()
            .or_else(|| client_secret.map(String::from))
            .expect("GOOGLE_OAUTH_CLIENT_SECRET env var or client_secret argument is required");
        let default_project_id = std::env::var("ANTIGRAVITY_DEFAULT_PROJECT_ID").ok();

        Self {
            client_id: resolved_client_id,
            client_secret: resolved_client_secret,
            auth_url: auth_url.unwrap_or(DEFAULT_AUTH_URL).to_string(),
            token_url: token_url.unwrap_or(DEFAULT_TOKEN_URL).to_string(),
            scopes: scopes
                .map(|s| s.to_vec())
                .unwrap_or_else(|| DEFAULT_SCOPES.iter().map(|s| s.to_string()).collect()),
            http_client: reqwest::Client::builder()
                .user_agent(format!(
                    "ClawShell/{} (https://github.com/nicholasgasior/clawshell)",
                    env!("CARGO_PKG_VERSION")
                ))
                .build()
                .expect("failed to build HTTP client"),
            endpoints: vec![
                ENDPOINT_PRODUCTION.to_string(),
                ENDPOINT_DAILY.to_string(),
                ENDPOINT_ALT.to_string(),
            ],
            default_project_id,
        }
    }

    pub fn from_config(config: &super::OAuthProviderConfig) -> Self {
        Self::new(
            config.client_id.as_deref(),
            config.auth_url.as_deref(),
            config.token_url.as_deref(),
            config.scopes.as_deref(),
        )
    }

    async fn exchange_code(
        &self,
        code: &str,
        code_verifier: &str,
        redirect_uri: &str,
    ) -> Result<OAuthTokens, OAuthError> {
        let params = [
            ("grant_type", "authorization_code"),
            ("client_id", self.client_id.as_str()),
            ("client_secret", self.client_secret.as_str()),
            ("code", code),
            ("code_verifier", code_verifier),
            ("redirect_uri", redirect_uri),
        ];

        let resp = self
            .http_client
            .post(&self.token_url)
            .form(&params)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(OAuthError::LoginFailed(format!(
                "token exchange failed ({status}): {body}"
            )));
        }

        let json: serde_json::Value = resp.json().await?;
        let mut tokens = parse_google_token_response(&json)?;

        // Discover project ID after successful login
        if let Err(e) = self.discover_project_id(&mut tokens).await {
            warn!(error = %e, "Failed to discover Antigravity project ID");
        }

        Ok(tokens)
    }

    async fn exchange_refresh_token(
        &self,
        refresh_token: &str,
    ) -> Result<OAuthTokens, OAuthError> {
        let params = [
            ("grant_type", "refresh_token"),
            ("client_id", self.client_id.as_str()),
            ("client_secret", self.client_secret.as_str()),
            ("refresh_token", refresh_token),
        ];

        let resp = self
            .http_client
            .post(&self.token_url)
            .form(&params)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(OAuthError::RefreshFailed(format!(
                "refresh failed ({status}): {body}"
            )));
        }

        let json: serde_json::Value = resp.json().await?;
        // Google refresh responses may not include a new refresh token;
        // the caller should preserve the original refresh token.
        let mut tokens = parse_google_token_response(&json)?;
        if tokens.refresh_token.is_none() {
            tokens.refresh_token = Some(refresh_token.to_string());
        }

        // Re-discover project ID with the fresh access token.
        // This also recovers from initial login discovery failures.
        if let Err(e) = self.discover_project_id(&mut tokens).await {
            warn!(error = %e, "Failed to discover Antigravity project ID during refresh");
        }

        Ok(tokens)
    }

    async fn discover_project_id(&self, tokens: &mut OAuthTokens) -> Result<(), OAuthError> {
        let url = format!("{}/v1internal:loadCodeAssist", self.endpoints[0]);

        let resp: reqwest::Response = self
            .http_client
            .post(&url)
            .header("Authorization", format!("Bearer {}", tokens.access_token))
            .header(
                "x-goog-api-client",
                "google-cloud-sdk vscode_cloudshelleditor/0.1",
            )
            .header(
                "Client-Metadata",
                r#"{"ideType":"IDE_UNSPECIFIED","platform":"PLATFORM_UNSPECIFIED","pluginType":"GEMINI"}"#,
            )
            .json(&serde_json::json!({
                "metadata": {
                    "ideType": "IDE_UNSPECIFIED",
                    "platform": "PLATFORM_UNSPECIFIED",
                    "pluginType": "GEMINI"
                }
            }))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            warn!(
                %status,
                "loadCodeAssist failed, attempting fallback to default project ID"
            );
            debug!(response_body = %body, "loadCodeAssist error response");
            if let Some(ref default_id) = self.default_project_id {
                tokens.extra.insert(
                    "project_id".to_string(),
                    serde_json::json!(default_id),
                );
                info!(project_id = %default_id, "Using default Antigravity project ID from env");
                return Ok(());
            }
            return Err(OAuthError::LoginFailed(format!(
                "loadCodeAssist failed ({status}) and no ANTIGRAVITY_DEFAULT_PROJECT_ID set"
            )));
        }

        let json: serde_json::Value = resp.json().await?;

        // Extract project ID — openclaw uses cloudaicompanionProject (string or {id: ...})
        let project_id = json
            .get("cloudaicompanionProject")
            .and_then(|v| {
                v.as_str().map(String::from).or_else(|| {
                    v.get("id").and_then(|id| id.as_str()).map(String::from)
                })
            })
            .or_else(|| {
                json.get("projectId")
                    .and_then(|v| v.as_str())
                    .map(String::from)
            });

        if let Some(pid) = project_id {
            debug!(project_id = %pid, "Discovered Antigravity project ID");
            tokens
                .extra
                .insert("project_id".to_string(), serde_json::json!(pid));
        } else if let Some(ref default_id) = self.default_project_id {
            tokens.extra.insert(
                "project_id".to_string(),
                serde_json::json!(default_id),
            );
            info!(project_id = %default_id, "API returned no project ID, using default from env");
        }
        if let Some(tier) = json.get("tier").and_then(|v| v.as_str()) {
            tokens
                .extra
                .insert("tier".to_string(), serde_json::json!(tier));
        }

        Ok(())
    }
}

fn parse_google_token_response(json: &serde_json::Value) -> Result<OAuthTokens, OAuthError> {
    let access_token = json
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| OAuthError::LoginFailed("missing access_token in response".to_string()))?
        .to_string();

    let refresh_token = json
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(String::from);

    let id_token = json
        .get("id_token")
        .and_then(|v| v.as_str())
        .map(String::from);

    let expires_at = json
        .get("expires_in")
        .and_then(|v| v.as_i64())
        .map(|secs| Utc::now() + chrono::Duration::seconds(secs));

    Ok(OAuthTokens {
        access_token,
        refresh_token,
        id_token,
        expires_at,
        account_id: None,
        extra: BTreeMap::new(),
    })
}

fn generate_pkce() -> (String, String) {
    use base64::Engine;
    use sha2::{Digest, Sha256};

    let verifier_bytes: [u8; 32] = rand::random();
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(verifier_bytes);

    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize());

    (verifier, challenge)
}

/// Wrap an OpenAI-format request body into Antigravity/Gemini-style format.
///
/// Translates OpenAI chat/completions fields to Gemini generateContent fields:
/// - `messages` → `contents` + `systemInstruction`
/// - `max_tokens`/`max_completion_tokens` → `generationConfig.maxOutputTokens`
/// - `temperature`, `top_p`, `stop` → `generationConfig`
/// - `tools` (OpenAI function-calling) → Gemini `tools[].functionDeclarations`
/// - Strips OpenAI-only fields (`stream`, `stream_options`, `store`, etc.)
pub fn wrap_antigravity_request(
    body: &[u8],
    project_id: &str,
) -> Result<Vec<u8>, OAuthError> {
    let original: serde_json::Value = serde_json::from_slice(body).map_err(|e| {
        OAuthError::LoginFailed(format!("failed to parse request body as JSON: {e}"))
    })?;

    let obj = original.as_object().ok_or_else(|| {
        OAuthError::LoginFailed("request body is not a JSON object".to_string())
    })?;

    let raw_model = obj
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("gemini-2.0-flash");
    // Strip provider prefix (e.g. "google/gemini-2.5-flash" → "gemini-2.5-flash")
    let model = raw_model
        .split_once('/')
        .map(|(_, id)| id)
        .unwrap_or(raw_model);

    let mut request = serde_json::Map::new();

    // Translate messages → contents + systemInstruction
    if let Some(messages) = obj.get("messages").and_then(|v| v.as_array()) {
        let mut system_parts: Vec<serde_json::Value> = Vec::new();
        let mut contents: Vec<serde_json::Value> = Vec::new();

        for msg in messages {
            let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
            let parts = message_content_to_gemini_parts(msg);

            match role {
                "system" => {
                    system_parts.extend(parts);
                }
                "assistant" => {
                    contents.push(serde_json::json!({
                        "role": "model",
                        "parts": parts,
                    }));
                }
                _ => {
                    // "user", "tool", and anything else → keep role as-is
                    contents.push(serde_json::json!({
                        "role": role,
                        "parts": parts,
                    }));
                }
            }
        }

        if !system_parts.is_empty() {
            request.insert(
                "systemInstruction".to_string(),
                serde_json::json!({ "parts": system_parts }),
            );
        }
        request.insert("contents".to_string(), serde_json::json!(contents));
    }

    // Translate generation parameters → generationConfig
    let mut gen_config = serde_json::Map::new();
    if let Some(v) = obj.get("max_tokens").or(obj.get("max_completion_tokens")) {
        gen_config.insert("maxOutputTokens".to_string(), v.clone());
    }
    if let Some(v) = obj.get("temperature") {
        gen_config.insert("temperature".to_string(), v.clone());
    }
    if let Some(v) = obj.get("top_p") {
        gen_config.insert("topP".to_string(), v.clone());
    }
    if let Some(v) = obj.get("stop") {
        // OpenAI: stop can be string or array; Gemini: stopSequences is always array
        let sequences = if v.is_string() {
            serde_json::json!([v])
        } else {
            v.clone()
        };
        gen_config.insert("stopSequences".to_string(), sequences);
    }
    if !gen_config.is_empty() {
        request.insert(
            "generationConfig".to_string(),
            serde_json::Value::Object(gen_config),
        );
    }

    // Translate tools (OpenAI function-calling → Gemini functionDeclarations)
    if let Some(tools) = obj.get("tools").and_then(|v| v.as_array()) {
        let mut func_decls: Vec<serde_json::Value> = Vec::new();
        for tool in tools {
            if tool.get("type").and_then(|v| v.as_str()) == Some("function") {
                if let Some(func) = tool.get("function") {
                    let mut decl = serde_json::Map::new();
                    if let Some(name) = func.get("name") {
                        decl.insert("name".to_string(), name.clone());
                    }
                    if let Some(desc) = func.get("description") {
                        decl.insert("description".to_string(), desc.clone());
                    }
                    if let Some(params) = func.get("parameters") {
                        decl.insert(
                            "parameters".to_string(),
                            sanitize_schema_for_gemini(params.clone()),
                        );
                    }
                    func_decls.push(serde_json::Value::Object(decl));
                }
            }
        }
        if !func_decls.is_empty() {
            request.insert(
                "tools".to_string(),
                serde_json::json!([{ "functionDeclarations": func_decls }]),
            );
        }
    }

    debug!(
        raw_model = raw_model,
        model = model,
        project = project_id,
        "Antigravity request: wrapping for upstream"
    );

    let wrapped = serde_json::json!({
        "project": project_id,
        "model": model,
        "user_prompt_id": uuid::Uuid::new_v4().to_string(),
        "request": request,
    });

    serde_json::to_vec(&wrapped)
        .map_err(|e| OAuthError::LoginFailed(format!("failed to serialize wrapped body: {e}")))
}

/// Convert an OpenAI message's `content` field to Gemini `parts` array.
fn message_content_to_gemini_parts(msg: &serde_json::Value) -> Vec<serde_json::Value> {
    let Some(content) = msg.get("content") else {
        return vec![];
    };

    // String content → single text part
    if let Some(text) = content.as_str() {
        return vec![serde_json::json!({ "text": text })];
    }

    // Array content (multimodal) → convert each part
    if let Some(parts) = content.as_array() {
        return parts
            .iter()
            .filter_map(|part| {
                match part.get("type").and_then(|v| v.as_str()) {
                    Some("text") => {
                        let text = part.get("text").and_then(|v| v.as_str()).unwrap_or("");
                        Some(serde_json::json!({ "text": text }))
                    }
                    Some("image_url") => {
                        // Convert OpenAI image_url to Gemini inlineData or fileData
                        let url = part
                            .get("image_url")
                            .and_then(|v| v.get("url"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if let Some(rest) = url.strip_prefix("data:") {
                            // data URI → inlineData
                            if let Some((mime, data)) = rest.split_once(";base64,") {
                                return Some(serde_json::json!({
                                    "inlineData": {
                                        "mimeType": mime,
                                        "data": data,
                                    }
                                }));
                            }
                        }
                        // URL → fileData
                        Some(serde_json::json!({
                            "fileData": {
                                "fileUri": url,
                            }
                        }))
                    }
                    _ => None,
                }
            })
            .collect();
    }

    vec![]
}

/// JSON Schema keywords that the Cloud Code Assist API rejects.
/// Matching openclaw's `GEMINI_UNSUPPORTED_SCHEMA_KEYWORDS`.
const GEMINI_SCHEMA_REJECTED: &[&str] = &[
    "patternProperties",
    "additionalProperties",
    "$schema",
    "$id",
    "$ref",
    "$defs",
    "definitions",
    "examples",
    "minLength",
    "maxLength",
    "minimum",
    "maximum",
    "multipleOf",
    "pattern",
    "format",
    "minItems",
    "maxItems",
    "uniqueItems",
    "minProperties",
    "maxProperties",
];

/// Recursively strip JSON Schema fields that the Gemini API does not support.
fn sanitize_schema_for_gemini(mut value: serde_json::Value) -> serde_json::Value {
    let Some(obj) = value.as_object_mut() else {
        return value;
    };

    obj.retain(|key, _| !GEMINI_SCHEMA_REJECTED.contains(&key.as_str()));

    // Recurse into nested schemas
    if let Some(props) = obj.get_mut("properties") {
        if let Some(map) = props.as_object_mut() {
            for v in map.values_mut() {
                *v = sanitize_schema_for_gemini(v.clone());
            }
        }
    }
    if let Some(items) = obj.get_mut("items") {
        *items = sanitize_schema_for_gemini(items.clone());
    }

    value
}

#[async_trait]
impl OAuthProvider for AntigravityProvider {
    fn id(&self) -> &str {
        "antigravity"
    }

    fn display_name(&self) -> &str {
        "Antigravity / Google (OAuth)"
    }

    fn supports_headless_url(&self) -> bool {
        true
    }

    async fn login_browser(&self, _callback_port: u16) -> Result<OAuthTokens, OAuthError> {
        // This client ID requires a fixed redirect URI registered in Google's OAuth console.
        const ANTIGRAVITY_CALLBACK_PORT: u16 = 51121;
        let (verifier, challenge) = generate_pkce();
        let redirect_uri = format!("http://localhost:{ANTIGRAVITY_CALLBACK_PORT}/oauth-callback");
        let state: String = uuid::Uuid::new_v4().to_string();

        let auth_url = format!(
            "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256&state={}&access_type=offline&prompt=consent",
            self.auth_url,
            urlencoding::encode(&self.client_id),
            urlencoding::encode(&redirect_uri),
            urlencoding::encode(&self.scopes.join(" ")),
            urlencoding::encode(&challenge),
            urlencoding::encode(&state),
        );

        info!("Opening browser for Antigravity/Google OAuth login");
        if let Err(e) = open::that(&auth_url) {
            return Err(OAuthError::LoginFailed(format!(
                "failed to open browser: {e}. Visit this URL manually:\n{auth_url}"
            )));
        }

        let (code, received_state) =
            wait_for_oauth_callback(ANTIGRAVITY_CALLBACK_PORT).await.map_err(|e| {
                OAuthError::LoginFailed(format!("callback server failed: {e}"))
            })?;

        if received_state != state {
            return Err(OAuthError::LoginFailed(
                "OAuth state mismatch — possible CSRF".to_string(),
            ));
        }

        self.exchange_code(&code, &verifier, &redirect_uri).await
    }

    async fn login_headless(&self) -> Result<OAuthTokens, OAuthError> {
        const ANTIGRAVITY_CALLBACK_PORT: u16 = 51121;
        let (verifier, challenge) = generate_pkce();
        let redirect_uri = format!("http://localhost:{ANTIGRAVITY_CALLBACK_PORT}/oauth-callback");
        let state: String = uuid::Uuid::new_v4().to_string();

        let auth_url = format!(
            "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256&state={}&access_type=offline&prompt=consent",
            self.auth_url,
            urlencoding::encode(&self.client_id),
            urlencoding::encode(&redirect_uri),
            urlencoding::encode(&self.scopes.join(" ")),
            urlencoding::encode(&challenge),
            urlencoding::encode(&state),
        );

        println!();
        println!("  Visit this URL to authenticate:");
        println!("  {auth_url}");
        println!();

        // Start a local HTTP server to receive the OAuth callback, then wait.
        let (code, received_state) =
            wait_for_oauth_callback(ANTIGRAVITY_CALLBACK_PORT).await.map_err(|e| {
                OAuthError::LoginFailed(format!("callback server failed: {e}"))
            })?;

        if received_state != state {
            return Err(OAuthError::LoginFailed(
                "OAuth state mismatch — possible CSRF".to_string(),
            ));
        }

        self.exchange_code(&code, &verifier, &redirect_uri).await
    }

    async fn refresh(&self, refresh_token: &str) -> Result<OAuthTokens, OAuthError> {
        self.exchange_refresh_token(refresh_token).await
    }

    fn inject_auth(
        &self,
        headers: &mut HeaderMap,
        access_token: &str,
    ) -> Result<(), OAuthError> {
        headers.insert(
            AUTHORIZATION,
            format!("Bearer {access_token}").parse()?,
        );
        headers.insert(
            "x-goog-api-client",
            "google-cloud-sdk vscode_cloudshelleditor/0.1".parse()?,
        );
        headers.insert(
            "client-metadata",
            r#"{"ideType":"ANTIGRAVITY","platform":"LINUX","pluginType":"GEMINI"}"#.parse()?,
        );
        Ok(())
    }

    async fn enrich_tokens(&self, tokens: &OAuthTokens) -> Result<Option<OAuthTokens>, OAuthError> {
        if tokens.extra.contains_key("project_id") {
            return Ok(None);
        }
        info!("Antigravity tokens missing project_id, discovering on-demand");
        let mut enriched = tokens.clone();
        self.discover_project_id(&mut enriched).await?;
        Ok(Some(enriched))
    }

    fn prepare_request_body(
        &self,
        body: &[u8],
        tokens: &OAuthTokens,
    ) -> Result<Option<Vec<u8>>, OAuthError> {
        let project_id = tokens
            .extra
            .get("project_id")
            .and_then(|v| v.as_str())
            .ok_or(OAuthError::LoginFailed(
                "missing project_id for Antigravity provider".to_string(),
            ))?;
        let wrapped = wrap_antigravity_request(body, project_id)?;
        Ok(Some(wrapped))
    }

    fn upstream_url(&self, _tokens: &OAuthTokens) -> Option<String> {
        Some(self.endpoints[0].clone())
    }

    fn rewrite_request_path(&self, _path: &str) -> Option<String> {
        Some("/v1internal:streamGenerateContent?alt=sse".to_string())
    }

    fn response_format(&self, _original_path: &str) -> Option<super::ResponseFormat> {
        Some(super::ResponseFormat::GeminiSse)
    }
}

/// Parse code and state from a pasted redirect URL (headless flow).
fn parse_redirect_url(input: &str) -> Result<(String, String), OAuthError> {
    let query = input
        .split('?')
        .nth(1)
        .ok_or_else(|| OAuthError::LoginFailed("no query string in redirect URL".to_string()))?;

    let mut code = String::new();
    let mut state = String::new();

    for param in query.split('&') {
        if let Some((key, value)) = param.split_once('=') {
            match key {
                "code" => code = urlencoding::decode(value).unwrap_or_default().to_string(),
                "state" => state = urlencoding::decode(value).unwrap_or_default().to_string(),
                _ => {}
            }
        }
    }

    if code.is_empty() {
        return Err(OAuthError::LoginFailed(
            "no authorization code found in redirect URL".to_string(),
        ));
    }

    Ok((code, state))
}

/// Wait for an OAuth callback on a local HTTP server (same as codex).
async fn wait_for_oauth_callback(
    port: u16,
) -> Result<(String, String), Box<dyn std::error::Error + Send + Sync>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}")).await?;
    let (mut stream, _) = listener.accept().await?;

    let mut buf = vec![0u8; 4096];
    let n = stream.read(&mut buf).await?;
    let request = String::from_utf8_lossy(&buf[..n]);

    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("");

    let query = path.split('?').nth(1).unwrap_or("");
    let mut code = String::new();
    let mut state = String::new();

    for param in query.split('&') {
        if let Some((key, value)) = param.split_once('=') {
            match key {
                "code" => code = urlencoding::decode(value).unwrap_or_default().to_string(),
                "state" => state = urlencoding::decode(value).unwrap_or_default().to_string(),
                _ => {}
            }
        }
    }

    let response = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n\
        <html><body><h1>Login successful!</h1><p>You can close this tab.</p></body></html>";
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await?;

    if code.is_empty() {
        return Err("no authorization code in callback".into());
    }

    Ok((code, state))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_provider() -> AntigravityProvider {
        AntigravityProvider::new_with_secret(
            Some("test-client-id"),
            Some("test-client-secret"),
            None,
            None,
            None,
        )
    }

    #[test]
    fn test_parse_google_token_response() {
        let json = serde_json::json!({
            "access_token": "ya29.test",
            "refresh_token": "1//test",
            "expires_in": 3600,
            "scope": "openid email profile",
            "token_type": "Bearer"
        });

        let tokens = parse_google_token_response(&json).unwrap();
        assert_eq!(tokens.access_token, "ya29.test");
        assert_eq!(tokens.refresh_token.as_deref(), Some("1//test"));
        assert!(tokens.expires_at.is_some());
    }

    #[test]
    fn test_parse_google_token_response_no_refresh() {
        let json = serde_json::json!({
            "access_token": "ya29.refreshed",
            "expires_in": 3600
        });

        let tokens = parse_google_token_response(&json).unwrap();
        assert_eq!(tokens.access_token, "ya29.refreshed");
        assert!(tokens.refresh_token.is_none());
    }

    #[test]
    fn test_wrap_antigravity_request() {
        let body = serde_json::json!({
            "model": "gemini-3-pro",
            "messages": [{"role": "user", "content": "hello"}]
        });
        let body_bytes = serde_json::to_vec(&body).unwrap();

        let wrapped = wrap_antigravity_request(&body_bytes, "proj-abc-123").unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&wrapped).unwrap();

        assert_eq!(parsed["project"], "proj-abc-123");
        assert_eq!(parsed["model"], "gemini-3-pro");

        // messages should be translated to Gemini contents format
        let contents = parsed["request"]["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[0]["parts"][0]["text"], "hello");

        // Raw OpenAI fields should not be present
        assert!(parsed["request"].get("messages").is_none());
    }

    #[test]
    fn test_wrap_antigravity_request_default_model() {
        let body = serde_json::json!({
            "messages": [{"role": "user", "content": "test"}]
        });
        let body_bytes = serde_json::to_vec(&body).unwrap();

        let wrapped = wrap_antigravity_request(&body_bytes, "proj-123").unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&wrapped).unwrap();

        assert_eq!(parsed["model"], "gemini-2.0-flash");
    }

    #[test]
    fn test_wrap_antigravity_system_message() {
        let body = serde_json::json!({
            "model": "gemini-2.0-flash",
            "messages": [
                {"role": "system", "content": "You are helpful."},
                {"role": "user", "content": "hi"}
            ]
        });
        let body_bytes = serde_json::to_vec(&body).unwrap();

        let wrapped = wrap_antigravity_request(&body_bytes, "proj-123").unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&wrapped).unwrap();

        // System messages become systemInstruction
        assert_eq!(
            parsed["request"]["systemInstruction"]["parts"][0]["text"],
            "You are helpful."
        );
        // Only non-system messages in contents
        let contents = parsed["request"]["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "user");
    }

    #[test]
    fn test_wrap_antigravity_assistant_role() {
        let body = serde_json::json!({
            "model": "gemini-2.0-flash",
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "hello"}
            ]
        });
        let body_bytes = serde_json::to_vec(&body).unwrap();

        let wrapped = wrap_antigravity_request(&body_bytes, "proj-123").unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&wrapped).unwrap();

        let contents = parsed["request"]["contents"].as_array().unwrap();
        assert_eq!(contents[1]["role"], "model");
        assert_eq!(contents[1]["parts"][0]["text"], "hello");
    }

    #[test]
    fn test_wrap_antigravity_generation_config() {
        let body = serde_json::json!({
            "model": "gemini-2.0-flash",
            "messages": [{"role": "user", "content": "hi"}],
            "max_completion_tokens": 1024,
            "temperature": 0.7,
            "top_p": 0.9,
            "stop": ["\n"]
        });
        let body_bytes = serde_json::to_vec(&body).unwrap();

        let wrapped = wrap_antigravity_request(&body_bytes, "proj-123").unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&wrapped).unwrap();

        let gc = &parsed["request"]["generationConfig"];
        assert_eq!(gc["maxOutputTokens"], 1024);
        assert_eq!(gc["temperature"], 0.7);
        assert_eq!(gc["topP"], 0.9);
        assert_eq!(gc["stopSequences"], serde_json::json!(["\n"]));
    }

    #[test]
    fn test_wrap_antigravity_tools() {
        let body = serde_json::json!({
            "model": "gemini-2.0-flash",
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [
                {
                    "type": "function",
                    "function": {
                        "name": "get_weather",
                        "description": "Get weather",
                        "parameters": {"type": "object", "properties": {}}
                    }
                }
            ]
        });
        let body_bytes = serde_json::to_vec(&body).unwrap();

        let wrapped = wrap_antigravity_request(&body_bytes, "proj-123").unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&wrapped).unwrap();

        let decls = &parsed["request"]["tools"][0]["functionDeclarations"];
        assert_eq!(decls[0]["name"], "get_weather");
        assert_eq!(decls[0]["description"], "Get weather");
    }

    #[test]
    fn test_wrap_antigravity_strips_openai_fields() {
        let body = serde_json::json!({
            "model": "gemini-2.0-flash",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true,
            "stream_options": {"include_usage": true},
            "store": false
        });
        let body_bytes = serde_json::to_vec(&body).unwrap();

        let wrapped = wrap_antigravity_request(&body_bytes, "proj-123").unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&wrapped).unwrap();

        assert!(parsed["request"].get("stream").is_none());
        assert!(parsed["request"].get("stream_options").is_none());
        assert!(parsed["request"].get("store").is_none());
        assert!(parsed["request"].get("messages").is_none());
    }

    #[test]
    fn test_sanitize_schema_strips_unsupported_fields() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The name",
                    "patternProperties": {"^x-": {"type": "string"}},
                    "additionalProperties": false,
                    "minLength": 1,
                    "maxLength": 100
                },
                "tags": {
                    "type": "array",
                    "items": {
                        "type": "string",
                        "$ref": "#/defs/Tag",
                        "default": "foo"
                    }
                }
            },
            "required": ["name"],
            "$schema": "http://json-schema.org/draft-07/schema#",
            "additionalProperties": false,
            "$defs": {}
        });

        let sanitized = sanitize_schema_for_gemini(schema);

        // Top-level rejected fields stripped
        assert!(sanitized.get("$schema").is_none());
        assert!(sanitized.get("additionalProperties").is_none());
        assert!(sanitized.get("$defs").is_none());
        // Supported fields preserved
        assert_eq!(sanitized["type"], "object");
        assert_eq!(sanitized["required"], serde_json::json!(["name"]));

        // Nested property rejected fields stripped
        let name_prop = &sanitized["properties"]["name"];
        assert!(name_prop.get("patternProperties").is_none());
        assert!(name_prop.get("additionalProperties").is_none());
        assert!(name_prop.get("minLength").is_none());
        assert!(name_prop.get("maxLength").is_none());
        assert_eq!(name_prop["type"], "string");
        assert_eq!(name_prop["description"], "The name");

        // Items rejected fields stripped, but non-rejected fields preserved
        let items = &sanitized["properties"]["tags"]["items"];
        assert!(items.get("$ref").is_none());
        assert_eq!(items["type"], "string");
        // "default" is NOT in the rejected list, so it's preserved
        assert_eq!(items["default"], "foo");
    }

    #[test]
    fn test_antigravity_provider_defaults() {
        let provider = test_provider();
        assert_eq!(provider.id(), "antigravity");
        assert_eq!(provider.display_name(), "Antigravity / Google (OAuth)");
        assert!(provider.supports_headless_url());
        assert!(!provider.supports_device_code());
    }

    #[test]
    fn test_inject_auth_headers() {
        let provider = test_provider();
        let mut headers = HeaderMap::new();
        provider.inject_auth(&mut headers, "ya29.test").unwrap();

        assert_eq!(
            headers.get("authorization").unwrap().to_str().unwrap(),
            "Bearer ya29.test"
        );
        assert!(headers.get("x-goog-api-client").is_some());
        assert!(headers.get("client-metadata").is_some());
    }

    #[test]
    fn test_prepare_request_body_with_project_id() {
        let provider = test_provider();
        let mut tokens = OAuthTokens {
            access_token: "t".to_string(),
            refresh_token: None,
            id_token: None,
            expires_at: None,
            account_id: None,
            extra: BTreeMap::new(),
        };
        tokens.extra.insert(
            "project_id".to_string(),
            serde_json::json!("proj-test"),
        );

        let body = serde_json::json!({"model": "gemini-3-pro", "messages": []});
        let result = provider
            .prepare_request_body(&serde_json::to_vec(&body).unwrap(), &tokens)
            .unwrap();
        assert!(result.is_some());

        let parsed: serde_json::Value = serde_json::from_slice(&result.unwrap()).unwrap();
        assert_eq!(parsed["project"], "proj-test");
    }

    #[test]
    fn test_prepare_request_body_missing_project_id() {
        let provider = test_provider();
        let tokens = OAuthTokens {
            access_token: "t".to_string(),
            refresh_token: None,
            id_token: None,
            expires_at: None,
            account_id: None,
            extra: BTreeMap::new(),
        };

        let body = serde_json::json!({"model": "gemini-3-pro"});
        let result = provider.prepare_request_body(&serde_json::to_vec(&body).unwrap(), &tokens);
        assert!(result.is_err());
    }

    #[test]
    fn test_upstream_url() {
        let provider = test_provider();
        let tokens = OAuthTokens {
            access_token: "t".to_string(),
            refresh_token: None,
            id_token: None,
            expires_at: None,
            account_id: None,
            extra: BTreeMap::new(),
        };

        let url = provider.upstream_url(&tokens).unwrap();
        assert!(url.contains("cloudcode-pa.googleapis.com"));
        assert!(!url.contains("streamGenerateContent"), "upstream_url should be base only");
    }

    #[test]
    fn test_rewrite_request_path() {
        let provider = test_provider();
        let rewritten = provider.rewrite_request_path("/v1/chat/completions");
        assert_eq!(
            rewritten.as_deref(),
            Some("/v1internal:streamGenerateContent?alt=sse")
        );
    }
}
