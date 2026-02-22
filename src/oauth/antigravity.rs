use super::{OAuthError, OAuthProvider, OAuthTokens};
use async_trait::async_trait;
use axum::http::header::AUTHORIZATION;
use axum::http::HeaderMap;
use chrono::Utc;
use std::collections::BTreeMap;
use tracing::{debug, info, warn};

const DEFAULT_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/auth";
const DEFAULT_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const DEFAULT_SCOPES: &[&str] = &[
    "openid",
    "https://www.googleapis.com/auth/userinfo.email",
    "https://www.googleapis.com/auth/userinfo.profile",
    "https://www.googleapis.com/auth/cloud-platform",
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
            .json(&serde_json::json!({}))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(OAuthError::LoginFailed(format!(
                "loadCodeAssist failed ({status}): {body}"
            )));
        }

        let json: serde_json::Value = resp.json().await?;
        if let Some(project_id) = json.get("projectId").and_then(|v| v.as_str()) {
            tokens
                .extra
                .insert("project_id".to_string(), serde_json::json!(project_id));
            debug!(project_id, "Discovered Antigravity project ID");
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
pub fn wrap_antigravity_request(
    body: &[u8],
    project_id: &str,
) -> Result<Vec<u8>, OAuthError> {
    let original: serde_json::Value = serde_json::from_slice(body).map_err(|e| {
        OAuthError::LoginFailed(format!("failed to parse request body as JSON: {e}"))
    })?;

    let model = original
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("gemini-2.0-flash");

    let wrapped = serde_json::json!({
        "project": project_id,
        "model": model,
        "request": original,
    });

    serde_json::to_vec(&wrapped)
        .map_err(|e| OAuthError::LoginFailed(format!("failed to serialize wrapped body: {e}")))
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

    async fn login_browser(&self, callback_port: u16) -> Result<OAuthTokens, OAuthError> {
        let (verifier, challenge) = generate_pkce();
        let redirect_uri = format!("http://localhost:{callback_port}/oauth-callback");
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
            wait_for_oauth_callback(callback_port).await.map_err(|e| {
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
        let (verifier, challenge) = generate_pkce();
        let redirect_uri = "https://codeassist.google.com/authcode".to_string();

        let auth_url = format!(
            "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256&access_type=offline&prompt=consent",
            self.auth_url,
            urlencoding::encode(&self.client_id),
            urlencoding::encode(&redirect_uri),
            urlencoding::encode(&self.scopes.join(" ")),
            urlencoding::encode(&challenge),
        );

        println!();
        println!("  Visit this URL to authenticate:");
        println!("  {auth_url}");
        println!();
        println!("  After authorizing, Google will show an authorization code.");
        println!("  Copy and paste it below:");

        let code = crate::tui::prompt_text("Authorization code", None)
            .map_err(|e| OAuthError::LoginFailed(format!("failed to read code: {e}")))?;

        self.exchange_code(code.trim(), &verifier, &redirect_uri)
            .await
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
        Some(format!(
            "{}/v1internal:streamGenerateContent?alt=sse",
            self.endpoints[0]
        ))
    }
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
        assert_eq!(parsed["request"]["messages"][0]["content"], "hello");
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
        assert!(url.contains("streamGenerateContent"));
    }
}
