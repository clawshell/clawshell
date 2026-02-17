use google_gmail1 as gmail1;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::sync::Mutex;

use crate::config::GmailMode;

#[derive(Debug, Clone)]
pub struct GmailAccountCredentials {
    pub refresh: GmailRefreshCredentials,
    pub user_id: String,
}

#[derive(Debug, Clone)]
pub struct GmailRefreshCredentials {
    pub token_uri: String,
    pub refresh_token: String,
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Debug, Clone)]
pub struct GmailPolicy {
    pub mode: GmailMode,
    pub sender_rules: Vec<String>,
    pub default_max_results: u32,
}

impl GmailPolicy {
    pub fn sender_visible(&self, from_header: &str) -> bool {
        let Some(sender) = extract_sender_address(from_header) else {
            return false;
        };
        let matches_rule = self
            .sender_rules
            .iter()
            .any(|rule| sender_matches_rule(&sender, rule));
        match self.mode {
            GmailMode::Allowlist => matches_rule,
            GmailMode::Denylist => !matches_rule,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GmailListMessagesRequest {
    pub q: Option<String>,
    pub max_results: u32,
    pub page_token: Option<String>,
    pub include_spam_trash: bool,
}

#[derive(Debug, Clone)]
pub struct GmailMessageMetadata {
    pub id: String,
    pub thread_id: Option<String>,
    pub from: Option<String>,
    pub subject: Option<String>,
    pub date: Option<String>,
    pub snippet: Option<String>,
    pub internal_date_ms: Option<i64>,
    pub label_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct GmailListMessagesResponse {
    pub messages: Vec<GmailMessageMetadata>,
    pub next_page_token: Option<String>,
}

#[derive(Debug, Error)]
pub enum GmailServiceError {
    #[error("failed to build gmail client: {0}")]
    ClientSetup(String),
    #[error("failed to refresh gmail access token: {0}")]
    TokenRefresh(String),
    #[error("gmail api error: {0}")]
    Api(String),
}

#[derive(Debug, Clone)]
pub enum GmailService {
    Google(GoogleGmailService),
    #[cfg(test)]
    Mock(MockGmailService),
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub enum MockGmailService {
    Disabled,
    Static(GmailListMessagesResponse),
}

impl GmailService {
    pub async fn list_message_metadata(
        &self,
        account_key: &str,
        credentials: &GmailAccountCredentials,
        request: &GmailListMessagesRequest,
    ) -> Result<GmailListMessagesResponse, GmailServiceError> {
        match self {
            GmailService::Google(service) => {
                service
                    .list_message_metadata(account_key, credentials, request)
                    .await
            }
            #[cfg(test)]
            GmailService::Mock(mock) => match mock {
                MockGmailService::Disabled => {
                    Err(GmailServiceError::Api("gmail service disabled".to_string()))
                }
                MockGmailService::Static(response) => Ok(response.clone()),
            },
        }
    }

    #[cfg(test)]
    pub fn mock_disabled() -> Self {
        Self::Mock(MockGmailService::Disabled)
    }

    #[cfg(test)]
    pub fn mock_static(response: GmailListMessagesResponse) -> Self {
        Self::Mock(MockGmailService::Static(response))
    }
}

#[derive(Debug, Clone)]
pub struct GoogleGmailService {
    api_base_url: String,
    refresh_client: reqwest::Client,
    token_cache: Arc<Mutex<BTreeMap<String, CachedToken>>>,
}

#[derive(Debug, Clone)]
struct CachedToken {
    access_token: String,
    expires_at: Instant,
}

#[derive(Debug, Deserialize)]
struct RefreshTokenResponse {
    access_token: String,
    expires_in: Option<u64>,
}

impl GoogleGmailService {
    pub fn new(api_base_url: String) -> Self {
        Self {
            api_base_url: normalize_api_base_url(&api_base_url),
            refresh_client: reqwest::Client::new(),
            token_cache: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    async fn resolve_access_token(
        &self,
        account_key: &str,
        credentials: &GmailAccountCredentials,
    ) -> Result<String, GmailServiceError> {
        if let Some(cached) = self.cached_token(account_key).await {
            return Ok(cached);
        }

        let (access_token, expires_in) = self.refresh_access_token(&credentials.refresh).await?;
        self.store_cached_token(account_key, &access_token, expires_in)
            .await;
        Ok(access_token)
    }

    async fn cached_token(&self, account_key: &str) -> Option<String> {
        let cache = self.token_cache.lock().await;
        let entry = cache.get(account_key)?;
        if entry.expires_at > Instant::now() + Duration::from_secs(30) {
            Some(entry.access_token.clone())
        } else {
            None
        }
    }

    async fn store_cached_token(&self, account_key: &str, access_token: &str, expires_in: u64) {
        // Keep a safety window to refresh before exact expiry.
        let ttl = Duration::from_secs(expires_in.saturating_sub(30));
        let expires_at = Instant::now() + ttl;
        let mut cache = self.token_cache.lock().await;
        cache.insert(
            account_key.to_string(),
            CachedToken {
                access_token: access_token.to_string(),
                expires_at,
            },
        );
    }

    async fn refresh_access_token(
        &self,
        refresh: &GmailRefreshCredentials,
    ) -> Result<(String, u64), GmailServiceError> {
        let token_uri = refresh.token_uri.trim();
        let client_id = refresh.client_id.trim();
        let refresh_token = refresh.refresh_token.trim();
        let client_secret = refresh.client_secret.trim();
        let mut form_fields = vec![
            ("client_id", client_id),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
        ];
        if !client_secret.is_empty() {
            form_fields.push(("client_secret", client_secret));
        }

        let response = self
            .refresh_client
            .post(token_uri)
            .form(&form_fields)
            .send()
            .await
            .map_err(|e| GmailServiceError::TokenRefresh(e.to_string()))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| GmailServiceError::TokenRefresh(e.to_string()))?;
        if !status.is_success() {
            return Err(GmailServiceError::TokenRefresh(format!(
                "token endpoint returned {status}: {body}"
            )));
        }

        let parsed: RefreshTokenResponse = serde_json::from_str(&body)
            .map_err(|e| GmailServiceError::TokenRefresh(e.to_string()))?;
        let expires_in = parsed.expires_in.unwrap_or(300);
        Ok((parsed.access_token, expires_in))
    }
}

impl GoogleGmailService {
    pub async fn list_message_metadata(
        &self,
        account_key: &str,
        credentials: &GmailAccountCredentials,
        request: &GmailListMessagesRequest,
    ) -> Result<GmailListMessagesResponse, GmailServiceError> {
        let access_token = self.resolve_access_token(account_key, credentials).await?;
        let connector = gmail1::hyper_rustls::HttpsConnectorBuilder::new()
            .with_native_roots()
            .map_err(|e| GmailServiceError::ClientSetup(e.to_string()))?
            .https_or_http()
            .enable_http2()
            .build();
        let client = gmail1::hyper_util::client::legacy::Client::builder(
            gmail1::hyper_util::rt::TokioExecutor::new(),
        )
        .build(connector);
        let mut hub = gmail1::Gmail::new(client, access_token);
        hub.base_url(self.api_base_url.clone());
        hub.root_url(self.api_base_url.clone());

        let mut call = hub.users().messages_list(&credentials.user_id);
        if let Some(q) = request.q.as_deref() {
            call = call.q(q);
        }
        if let Some(page_token) = request.page_token.as_deref() {
            call = call.page_token(page_token);
        }
        if request.include_spam_trash {
            call = call.include_spam_trash(true);
        }
        call = call.max_results(request.max_results);

        let (_, list_response) = call
            .doit()
            .await
            .map_err(|e| GmailServiceError::Api(e.to_string()))?;

        let mut messages = Vec::new();
        for message in list_response.messages.unwrap_or_default() {
            let Some(message_id) = message.id else {
                continue;
            };

            let (_, detail) = hub
                .users()
                .messages_get(&credentials.user_id, &message_id)
                .format("metadata")
                .add_metadata_headers("From")
                .add_metadata_headers("Subject")
                .add_metadata_headers("Date")
                .doit()
                .await
                .map_err(|e| GmailServiceError::Api(e.to_string()))?;

            let headers = detail.payload.and_then(|p| p.headers).unwrap_or_default();
            let from = find_header_value(&headers, "from");
            let subject = find_header_value(&headers, "subject");
            let date = find_header_value(&headers, "date");

            messages.push(GmailMessageMetadata {
                id: detail.id.unwrap_or(message_id),
                thread_id: detail.thread_id,
                from,
                subject,
                date,
                snippet: detail.snippet,
                internal_date_ms: detail.internal_date,
                label_ids: detail.label_ids.unwrap_or_default(),
            });
        }

        Ok(GmailListMessagesResponse {
            messages,
            next_page_token: list_response.next_page_token,
        })
    }
}

fn normalize_api_base_url(api_base_url: &str) -> String {
    let mut value = api_base_url.trim().to_string();
    if !value.ends_with('/') {
        value.push('/');
    }
    value
}

fn find_header_value(headers: &[gmail1::api::MessagePartHeader], name: &str) -> Option<String> {
    headers.iter().find_map(|header| {
        let header_name = header.name.as_deref()?;
        if header_name.eq_ignore_ascii_case(name) {
            header.value.as_ref().map(|value| value.trim().to_string())
        } else {
            None
        }
    })
}

pub fn normalize_sender_rule(rule: &str) -> String {
    rule.trim().to_ascii_lowercase()
}

pub fn extract_sender_address(from_header: &str) -> Option<String> {
    let from_header = from_header.trim();
    if from_header.is_empty() {
        return None;
    }

    if let Some((start, end)) = from_header
        .rfind('<')
        .zip(from_header.rfind('>'))
        .filter(|(start, end)| start < end)
    {
        let candidate = from_header[start + 1..end].trim();
        if candidate.contains('@') {
            return Some(candidate.to_ascii_lowercase());
        }
    }

    from_header.split_whitespace().find_map(|token| {
        let token = token.trim_matches(|ch: char| matches!(ch, '"' | '\'' | '<' | '>' | ',' | ';'));
        if token.contains('@') {
            Some(token.to_ascii_lowercase())
        } else {
            None
        }
    })
}

pub fn sender_matches_rule(sender_email: &str, rule: &str) -> bool {
    let sender_email = sender_email.to_ascii_lowercase();
    let rule = normalize_sender_rule(rule);
    if let Some(domain_rule) = rule.strip_prefix('@') {
        if domain_rule.is_empty() {
            return false;
        }
        sender_email.ends_with(&format!("@{domain_rule}"))
    } else {
        sender_email == rule
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn test_extract_sender_address_with_display_name() {
        let sender = extract_sender_address("Alice Example <Alice@Example.com>").unwrap();
        assert_eq!(sender, "alice@example.com");
    }

    #[test]
    fn test_extract_sender_address_plain_email() {
        let sender = extract_sender_address("user@example.com").unwrap();
        assert_eq!(sender, "user@example.com");
    }

    #[test]
    fn test_sender_matches_rule_exact() {
        assert!(sender_matches_rule(
            "alice@example.com",
            "ALICE@EXAMPLE.COM"
        ));
        assert!(!sender_matches_rule("alice@example.com", "bob@example.com"));
    }

    #[test]
    fn test_sender_matches_rule_domain() {
        assert!(sender_matches_rule("alice@example.com", "@example.com"));
        assert!(!sender_matches_rule("alice@other.com", "@example.com"));
    }

    #[test]
    fn test_allowlist_policy() {
        let policy = GmailPolicy {
            mode: GmailMode::Allowlist,
            sender_rules: vec![normalize_sender_rule("@trusted.com")],
            default_max_results: 50,
        };
        assert!(policy.sender_visible("Alice <alice@trusted.com>"));
        assert!(!policy.sender_visible("Bob <bob@untrusted.com>"));
    }

    #[test]
    fn test_denylist_policy() {
        let policy = GmailPolicy {
            mode: GmailMode::Denylist,
            sender_rules: vec![normalize_sender_rule("@blocked.com")],
            default_max_results: 50,
        };
        assert!(!policy.sender_visible("Spam <offer@blocked.com>"));
        assert!(policy.sender_visible("News <news@trusted.com>"));
    }

    #[tokio::test]
    async fn test_resolve_access_token_refreshes_and_caches() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(body_string_contains("grant_type=refresh_token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "refreshed-token",
                "expires_in": 3600,
                "token_type": "Bearer"
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        let service = GoogleGmailService::new("https://gmail.googleapis.com/".to_string());
        let credentials = GmailAccountCredentials {
            refresh: GmailRefreshCredentials {
                token_uri: mock_server.uri(),
                refresh_token: "refresh-token".to_string(),
                client_id: "client-id".to_string(),
                client_secret: "client-secret".to_string(),
            },
            user_id: "me".to_string(),
        };

        let token_1 = service
            .resolve_access_token("vk-test", &credentials)
            .await
            .unwrap();
        let token_2 = service
            .resolve_access_token("vk-test", &credentials)
            .await
            .unwrap();

        assert_eq!(token_1, "refreshed-token");
        assert_eq!(token_2, "refreshed-token");
    }
}
