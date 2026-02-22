use super::{OAuthError, OAuthProvider, OAuthTokens};
use async_trait::async_trait;
use axum::http::header::AUTHORIZATION;
use axum::http::HeaderMap;
use chrono::Utc;
use std::collections::BTreeMap;
use tracing::{debug, info};

const DEFAULT_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const DEFAULT_AUTH_URL: &str = "https://auth.openai.com/authorize";
const DEFAULT_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const DEFAULT_SCOPES: &[&str] = &["openid", "profile", "email", "offline_access"];

#[derive(Debug)]
pub struct CodexProvider {
    client_id: String,
    auth_url: String,
    token_url: String,
    scopes: Vec<String>,
    http_client: reqwest::Client,
}

impl CodexProvider {
    pub fn new(
        client_id: Option<&str>,
        auth_url: Option<&str>,
        token_url: Option<&str>,
        scopes: Option<&[String]>,
    ) -> Self {
        Self {
            client_id: client_id.unwrap_or(DEFAULT_CLIENT_ID).to_string(),
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
            ("client_id", &self.client_id),
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
        parse_token_response(&json)
    }

    async fn exchange_refresh_token(
        &self,
        refresh_token: &str,
    ) -> Result<OAuthTokens, OAuthError> {
        let params = [
            ("grant_type", "refresh_token"),
            ("client_id", &self.client_id),
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
        parse_token_response(&json)
    }

    /// Poll OpenAI's custom device-auth token endpoint until user authorises.
    /// Returns (authorization_code, code_verifier) on success.
    async fn poll_device_auth(
        &self,
        device_auth_id: &str,
        user_code: &str,
        interval: u64,
    ) -> Result<(String, String), OAuthError> {
        let url = self.device_auth_base_url() + "/token";
        let max_wait = std::time::Duration::from_secs(15 * 60);
        let start = std::time::Instant::now();

        loop {
            tokio::time::sleep(std::time::Duration::from_secs(interval)).await;

            if start.elapsed() > max_wait {
                return Err(OAuthError::LoginFailed(
                    "device code polling timed out (15 min)".to_string(),
                ));
            }

            let body = serde_json::json!({
                "device_auth_id": device_auth_id,
                "user_code": user_code,
            });

            let resp = self
                .http_client
                .post(&url)
                .json(&body)
                .send()
                .await?;

            if resp.status().is_success() {
                let json: serde_json::Value = resp.json().await?;
                let auth_code = json
                    .get("authorization_code")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        OAuthError::LoginFailed(
                            "missing authorization_code in device-auth response".to_string(),
                        )
                    })?
                    .to_string();
                let code_verifier = json
                    .get("code_verifier")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        OAuthError::LoginFailed(
                            "missing code_verifier in device-auth response".to_string(),
                        )
                    })?
                    .to_string();
                return Ok((auth_code, code_verifier));
            }

            // 403 / 404 = authorization still pending
            let status = resp.status();
            if status == reqwest::StatusCode::FORBIDDEN
                || status == reqwest::StatusCode::NOT_FOUND
            {
                debug!("Device code authorization pending ({status})");
                continue;
            }

            let text = resp.text().await.unwrap_or_default();
            return Err(OAuthError::LoginFailed(format!(
                "device-auth polling failed ({status}): {text}"
            )));
        }
    }

    /// Base URL for OpenAI's custom device-auth API, derived from `auth_url`.
    fn device_auth_base_url(&self) -> String {
        // auth_url is e.g. "https://auth.openai.com/authorize"
        // We need "https://auth.openai.com/api/accounts/deviceauth"
        let base = self
            .auth_url
            .trim_end_matches("/authorize")
            .trim_end_matches('/');
        format!("{base}/api/accounts/deviceauth")
    }
}

fn parse_token_response(json: &serde_json::Value) -> Result<OAuthTokens, OAuthError> {
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

#[async_trait]
impl OAuthProvider for CodexProvider {
    fn id(&self) -> &str {
        "codex"
    }

    fn display_name(&self) -> &str {
        "Codex / ChatGPT (OAuth)"
    }

    fn supports_device_code(&self) -> bool {
        true
    }

    async fn login_browser(&self, callback_port: u16) -> Result<OAuthTokens, OAuthError> {
        let (verifier, challenge) = generate_pkce();
        let redirect_uri = format!("http://localhost:{callback_port}/auth/callback");
        let state: String = uuid::Uuid::new_v4().to_string();

        let auth_url = format!(
            "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256&state={}",
            self.auth_url,
            urlencoding::encode(&self.client_id),
            urlencoding::encode(&redirect_uri),
            urlencoding::encode(&self.scopes.join(" ")),
            urlencoding::encode(&challenge),
            urlencoding::encode(&state),
        );

        info!("Opening browser for Codex OAuth login");
        if let Err(e) = open::that(&auth_url) {
            return Err(OAuthError::LoginFailed(format!(
                "failed to open browser: {e}. Visit this URL manually: {auth_url}"
            )));
        }

        // Start a temporary HTTP server to receive the callback
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
        // Step 1: Request a user code from OpenAI's device-auth endpoint
        let usercode_url = self.device_auth_base_url() + "/usercode";
        let body = serde_json::json!({ "client_id": self.client_id });

        let resp = self
            .http_client
            .post(&usercode_url)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(OAuthError::LoginFailed(format!(
                "device code request failed ({status}): {text}"
            )));
        }

        let json: serde_json::Value = resp.json().await?;

        let device_auth_id = json
            .get("device_auth_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                OAuthError::LoginFailed("missing device_auth_id in response".to_string())
            })?;

        let user_code = json
            .get("user_code")
            .or_else(|| json.get("usercode"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                OAuthError::LoginFailed("missing user_code in response".to_string())
            })?;

        let interval = json
            .get("interval")
            .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
            .unwrap_or(5);

        // Verification URL for the user
        let base = self
            .auth_url
            .trim_end_matches("/authorize")
            .trim_end_matches('/');
        let verification_url = format!("{base}/codex/device");

        println!();
        println!("  Visit: {verification_url}");
        println!("  Enter code: {user_code}");
        println!();

        // Step 2: Poll until user authorises, get authorization_code + code_verifier
        let (auth_code, code_verifier) =
            self.poll_device_auth(device_auth_id, user_code, interval).await?;

        // Step 3: Exchange authorization_code for tokens via the standard token endpoint
        let redirect_uri = format!("{base}/deviceauth/callback");
        self.exchange_code(&auth_code, &code_verifier, &redirect_uri)
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
        Ok(())
    }
}

/// Wait for an OAuth callback on a local HTTP server.
/// Returns (code, state).
async fn wait_for_oauth_callback(
    port: u16,
) -> Result<(String, String), Box<dyn std::error::Error + Send + Sync>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}")).await?;
    let (mut stream, _) = listener.accept().await?;

    let mut buf = vec![0u8; 4096];
    let n = stream.read(&mut buf).await?;
    let request = String::from_utf8_lossy(&buf[..n]);

    // Parse the GET request for code and state query params
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

    #[test]
    fn test_parse_token_response() {
        let json = serde_json::json!({
            "access_token": "eyJ...",
            "refresh_token": "v1.MjQ...",
            "id_token": "eyJhbG...",
            "expires_in": 3600,
            "token_type": "Bearer"
        });

        let tokens = parse_token_response(&json).unwrap();
        assert_eq!(tokens.access_token, "eyJ...");
        assert_eq!(tokens.refresh_token.as_deref(), Some("v1.MjQ..."));
        assert_eq!(tokens.id_token.as_deref(), Some("eyJhbG..."));
        assert!(tokens.expires_at.is_some());
    }

    #[test]
    fn test_parse_token_response_missing_access_token() {
        let json = serde_json::json!({
            "refresh_token": "v1.MjQ...",
        });

        let result = parse_token_response(&json);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_token_response_minimal() {
        let json = serde_json::json!({
            "access_token": "minimal"
        });

        let tokens = parse_token_response(&json).unwrap();
        assert_eq!(tokens.access_token, "minimal");
        assert!(tokens.refresh_token.is_none());
        assert!(tokens.id_token.is_none());
        assert!(tokens.expires_at.is_none());
    }

    #[test]
    fn test_generate_pkce() {
        let (verifier, challenge) = generate_pkce();
        assert!(!verifier.is_empty());
        assert!(!challenge.is_empty());
        assert_ne!(verifier, challenge);

        // Verify challenge is S256 of verifier
        use base64::Engine;
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(verifier.as_bytes());
        let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize());
        assert_eq!(challenge, expected);
    }

    #[test]
    fn test_codex_provider_defaults() {
        let provider = CodexProvider::new(None, None, None, None);
        assert_eq!(provider.id(), "codex");
        assert_eq!(provider.display_name(), "Codex / ChatGPT (OAuth)");
        assert!(provider.supports_device_code());
        assert!(!provider.supports_headless_url());
        assert_eq!(provider.client_id, DEFAULT_CLIENT_ID);
    }

    #[test]
    fn test_codex_provider_custom() {
        let provider = CodexProvider::new(
            Some("custom-client"),
            Some("https://custom.auth/authorize"),
            Some("https://custom.auth/token"),
            Some(&["openid".to_string()]),
        );
        assert_eq!(provider.client_id, "custom-client");
        assert_eq!(provider.auth_url, "https://custom.auth/authorize");
        assert_eq!(provider.token_url, "https://custom.auth/token");
        assert_eq!(provider.scopes, vec!["openid"]);
    }

    #[test]
    fn test_inject_auth() {
        let provider = CodexProvider::new(None, None, None, None);
        let mut headers = HeaderMap::new();
        provider.inject_auth(&mut headers, "test-token").unwrap();
        assert_eq!(
            headers.get("authorization").unwrap().to_str().unwrap(),
            "Bearer test-token"
        );
    }

    #[test]
    fn test_prepare_request_body_passthrough() {
        let provider = CodexProvider::new(None, None, None, None);
        let tokens = OAuthTokens {
            access_token: "t".to_string(),
            refresh_token: None,
            id_token: None,
            expires_at: None,
            account_id: None,
            extra: BTreeMap::new(),
        };
        let result = provider.prepare_request_body(b"test body", &tokens).unwrap();
        assert!(result.is_none()); // pass-through
    }

    #[test]
    fn test_upstream_url_none() {
        let provider = CodexProvider::new(None, None, None, None);
        let tokens = OAuthTokens {
            access_token: "t".to_string(),
            refresh_token: None,
            id_token: None,
            expires_at: None,
            account_id: None,
            extra: BTreeMap::new(),
        };
        assert!(provider.upstream_url(&tokens).is_none());
    }
}
