mod storage;

pub mod codex;

pub use storage::TokenStorage;

use async_trait::async_trait;
use axum::http::HeaderMap;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Error type for OAuth operations.
#[derive(Debug, thiserror::Error)]
pub enum OAuthError {
    #[error("login failed: {0}")]
    LoginFailed(String),

    #[error("token refresh failed: {0}")]
    RefreshFailed(String),

    #[error("no tokens available for provider '{0}'")]
    NoTokens(String),

    #[error("token expired for provider '{0}'")]
    TokenExpired(String),

    #[error("provider not found: {0}")]
    ProviderNotFound(String),

    #[error("header error: {0}")]
    HeaderError(String),

    #[error("http error: {0}")]
    HttpError(#[from] reqwest::Error),

    #[error("io error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("json error: {0}")]
    JsonError(#[from] serde_json::Error),

    #[error("storage error: {0}")]
    StorageError(String),
}

impl From<axum::http::header::InvalidHeaderValue> for OAuthError {
    fn from(e: axum::http::header::InvalidHeaderValue) -> Self {
        OAuthError::HeaderError(e.to_string())
    }
}

/// Tokens obtained from an OAuth provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl OAuthTokens {
    pub fn is_expired(&self) -> bool {
        self.expires_at.is_some_and(|exp| exp <= Utc::now())
    }

    pub fn expires_in_secs(&self) -> Option<i64> {
        self.expires_at.map(|exp| (exp - Utc::now()).num_seconds())
    }
}

/// The core trait that each OAuth provider implements.
#[async_trait]
pub trait OAuthProvider: Send + Sync + fmt::Debug {
    /// Unique identifier (e.g., "codex").
    fn id(&self) -> &str;

    /// Display name (e.g., "Codex (OpenAI)").
    fn display_name(&self) -> &str;

    /// Execute browser-based OAuth login flow.
    async fn login_browser(&self, callback_port: u16) -> Result<OAuthTokens, OAuthError>;

    /// Execute headless login flow (device code or copy/paste URL).
    async fn login_headless(&self) -> Result<OAuthTokens, OAuthError>;

    /// Refresh the access token using the refresh token.
    async fn refresh(&self, refresh_token: &str) -> Result<OAuthTokens, OAuthError>;

    /// Inject provider-specific auth headers into the request.
    fn inject_auth(&self, headers: &mut HeaderMap, access_token: &str) -> Result<(), OAuthError>;

    /// Optionally transform the request body for provider-specific formats.
    /// Returns None for pass-through (Codex).
    fn prepare_request_body(
        &self,
        _body: &[u8],
        _tokens: &OAuthTokens,
    ) -> Result<Option<Vec<u8>>, OAuthError> {
        Ok(None)
    }

    /// Resolve the upstream URL for this provider.
    /// Returns None to use the configured [upstream] URL (Codex).
    fn upstream_url(&self, _tokens: &OAuthTokens) -> Option<String> {
        None
    }

    /// Whether this provider supports device code flow.
    fn supports_device_code(&self) -> bool {
        false
    }

    /// Whether this provider supports headless copy/paste URL fallback.
    fn supports_headless_url(&self) -> bool {
        false
    }

    /// Enrich tokens with provider-specific state if missing (e.g., project ID discovery).
    /// Returns `Some(enriched)` if tokens were updated, `None` if no changes needed.
    /// Called before `prepare_request_body` to ensure tokens are ready for use.
    async fn enrich_tokens(
        &self,
        _tokens: &OAuthTokens,
    ) -> Result<Option<OAuthTokens>, OAuthError> {
        Ok(None)
    }

    /// Optionally rewrite the request path (e.g., `/v1/chat/completions` → `/v1/responses`).
    /// Returns `None` to use the original path unchanged.
    fn rewrite_request_path(&self, _path: &str) -> Option<String> {
        None
    }

    /// Whether responses from the upstream need to be translated back
    /// to match the original request format.
    fn needs_response_translation(&self, _original_path: &str) -> bool {
        false
    }

    /// What format the upstream response is in, for translation purposes.
    /// Returns `None` if no translation is needed (passthrough).
    fn response_format(&self, _original_path: &str) -> Option<ResponseFormat> {
        None
    }
}

/// The format of upstream API responses, used to select the correct translator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseFormat {
    /// OpenAI Responses API → translate to chat.completion format
    ResponsesApi,
}

/// Configuration for an OAuth provider from TOML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthProviderConfig {
    pub provider: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback_port: Option<u16>,
}

fn default_true() -> bool {
    true
}

const CALLBACK_BIND_HOST_ENV: &str = "CLAWSHELL_OAUTH_CALLBACK_HOST";
const DEFAULT_CALLBACK_BIND_HOST: &str = "127.0.0.1";

pub(crate) fn callback_bind_addr(port: u16) -> String {
    let host = std::env::var(CALLBACK_BIND_HOST_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_CALLBACK_BIND_HOST.to_string());
    format!("{host}:{port}")
}

/// Manages multiple OAuth providers, their tokens, and per-provider refresh tasks.
#[derive(Debug)]
pub struct OAuthRegistry {
    providers: BTreeMap<String, Arc<dyn OAuthProvider>>,
    tokens: Arc<RwLock<BTreeMap<String, OAuthTokens>>>,
    storage: TokenStorage,
}

impl OAuthRegistry {
    pub fn new(storage: TokenStorage) -> Self {
        Self {
            providers: BTreeMap::new(),
            tokens: Arc::new(RwLock::new(BTreeMap::new())),
            storage,
        }
    }

    pub fn register(&mut self, provider: Arc<dyn OAuthProvider>) {
        let id = provider.id().to_string();
        debug!(provider = %id, "Registering OAuth provider");
        self.providers.insert(id, provider);
    }

    /// Load persisted tokens from disk for all registered providers.
    pub async fn load_tokens(&self) -> Result<(), OAuthError> {
        let mut tokens = self.tokens.write().await;
        for id in self.providers.keys() {
            match self.storage.load(id) {
                Ok(Some(t)) => {
                    info!(provider = %id, expired = t.is_expired(), "Loaded OAuth tokens from disk");
                    tokens.insert(id.clone(), t);
                }
                Ok(None) => {
                    debug!(provider = %id, "No persisted tokens found");
                }
                Err(e) => {
                    warn!(provider = %id, error = %e, "Failed to load persisted tokens");
                }
            }
        }
        Ok(())
    }

    /// Get the current access token for a provider, refreshing if expired.
    pub async fn current_access_token(&self, provider_id: &str) -> Result<String, OAuthError> {
        {
            let tokens = self.tokens.read().await;
            if let Some(t) = tokens.get(provider_id) {
                if !t.is_expired() {
                    return Ok(t.access_token.clone());
                }
            }
        }
        // Token is expired or missing — try refreshing
        self.refresh(provider_id).await?;
        let tokens = self.tokens.read().await;
        tokens
            .get(provider_id)
            .map(|t| t.access_token.clone())
            .ok_or_else(|| OAuthError::NoTokens(provider_id.to_string()))
    }

    /// Inject auth headers for the given provider.
    pub async fn inject_auth(
        &self,
        provider_id: &str,
        headers: &mut HeaderMap,
    ) -> Result<(), OAuthError> {
        let token = self.current_access_token(provider_id).await?;
        let provider = self
            .providers
            .get(provider_id)
            .ok_or_else(|| OAuthError::ProviderNotFound(provider_id.to_string()))?;
        provider.inject_auth(headers, &token)
    }

    /// Prepare the request body for the given provider.
    /// Calls `enrich_tokens` first to ensure provider-specific state is populated.
    pub async fn prepare_request_body(
        &self,
        provider_id: &str,
        body: &[u8],
    ) -> Result<Option<Vec<u8>>, OAuthError> {
        let provider = self
            .providers
            .get(provider_id)
            .ok_or_else(|| OAuthError::ProviderNotFound(provider_id.to_string()))?;

        // Enrich tokens on-demand if the provider needs it (e.g., project_id discovery)
        {
            let tokens = self.tokens.read().await;
            let t = tokens
                .get(provider_id)
                .ok_or_else(|| OAuthError::NoTokens(provider_id.to_string()))?;
            if let Some(enriched) = provider.enrich_tokens(t).await? {
                drop(tokens);
                info!(provider = %provider_id, "Enriched OAuth tokens with provider-specific state");
                if let Err(e) = self.storage.save(provider_id, &enriched) {
                    warn!(provider = %provider_id, error = %e, "Failed to persist enriched tokens");
                }
                self.tokens
                    .write()
                    .await
                    .insert(provider_id.to_string(), enriched);
            }
        }

        let tokens = self.tokens.read().await;
        let t = tokens
            .get(provider_id)
            .ok_or_else(|| OAuthError::NoTokens(provider_id.to_string()))?;
        provider.prepare_request_body(body, t)
    }

    /// Resolve the upstream URL for the given provider.
    pub async fn upstream_url(&self, provider_id: &str) -> Result<Option<String>, OAuthError> {
        let provider = self
            .providers
            .get(provider_id)
            .ok_or_else(|| OAuthError::ProviderNotFound(provider_id.to_string()))?;
        let tokens = self.tokens.read().await;
        let t = tokens
            .get(provider_id)
            .ok_or_else(|| OAuthError::NoTokens(provider_id.to_string()))?;
        Ok(provider.upstream_url(t))
    }

    /// Refresh the access token for a specific provider.
    pub async fn refresh(&self, provider_id: &str) -> Result<(), OAuthError> {
        let provider = self
            .providers
            .get(provider_id)
            .ok_or_else(|| OAuthError::ProviderNotFound(provider_id.to_string()))?;

        let refresh_token = {
            let tokens = self.tokens.read().await;
            tokens
                .get(provider_id)
                .and_then(|t| t.refresh_token.clone())
                .ok_or_else(|| {
                    OAuthError::RefreshFailed(format!(
                        "no refresh token for provider '{provider_id}'"
                    ))
                })?
        };

        info!(provider = %provider_id, "Refreshing OAuth access token");
        let new_tokens = provider.refresh(&refresh_token).await?;
        self.storage
            .save(provider_id, &new_tokens)
            .map_err(|e| OAuthError::StorageError(e.to_string()))?;
        self.tokens
            .write()
            .await
            .insert(provider_id.to_string(), new_tokens);
        info!(provider = %provider_id, "OAuth token refreshed successfully");
        Ok(())
    }

    /// Store tokens after a successful login (called from onboard flow).
    pub async fn store_tokens(
        &self,
        provider_id: &str,
        tokens: OAuthTokens,
    ) -> Result<(), OAuthError> {
        self.storage
            .save(provider_id, &tokens)
            .map_err(|e| OAuthError::StorageError(e.to_string()))?;
        self.tokens
            .write()
            .await
            .insert(provider_id.to_string(), tokens);
        Ok(())
    }

    /// Spawn background refresh tasks for all providers with tokens.
    pub fn spawn_refresh_tasks(&self, cancel: CancellationToken) {
        let tokens = Arc::clone(&self.tokens);
        for (id, provider) in &self.providers {
            let id = id.clone();
            let provider = Arc::clone(provider);
            let tokens = Arc::clone(&tokens);
            let storage = self.storage.clone();
            let cancel = cancel.clone();

            tokio::spawn(async move {
                loop {
                    let sleep_secs = {
                        let guard = tokens.read().await;
                        match guard.get(&id) {
                            Some(t) => {
                                let remaining = t.expires_in_secs().unwrap_or(3600);
                                // Refresh at 75% of TTL, minimum 60 seconds
                                (remaining * 3 / 4).max(60)
                            }
                            None => 3600, // no tokens yet, check hourly
                        }
                    };

                    debug!(provider = %id, sleep_secs, "OAuth refresh task sleeping");

                    tokio::select! {
                        _ = cancel.cancelled() => {
                            info!(provider = %id, "OAuth refresh task cancelled");
                            return;
                        }
                        _ = tokio::time::sleep(std::time::Duration::from_secs(sleep_secs as u64)) => {}
                    }

                    let refresh_token = {
                        let guard = tokens.read().await;
                        guard.get(&id).and_then(|t| t.refresh_token.clone())
                    };

                    let Some(refresh_token) = refresh_token else {
                        debug!(provider = %id, "No refresh token available, skipping refresh");
                        continue;
                    };

                    match provider.refresh(&refresh_token).await {
                        Ok(new_tokens) => {
                            if let Err(e) = storage.save(&id, &new_tokens) {
                                error!(provider = %id, error = %e, "Failed to persist refreshed tokens");
                            }
                            tokens.write().await.insert(id.clone(), new_tokens);
                            info!(provider = %id, "Background token refresh successful");
                        }
                        Err(e) => {
                            error!(provider = %id, error = %e, "Background token refresh failed");
                        }
                    }
                }
            });
        }
    }

    pub fn rewrite_request_path(
        &self,
        provider_id: &str,
        path: &str,
    ) -> Result<Option<String>, OAuthError> {
        let provider = self
            .providers
            .get(provider_id)
            .ok_or_else(|| OAuthError::ProviderNotFound(provider_id.to_string()))?;
        Ok(provider.rewrite_request_path(path))
    }

    pub fn needs_response_translation(
        &self,
        provider_id: &str,
        original_path: &str,
    ) -> Result<bool, OAuthError> {
        let provider = self
            .providers
            .get(provider_id)
            .ok_or_else(|| OAuthError::ProviderNotFound(provider_id.to_string()))?;
        Ok(provider.needs_response_translation(original_path))
    }

    pub fn response_format(
        &self,
        provider_id: &str,
        original_path: &str,
    ) -> Result<Option<ResponseFormat>, OAuthError> {
        let provider = self
            .providers
            .get(provider_id)
            .ok_or_else(|| OAuthError::ProviderNotFound(provider_id.to_string()))?;
        Ok(provider.response_format(original_path))
    }

    pub fn has_provider(&self, id: &str) -> bool {
        self.providers.contains_key(id)
    }

    pub fn provider_ids(&self) -> Vec<String> {
        self.providers.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct MockProvider {
        id: String,
    }

    #[async_trait]
    impl OAuthProvider for MockProvider {
        fn id(&self) -> &str {
            &self.id
        }
        fn display_name(&self) -> &str {
            "Mock Provider"
        }
        async fn login_browser(&self, _callback_port: u16) -> Result<OAuthTokens, OAuthError> {
            Ok(OAuthTokens {
                access_token: "mock-access".to_string(),
                refresh_token: Some("mock-refresh".to_string()),
                id_token: None,
                expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
                account_id: None,
                extra: BTreeMap::new(),
            })
        }
        async fn login_headless(&self) -> Result<OAuthTokens, OAuthError> {
            self.login_browser(0).await
        }
        async fn refresh(&self, _refresh_token: &str) -> Result<OAuthTokens, OAuthError> {
            Ok(OAuthTokens {
                access_token: "refreshed-access".to_string(),
                refresh_token: Some("new-refresh".to_string()),
                id_token: None,
                expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
                account_id: None,
                extra: BTreeMap::new(),
            })
        }
        fn inject_auth(
            &self,
            headers: &mut HeaderMap,
            access_token: &str,
        ) -> Result<(), OAuthError> {
            headers.insert(
                axum::http::header::AUTHORIZATION,
                format!("Bearer {access_token}").parse()?,
            );
            Ok(())
        }
    }

    #[test]
    fn test_tokens_not_expired() {
        let tokens = OAuthTokens {
            access_token: "test".to_string(),
            refresh_token: None,
            id_token: None,
            expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
            account_id: None,
            extra: BTreeMap::new(),
        };
        assert!(!tokens.is_expired());
    }

    #[test]
    fn test_tokens_expired() {
        let tokens = OAuthTokens {
            access_token: "test".to_string(),
            refresh_token: None,
            id_token: None,
            expires_at: Some(Utc::now() - chrono::Duration::hours(1)),
            account_id: None,
            extra: BTreeMap::new(),
        };
        assert!(tokens.is_expired());
    }

    #[test]
    fn test_tokens_no_expiry() {
        let tokens = OAuthTokens {
            access_token: "test".to_string(),
            refresh_token: None,
            id_token: None,
            expires_at: None,
            account_id: None,
            extra: BTreeMap::new(),
        };
        assert!(!tokens.is_expired());
        assert!(tokens.expires_in_secs().is_none());
    }

    #[tokio::test]
    async fn test_registry_register_and_access() {
        let dir = tempfile::tempdir().unwrap();
        let storage = TokenStorage::new(dir.path().to_path_buf());
        let mut registry = OAuthRegistry::new(storage);

        let provider = Arc::new(MockProvider {
            id: "mock".to_string(),
        });
        registry.register(provider);
        assert!(registry.has_provider("mock"));
        assert!(!registry.has_provider("other"));
    }

    #[tokio::test]
    async fn test_registry_store_and_retrieve_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let storage = TokenStorage::new(dir.path().to_path_buf());
        let mut registry = OAuthRegistry::new(storage);

        let provider = Arc::new(MockProvider {
            id: "mock".to_string(),
        });
        registry.register(provider);

        let tokens = OAuthTokens {
            access_token: "test-access".to_string(),
            refresh_token: Some("test-refresh".to_string()),
            id_token: None,
            expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
            account_id: None,
            extra: BTreeMap::new(),
        };
        registry.store_tokens("mock", tokens).await.unwrap();

        let token = registry.current_access_token("mock").await.unwrap();
        assert_eq!(token, "test-access");
    }

    #[tokio::test]
    async fn test_registry_refresh_expired_token() {
        let dir = tempfile::tempdir().unwrap();
        let storage = TokenStorage::new(dir.path().to_path_buf());
        let mut registry = OAuthRegistry::new(storage);

        let provider = Arc::new(MockProvider {
            id: "mock".to_string(),
        });
        registry.register(provider);

        // Store an expired token
        let tokens = OAuthTokens {
            access_token: "expired-access".to_string(),
            refresh_token: Some("test-refresh".to_string()),
            id_token: None,
            expires_at: Some(Utc::now() - chrono::Duration::hours(1)),
            account_id: None,
            extra: BTreeMap::new(),
        };
        registry.store_tokens("mock", tokens).await.unwrap();

        // Should auto-refresh
        let token = registry.current_access_token("mock").await.unwrap();
        assert_eq!(token, "refreshed-access");
    }

    #[tokio::test]
    async fn test_registry_inject_auth() {
        let dir = tempfile::tempdir().unwrap();
        let storage = TokenStorage::new(dir.path().to_path_buf());
        let mut registry = OAuthRegistry::new(storage);

        let provider = Arc::new(MockProvider {
            id: "mock".to_string(),
        });
        registry.register(provider);

        let tokens = OAuthTokens {
            access_token: "inject-test".to_string(),
            refresh_token: Some("r".to_string()),
            id_token: None,
            expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
            account_id: None,
            extra: BTreeMap::new(),
        };
        registry.store_tokens("mock", tokens).await.unwrap();

        let mut headers = HeaderMap::new();
        registry.inject_auth("mock", &mut headers).await.unwrap();
        assert_eq!(
            headers.get("authorization").unwrap().to_str().unwrap(),
            "Bearer inject-test"
        );
    }

    #[tokio::test]
    async fn test_registry_provider_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let storage = TokenStorage::new(dir.path().to_path_buf());
        let registry = OAuthRegistry::new(storage);

        let result = registry.current_access_token("nonexistent").await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("provider not found")
        );
    }

    #[tokio::test]
    async fn test_registry_load_tokens_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let storage = TokenStorage::new(dir.path().to_path_buf());

        // Pre-persist tokens
        let tokens = OAuthTokens {
            access_token: "disk-token".to_string(),
            refresh_token: Some("disk-refresh".to_string()),
            id_token: None,
            expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
            account_id: None,
            extra: BTreeMap::new(),
        };
        storage.save("mock", &tokens).unwrap();

        let mut registry = OAuthRegistry::new(storage);
        let provider = Arc::new(MockProvider {
            id: "mock".to_string(),
        });
        registry.register(provider);
        registry.load_tokens().await.unwrap();

        let token = registry.current_access_token("mock").await.unwrap();
        assert_eq!(token, "disk-token");
    }
}
