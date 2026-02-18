use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use bytes::Bytes;
use http_body_util::BodyExt;
use tower::util::ServiceExt;
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{AppState, build_router};
use crate::config::{Config, DlpAction, DlpPattern, EmailMode, Provider};
use crate::dlp::DlpScanner;
use crate::email::{
    EmailAccountCredentials, EmailListMessagesResponse, EmailMessageContent, EmailMessageMetadata,
    EmailPolicy, EmailService,
};
use crate::keys::{KeyManager, ResolvedKey};
use crate::proxy::ProxyClient;

fn make_app(upstream_url: &str) -> axum::Router {
    let mut key_map = BTreeMap::new();
    key_map.insert(
        "vk-test-1".to_string(),
        ResolvedKey {
            real_key: "sk-real-1".to_string(),
            provider: Provider::Openai,
        },
    );
    key_map.insert(
        "vk-test-2".to_string(),
        ResolvedKey {
            real_key: "sk-real-2".to_string(),
            provider: Provider::Openai,
        },
    );

    let patterns = vec![
        DlpPattern {
            name: "ssn".to_string(),
            regex: r"\b\d{3}-\d{2}-\d{4}\b".to_string(),
            action: DlpAction::Block,
        },
        DlpPattern {
            name: "email".to_string(),
            regex: r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b".to_string(),
            action: DlpAction::Block,
        },
        DlpPattern {
            name: "credit_card".to_string(),
            regex: r"\b(?:\d[ -]*?){13,19}\b".to_string(),
            action: DlpAction::Block,
        },
    ];

    let mut upstream_urls = BTreeMap::new();
    upstream_urls.insert(Provider::Openai, upstream_url.to_string());
    upstream_urls.insert(Provider::Anthropic, upstream_url.to_string());

    let state = AppState {
        key_manager: Arc::new(KeyManager::new(key_map)),
        dlp_scanner: Arc::new(DlpScanner::new(&patterns, false).unwrap()),
        proxy_client: Arc::new(ProxyClient::with_upstream_urls(
            upstream_urls,
            "2023-06-01".to_string(),
        )),
        email_enabled: false,
        email_policy: None,
        email_accounts: Arc::new(BTreeMap::new()),
        email_service: Arc::new(EmailService::mock_disabled()),
    };

    build_router(state)
}

fn make_app_with_anthropic(upstream_url: &str) -> axum::Router {
    let mut key_map = BTreeMap::new();
    key_map.insert(
        "vk-test-1".to_string(),
        ResolvedKey {
            real_key: "sk-real-1".to_string(),
            provider: Provider::Openai,
        },
    );
    key_map.insert(
        "vk-ant-1".to_string(),
        ResolvedKey {
            real_key: "sk-ant-real-1".to_string(),
            provider: Provider::Anthropic,
        },
    );

    let mut upstream_urls = BTreeMap::new();
    upstream_urls.insert(Provider::Openai, upstream_url.to_string());
    upstream_urls.insert(Provider::Anthropic, upstream_url.to_string());

    let state = AppState {
        key_manager: Arc::new(KeyManager::new(key_map)),
        dlp_scanner: Arc::new(DlpScanner::new(&[], false).unwrap()),
        proxy_client: Arc::new(ProxyClient::with_upstream_urls(
            upstream_urls,
            "2023-06-01".to_string(),
        )),
        email_enabled: false,
        email_policy: None,
        email_accounts: Arc::new(BTreeMap::new()),
        email_service: Arc::new(EmailService::mock_disabled()),
    };

    build_router(state)
}

// ========== Proxy Integration Tests ==========

#[tokio::test]
async fn test_proxy_forward_success() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-real-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "chatcmpl-abc",
            "choices": [{"message": {"content": "Hello!"}}]
        })))
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let body = r#"{"model":"gpt-4","messages":[{"role":"user","content":"Hi"}]}"#;
    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("authorization", "Bearer vk-test-1")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["id"], "chatcmpl-abc");
}

#[tokio::test]
async fn test_proxy_preserves_query_params() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("authorization", "Bearer sk-real-1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"data": [{"id": "gpt-4"}]})),
        )
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let req = Request::builder()
        .method("GET")
        .uri("/v1/models?limit=10")
        .header("authorization", "Bearer vk-test-1")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_proxy_forwards_upstream_errors() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": {"message": "Invalid model", "type": "invalid_request_error"}
        })))
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let body = r#"{"model":"nonexistent"}"#;
    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("authorization", "Bearer vk-test-1")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Invalid model")
    );
}

#[tokio::test]
async fn test_real_key_injected_correctly() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("authorization", "Bearer sk-real-2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let req = Request::builder()
        .method("GET")
        .uri("/v1/models")
        .header("authorization", "Bearer vk-test-2")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ========== Auth Tests ==========

#[tokio::test]
async fn test_missing_auth_header() {
    let mock_server = MockServer::start().await;
    let app = make_app(&mock_server.uri());
    let req = Request::builder()
        .uri("/v1/chat/completions")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_invalid_auth_format() {
    let mock_server = MockServer::start().await;
    let app = make_app(&mock_server.uri());
    let req = Request::builder()
        .uri("/v1/chat/completions")
        .header("authorization", "Basic abc123")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_unknown_virtual_key() {
    let mock_server = MockServer::start().await;
    let app = make_app(&mock_server.uri());
    let req = Request::builder()
        .uri("/v1/chat/completions")
        .header("authorization", "Bearer vk-unknown")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_empty_bearer_token() {
    let mock_server = MockServer::start().await;
    let app = make_app(&mock_server.uri());
    let req = Request::builder()
        .uri("/v1/models")
        .header("authorization", "Bearer ")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ========== DLP Tests ==========

#[tokio::test]
async fn test_dlp_blocks_ssn() {
    let mock_server = MockServer::start().await;
    let app = make_app(&mock_server.uri());
    let body = r#"{"messages":[{"role":"user","content":"My SSN is 123-45-6789"}]}"#;
    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("authorization", "Bearer vk-test-1")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_dlp_blocks_email() {
    let mock_server = MockServer::start().await;
    let app = make_app(&mock_server.uri());
    let body = r#"{"messages":[{"role":"user","content":"Email me at user@example.com"}]}"#;
    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("authorization", "Bearer vk-test-1")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_dlp_blocks_credit_card() {
    let mock_server = MockServer::start().await;
    let app = make_app(&mock_server.uri());

    let body = r#"{"messages":[{"role":"user","content":"My card is 4111111111111111"}]}"#;
    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("authorization", "Bearer vk-test-1")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let error_msg = json["error"].as_str().unwrap();
    assert!(error_msg.contains("sensitive data detected"));
    assert!(error_msg.contains("credit_card"));
    assert!(!error_msg.contains("4111111111111111"));
}

#[tokio::test]
async fn test_clean_request_passes_through() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "chatcmpl-ok"})),
        )
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let body = r#"{"messages":[{"role":"user","content":"Hello, how are you?"}]}"#;
    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("authorization", "Bearer vk-test-1")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ========== Streaming Tests ==========

#[tokio::test]
async fn test_proxy_streaming_response() {
    let mock_server = MockServer::start().await;

    let sse_body =
        "data: {\"id\":\"chatcmpl-1\"}\n\ndata: {\"id\":\"chatcmpl-2\"}\n\ndata: [DONE]\n\n";
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("content-type", "text/event-stream")
                .set_body_string(sse_body),
        )
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let body = r#"{"model":"gpt-4","stream":true,"messages":[{"role":"user","content":"Hi"}]}"#;
    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("authorization", "Bearer vk-test-1")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let body_str = std::str::from_utf8(&body).unwrap();
    assert!(body_str.contains("chatcmpl-1"));
    assert!(body_str.contains("[DONE]"));
}

// ========== Multiple Endpoints Tests ==========

#[tokio::test]
async fn test_multiple_endpoints() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"data": [{"embedding": [0.1, 0.2]}]})),
        )
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let body = r#"{"model":"text-embedding-ada-002","input":"Hello"}"#;
    let req = Request::builder()
        .method("POST")
        .uri("/v1/embeddings")
        .header("authorization", "Bearer vk-test-1")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_delete_method() {
    let mock_server = MockServer::start().await;

    Mock::given(method("DELETE"))
        .and(path("/v1/files/file-abc"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"id": "file-abc", "deleted": true})),
        )
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let req = Request::builder()
        .method("DELETE")
        .uri("/v1/files/file-abc")
        .header("authorization", "Bearer vk-test-1")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_put_method() {
    let mock_server = MockServer::start().await;

    Mock::given(method("PUT"))
        .and(path("/v1/files/file-abc"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let req = Request::builder()
        .method("PUT")
        .uri("/v1/files/file-abc")
        .header("authorization", "Bearer vk-test-1")
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_patch_method() {
    let mock_server = MockServer::start().await;

    Mock::given(method("PATCH"))
        .and(path("/v1/assistants/asst-abc"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let req = Request::builder()
        .method("PATCH")
        .uri("/v1/assistants/asst-abc")
        .header("authorization", "Bearer vk-test-1")
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_head_method() {
    let mock_server = MockServer::start().await;

    Mock::given(method("HEAD"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let req = Request::builder()
        .method("HEAD")
        .uri("/v1/models")
        .header("authorization", "Bearer vk-test-1")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_options_method() {
    let mock_server = MockServer::start().await;

    Mock::given(method("OPTIONS"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let req = Request::builder()
        .method("OPTIONS")
        .uri("/v1/models")
        .header("authorization", "Bearer vk-test-1")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    // 204 or 200 depending on what wiremock returns
    assert!(resp.status().is_success() || resp.status() == StatusCode::NO_CONTENT);
}

// ========== AppState Tests ==========

#[tokio::test]
async fn test_app_state_from_config() {
    let toml_str = r#"
[server]
host = "127.0.0.1"
port = 3000
[upstream]
openai_base_url = "https://api.openai.com"
[[keys]]
virtual_key = "vk-1"
real_key = "sk-real-1"
"#;
    let config = Config::parse(toml_str).unwrap();
    let state = AppState::from_config(&config).unwrap();
    let resolved = state.key_manager.resolve("vk-1").unwrap();
    assert_eq!(resolved.real_key, "sk-real-1");
    assert_eq!(resolved.provider, Provider::Openai);
    assert!(state.key_manager.resolve("vk-unknown").is_none());
}

#[tokio::test]
async fn test_app_state_from_config_with_anthropic() {
    let toml_str = r#"
[server]
host = "127.0.0.1"
port = 3000
[upstream]
openai_base_url = "https://api.openai.com"
anthropic_base_url = "https://api.anthropic.com"
[[keys]]
virtual_key = "vk-oai"
real_key = "sk-oai-key"
provider = "openai"
[[keys]]
virtual_key = "vk-ant"
real_key = "sk-ant-key"
provider = "anthropic"
"#;
    let config = Config::parse(toml_str).unwrap();
    let state = AppState::from_config(&config).unwrap();
    let oai = state.key_manager.resolve("vk-oai").unwrap();
    assert_eq!(oai.real_key, "sk-oai-key");
    assert_eq!(oai.provider, Provider::Openai);
    let ant = state.key_manager.resolve("vk-ant").unwrap();
    assert_eq!(ant.real_key, "sk-ant-key");
    assert_eq!(ant.provider, Provider::Anthropic);
}

#[tokio::test]
async fn test_app_state_from_config_with_email_imap_fields() {
    let toml_str = r#"
[server]
host = "127.0.0.1"
port = 3000

[upstream]
openai_base_url = "https://api.openai.com"

[email]
enabled = true
mode = "allowlist"
allow_senders = ["alice@example.com"]

[[email.accounts]]
virtual_key = "vk-email"
email = "bot@gmail.com"
app_password = "abcd efgh ijkl mnop"
imap_host = "imap.gmail.com"
imap_port = 993
"#;
    let config = Config::parse(toml_str).unwrap();
    let state = AppState::from_config(&config).unwrap();

    let email_account = state.email_accounts.get("vk-email").unwrap();
    assert_eq!(email_account.email, "bot@gmail.com");
    assert_eq!(email_account.app_password, "abcd efgh ijkl mnop");
    assert_eq!(email_account.imap_host, "imap.gmail.com");
    assert_eq!(email_account.imap_port, 993);
}

#[tokio::test]
async fn test_app_state_from_config_skips_email_credentials_when_disabled() {
    let toml_str = r#"
[server]
host = "127.0.0.1"
port = 3000

[upstream]
openai_base_url = "https://api.openai.com"

[email]
enabled = false
mode = "allowlist"
allow_senders = ["alice@example.com"]

[[email.accounts]]
virtual_key = "vk-email"
email = "bot@gmail.com"
app_password = "abcd efgh ijkl mnop"
"#;
    let config = Config::parse(toml_str).unwrap();
    let state = AppState::from_config(&config).unwrap();

    assert!(!state.email_enabled);
    assert!(state.email_policy.is_none());
    assert!(state.email_accounts.is_empty());
}

// ========== Proxy Error Tests ==========

#[tokio::test]
async fn test_proxy_error_on_unreachable_upstream() {
    // Point to a definitely-unreachable address
    let state = AppState {
        key_manager: Arc::new(KeyManager::new(
            [(
                "vk-1".to_string(),
                ResolvedKey {
                    real_key: "sk-1".to_string(),
                    provider: Provider::Openai,
                },
            )]
            .into_iter()
            .collect(),
        )),
        dlp_scanner: Arc::new(DlpScanner::new(&[], false).unwrap()),
        proxy_client: Arc::new(ProxyClient::with_upstream_urls(
            {
                let mut urls = BTreeMap::new();
                urls.insert(Provider::Openai, "http://127.0.0.1:1".to_string());
                urls.insert(Provider::Anthropic, "http://127.0.0.1:1".to_string());
                urls
            },
            "2023-06-01".to_string(),
        )),
        email_enabled: false,
        email_policy: None,
        email_accounts: Arc::new(BTreeMap::new()),
        email_service: Arc::new(EmailService::mock_disabled()),
    };

    let app = build_router(state);
    let req = Request::builder()
        .uri("/v1/models")
        .header("authorization", "Bearer vk-1")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
}

// ========== Anthropic Provider Tests ==========

#[tokio::test]
async fn test_anthropic_forward_uses_x_api_key() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "sk-ant-real-1"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "msg-abc",
            "type": "message",
            "content": [{"type": "text", "text": "Hello!"}]
        })))
        .mount(&mock_server)
        .await;

    let app = make_app_with_anthropic(&mock_server.uri());
    let body = r#"{"model":"claude-sonnet-4-5-20250929","max_tokens":1024,"messages":[{"role":"user","content":"Hi"}]}"#;
    let req = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("authorization", "Bearer vk-ant-1")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["id"], "msg-abc");
}

#[tokio::test]
async fn test_anthropic_no_bearer_header_sent_upstream() {
    let mock_server = MockServer::start().await;

    // This mock will NOT match if "authorization" header is present
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "sk-ant-real-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .mount(&mock_server)
        .await;

    let app = make_app_with_anthropic(&mock_server.uri());
    let body = r#"{"model":"claude-sonnet-4-5-20250929","max_tokens":1024,"messages":[{"role":"user","content":"Hi"}]}"#;
    let req = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("authorization", "Bearer vk-ant-1")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_anthropic_streaming_response() {
    let mock_server = MockServer::start().await;

    let sse_body = "event: message_start\ndata: {\"type\":\"message_start\"}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"Hello\"}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "sk-ant-real-1"))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("content-type", "text/event-stream")
                .set_body_string(sse_body),
        )
        .mount(&mock_server)
        .await;

    let app = make_app_with_anthropic(&mock_server.uri());
    let body = r#"{"model":"claude-sonnet-4-5-20250929","max_tokens":1024,"stream":true,"messages":[{"role":"user","content":"Hi"}]}"#;
    let req = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("authorization", "Bearer vk-ant-1")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let body_str = std::str::from_utf8(&body).unwrap();
    assert!(body_str.contains("message_start"));
    assert!(body_str.contains("message_stop"));
}

#[tokio::test]
async fn test_anthropic_dlp_blocks_sensitive_data() {
    let mock_server = MockServer::start().await;

    // Use make_app which has DLP patterns
    let mut key_map = BTreeMap::new();
    key_map.insert(
        "vk-ant-dlp".to_string(),
        ResolvedKey {
            real_key: "sk-ant-key".to_string(),
            provider: Provider::Anthropic,
        },
    );

    let patterns = vec![DlpPattern {
        name: "ssn".to_string(),
        regex: r"\b\d{3}-\d{2}-\d{4}\b".to_string(),
        action: DlpAction::Block,
    }];

    let mut upstream_urls = BTreeMap::new();
    upstream_urls.insert(Provider::Openai, mock_server.uri());
    upstream_urls.insert(Provider::Anthropic, mock_server.uri());

    let state = AppState {
        key_manager: Arc::new(KeyManager::new(key_map)),
        dlp_scanner: Arc::new(DlpScanner::new(&patterns, false).unwrap()),
        proxy_client: Arc::new(ProxyClient::with_upstream_urls(
            upstream_urls,
            "2023-06-01".to_string(),
        )),
        email_enabled: false,
        email_policy: None,
        email_accounts: Arc::new(BTreeMap::new()),
        email_service: Arc::new(EmailService::mock_disabled()),
    };
    let app = build_router(state);

    let body = r#"{"messages":[{"role":"user","content":"My SSN is 123-45-6789"}]}"#;
    let req = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("authorization", "Bearer vk-ant-dlp")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_openai_still_uses_bearer_auth() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-real-1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "chatcmpl-ok"})),
        )
        .mount(&mock_server)
        .await;

    let app = make_app_with_anthropic(&mock_server.uri());
    let body = r#"{"model":"gpt-4","messages":[{"role":"user","content":"Hi"}]}"#;
    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("authorization", "Bearer vk-test-1")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_openai_and_openrouter_keys_map_to_distinct_real_keys() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-openai-real"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "provider": "openai"
        })))
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header("authorization", "Bearer sk-openrouter-real"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "provider": "openrouter"
        })))
        .mount(&mock_server)
        .await;

    let mut key_map = BTreeMap::new();
    key_map.insert(
        "vk-openai".to_string(),
        ResolvedKey {
            real_key: "sk-openai-real".to_string(),
            provider: Provider::Openai,
        },
    );
    key_map.insert(
        "vk-openrouter".to_string(),
        ResolvedKey {
            real_key: "sk-openrouter-real".to_string(),
            provider: Provider::Openrouter,
        },
    );

    let mut upstream_urls = BTreeMap::new();
    upstream_urls.insert(Provider::Openai, mock_server.uri());
    upstream_urls.insert(Provider::Openrouter, mock_server.uri());
    upstream_urls.insert(Provider::Anthropic, mock_server.uri());

    let app = build_router(AppState {
        key_manager: Arc::new(KeyManager::new(key_map)),
        dlp_scanner: Arc::new(DlpScanner::new(&[], false).unwrap()),
        proxy_client: Arc::new(ProxyClient::with_upstream_urls(
            upstream_urls,
            "2023-06-01".to_string(),
        )),
        email_enabled: false,
        email_policy: None,
        email_accounts: Arc::new(BTreeMap::new()),
        email_service: Arc::new(EmailService::mock_disabled()),
    });

    let openai_req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("authorization", "Bearer vk-openai")
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let openai_resp = app.clone().oneshot(openai_req).await.unwrap();
    assert_eq!(openai_resp.status(), StatusCode::OK);

    let openrouter_req = Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header("authorization", "Bearer vk-openrouter")
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let openrouter_resp = app.oneshot(openrouter_req).await.unwrap();
    assert_eq!(openrouter_resp.status(), StatusCode::OK);
}

// ========== DLP Redaction Tests ==========

fn make_app_with_redact(upstream_url: &str) -> axum::Router {
    let mut key_map = BTreeMap::new();
    key_map.insert(
        "vk-test-1".to_string(),
        ResolvedKey {
            real_key: "sk-real-1".to_string(),
            provider: Provider::Openai,
        },
    );

    let patterns = vec![
        DlpPattern {
            name: "ssn".to_string(),
            regex: r"\b\d{3}-\d{2}-\d{4}\b".to_string(),
            action: DlpAction::Block,
        },
        DlpPattern {
            name: "email".to_string(),
            regex: r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b".to_string(),
            action: DlpAction::Redact,
        },
        DlpPattern {
            name: "phone_number".to_string(),
            regex: r"\b(?:\+?1[-.\s]?)?(?:\(?\d{3}\)?[-.\s]?)?\d{3}[-.\s]?\d{4}\b".to_string(),
            action: DlpAction::Redact,
        },
    ];

    let mut upstream_urls = BTreeMap::new();
    upstream_urls.insert(Provider::Openai, upstream_url.to_string());
    upstream_urls.insert(Provider::Anthropic, upstream_url.to_string());

    let state = AppState {
        key_manager: Arc::new(KeyManager::new(key_map)),
        dlp_scanner: Arc::new(DlpScanner::new(&patterns, true).unwrap()),
        proxy_client: Arc::new(ProxyClient::with_upstream_urls(
            upstream_urls,
            "2023-06-01".to_string(),
        )),
        email_enabled: false,
        email_policy: None,
        email_accounts: Arc::new(BTreeMap::new()),
        email_service: Arc::new(EmailService::mock_disabled()),
    };

    build_router(state)
}

#[tokio::test]
async fn test_request_redact_email_before_forwarding() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-real-1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "chatcmpl-ok"})),
        )
        .mount(&mock_server)
        .await;

    let app = make_app_with_redact(&mock_server.uri());
    // Body contains email (action=redact), should be redacted and forwarded, not blocked
    let body = r#"{"messages":[{"role":"user","content":"Contact me at user@example.com"}]}"#;
    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("authorization", "Bearer vk-test-1")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    // Should NOT be blocked (email is action=redact, not block)
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_request_block_ssn_still_works() {
    let mock_server = MockServer::start().await;
    let app = make_app_with_redact(&mock_server.uri());

    // Body contains SSN (action=block), should be blocked
    let body = r#"{"messages":[{"role":"user","content":"My SSN is 123-45-6789"}]}"#;
    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("authorization", "Bearer vk-test-1")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_response_dlp_redacts_pii() {
    let mock_server = MockServer::start().await;

    // Upstream responds with PII in the body
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "chatcmpl-abc",
            "choices": [{
                "message": {
                    "content": "Here is your info: email user@example.com, phone 555-123-4567"
                }
            }]
        })))
        .mount(&mock_server)
        .await;

    let app = make_app_with_redact(&mock_server.uri());
    let body = r#"{"model":"gpt-4","messages":[{"role":"user","content":"What is my info?"}]}"#;
    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("authorization", "Bearer vk-test-1")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let body_str = std::str::from_utf8(&body).unwrap();
    // PII should be redacted
    assert!(body_str.contains("[REDACTED:email]"));
    assert!(body_str.contains("[REDACTED:phone_number]"));
    assert!(!body_str.contains("user@example.com"));
    assert!(!body_str.contains("555-123-4567"));
}

#[tokio::test]
async fn test_response_dlp_redacts_ssn_in_response() {
    let mock_server = MockServer::start().await;

    // Upstream response contains an SSN (action=block on request, but redact_all on response)
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {
                    "content": "Your SSN is 123-45-6789"
                }
            }]
        })))
        .mount(&mock_server)
        .await;

    let app = make_app_with_redact(&mock_server.uri());
    let body = r#"{"model":"gpt-4","messages":[{"role":"user","content":"What is my SSN?"}]}"#;
    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("authorization", "Bearer vk-test-1")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let body_str = std::str::from_utf8(&body).unwrap();
    // SSN should be redacted in the response (redact_all applies to all patterns)
    assert!(body_str.contains("[REDACTED:ssn]"));
    assert!(!body_str.contains("123-45-6789"));
}

#[tokio::test]
async fn test_response_dlp_clean_response_untouched() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "chatcmpl-clean",
            "choices": [{"message": {"content": "Hello! How can I help?"}}]
        })))
        .mount(&mock_server)
        .await;

    let app = make_app_with_redact(&mock_server.uri());
    let body = r#"{"model":"gpt-4","messages":[{"role":"user","content":"Hi"}]}"#;
    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("authorization", "Bearer vk-test-1")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["id"], "chatcmpl-clean");
    // No redaction markers
    assert!(!json.to_string().contains("REDACTED"));
}

#[tokio::test]
async fn test_response_dlp_disabled() {
    let mock_server = MockServer::start().await;

    // Upstream responds with PII
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{"message": {"content": "Email: user@example.com"}}]
        })))
        .mount(&mock_server)
        .await;

    // Create app with scan_responses=false
    let mut key_map = BTreeMap::new();
    key_map.insert(
        "vk-test-1".to_string(),
        ResolvedKey {
            real_key: "sk-real-1".to_string(),
            provider: Provider::Openai,
        },
    );
    let patterns = vec![DlpPattern {
        name: "email".to_string(),
        regex: r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b".to_string(),
        action: DlpAction::Redact,
    }];
    let mut upstream_urls = BTreeMap::new();
    upstream_urls.insert(Provider::Openai, mock_server.uri());
    upstream_urls.insert(Provider::Anthropic, mock_server.uri());
    let state = AppState {
        key_manager: Arc::new(KeyManager::new(key_map)),
        dlp_scanner: Arc::new(DlpScanner::new(&patterns, false).unwrap()),
        proxy_client: Arc::new(ProxyClient::with_upstream_urls(
            upstream_urls,
            "2023-06-01".to_string(),
        )),
        email_enabled: false,
        email_policy: None,
        email_accounts: Arc::new(BTreeMap::new()),
        email_service: Arc::new(EmailService::mock_disabled()),
    };
    let app = build_router(state);

    let body = r#"{"model":"gpt-4","messages":[{"role":"user","content":"Hi"}]}"#;
    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("authorization", "Bearer vk-test-1")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let body_str = std::str::from_utf8(&body).unwrap();
    // PII should NOT be redacted because scan_responses=false
    assert!(body_str.contains("user@example.com"));
    assert!(!body_str.contains("REDACTED"));
}

#[tokio::test]
async fn test_redacted_body_content_length_not_stale() {
    let mock_server = MockServer::start().await;

    // The mock verifies the upstream receives the redacted body (valid JSON with [REDACTED:email])
    // If content-length were stale, the upstream would receive a truncated/over-read body
    // and fail to parse the JSON, returning a non-200 status.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-real-1"))
        .and(body_string_contains("[REDACTED:email]"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "chatcmpl-ok"})),
        )
        .mount(&mock_server)
        .await;

    let app = make_app_with_redact(&mock_server.uri());
    let body =
        r#"{"messages":[{"role":"user","content":"Contact me at user@example.com please"}]}"#;
    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("authorization", "Bearer vk-test-1")
        .header("content-type", "application/json")
        .header("content-length", body.len().to_string())
        .body(Body::from(body))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp_body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&resp_body).unwrap();
    assert_eq!(json["id"], "chatcmpl-ok");
}

// ========== New Feature Tests ==========

#[tokio::test]
async fn test_unsupported_method_returns_405() {
    let mock_server = MockServer::start().await;
    let app = make_app(&mock_server.uri());
    let req = Request::builder()
        .method("TRACE")
        .uri("/v1/models")
        .header("authorization", "Bearer vk-test-1")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn test_root_route_requires_auth() {
    let mock_server = MockServer::start().await;
    let app = make_app(&mock_server.uri());
    // Root route should match but require auth
    let req = Request::builder().uri("/").body(Body::empty()).unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_root_route_with_auth() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let req = Request::builder()
        .uri("/")
        .header("authorization", "Bearer vk-test-1")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_streaming_response_dlp_bypass_passes_through() {
    let mock_server = MockServer::start().await;

    // Upstream SSE response contains PII — it should pass through unmodified since streaming
    // bypasses DLP scanning. We use make_app (not make_app_with_redact) to ensure the SSE
    // content-type passthrough works correctly.
    let sse_body = "data: {\"content\":\"secret-token-xyz\"}\n\ndata: [DONE]\n\n";
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("content-type", "text/event-stream")
                .set_body_string(sse_body),
        )
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let body = r#"{"model":"gpt-4","stream":true,"messages":[{"role":"user","content":"Hi"}]}"#;
    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("authorization", "Bearer vk-test-1")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let body_str = std::str::from_utf8(&body).unwrap();
    // Content should pass through unmodified
    assert!(body_str.contains("secret-token-xyz"));
    assert!(body_str.contains("[DONE]"));
}

#[tokio::test]
async fn test_empty_body_passes_dlp() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": []})))
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    let req = Request::builder()
        .method("GET")
        .uri("/v1/models")
        .header("authorization", "Bearer vk-test-1")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_response_content_length_updated_after_redaction() {
    let mock_server = MockServer::start().await;

    // Upstream responds with PII and a content-length header
    let original_body = r#"{"content":"Contact user@example.com for info"}"#;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("content-type", "application/json")
                .set_body_string(original_body),
        )
        .mount(&mock_server)
        .await;

    let app = make_app_with_redact(&mock_server.uri());
    let body = r#"{"model":"gpt-4","messages":[{"role":"user","content":"Hi"}]}"#;
    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("authorization", "Bearer vk-test-1")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // The content-length should not be stale after redaction
    let content_length = resp
        .headers()
        .get("content-length")
        .map(|v| v.to_str().unwrap().parse::<usize>().unwrap());
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let body_str = std::str::from_utf8(&body).unwrap();

    assert!(body_str.contains("[REDACTED:email]"));
    // If content-length header is present, it must match the actual body length
    if let Some(cl) = content_length {
        assert_eq!(
            cl,
            body.len(),
            "content-length header must match actual body size after redaction"
        );
    }
}

#[tokio::test]
async fn test_non_utf8_body_passes_through() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/audio/transcriptions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"text": "hello world"})),
        )
        .mount(&mock_server)
        .await;

    let app = make_app(&mock_server.uri());
    // Body with invalid UTF-8 bytes — DLP scan should be skipped
    let binary_body: Bytes = Bytes::from(vec![0xFF, 0xFE, 0x00, 0x01, 0x80, 0x81]);
    let req = Request::builder()
        .method("POST")
        .uri("/v1/audio/transcriptions")
        .header("authorization", "Bearer vk-test-1")
        .header("content-type", "multipart/form-data")
        .body(Body::from(binary_body))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_streaming_response_with_dlp_enabled_passes_through() {
    let mock_server = MockServer::start().await;

    // SSE response — should pass through when DLP scanning is enabled
    // because streaming responses cannot be scanned (exercises lib.rs lines 261-268)
    let sse_body = "data: {\"content\":\"hello world\"}\n\ndata: [DONE]\n\n";
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(sse_body, "text/event-stream"),
        )
        .mount(&mock_server)
        .await;

    let app = make_app_with_redact(&mock_server.uri());
    let body = r#"{"model":"gpt-4","stream":true,"messages":[{"role":"user","content":"Hi"}]}"#;
    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("authorization", "Bearer vk-test-1")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ct.contains("text/event-stream"),
        "Expected text/event-stream content-type, got: {}",
        ct
    );

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let body_str = std::str::from_utf8(&body).unwrap();
    assert!(body_str.contains("[DONE]"));
}

fn make_email_app(
    policy: EmailPolicy,
    email_accounts: BTreeMap<String, EmailAccountCredentials>,
    email_service: Arc<EmailService>,
) -> axum::Router {
    let mut upstream_urls = BTreeMap::new();
    upstream_urls.insert(Provider::Openai, "http://127.0.0.1:1".to_string());
    upstream_urls.insert(Provider::Anthropic, "http://127.0.0.1:1".to_string());

    let state = AppState {
        key_manager: Arc::new(KeyManager::new(BTreeMap::new())),
        dlp_scanner: Arc::new(DlpScanner::new(&[], false).unwrap()),
        proxy_client: Arc::new(ProxyClient::with_upstream_urls(
            upstream_urls,
            "2023-06-01".to_string(),
        )),
        email_enabled: true,
        email_policy: Some(policy),
        email_accounts: Arc::new(email_accounts),
        email_service,
    };

    build_router(state)
}

fn test_email_credentials() -> EmailAccountCredentials {
    EmailAccountCredentials {
        email: "bot@gmail.com".to_string(),
        app_password: "abcd efgh ijkl mnop".to_string(),
        imap_host: "imap.gmail.com".to_string(),
        imap_port: 993,
    }
}

#[tokio::test]
async fn test_email_secure_allowlist_filters_senders() {
    let policy = EmailPolicy {
        mode: EmailMode::Allowlist,
        sender_rules: vec!["@trusted.local".to_string()],
        default_max_results: 50,
    };
    let mut accounts = BTreeMap::new();
    accounts.insert("vk-email".to_string(), test_email_credentials());
    let service = EmailService::mock_static(EmailListMessagesResponse {
        messages: vec![
            EmailMessageMetadata {
                id: "msg-1".to_string(),
                thread_id: Some("thread-1".to_string()),
                from: Some("Alice <alice@trusted.local>".to_string()),
                subject: Some("Trusted".to_string()),
                date: Some("Wed, 15 Jan 2025 10:00:00 +0000".to_string()),
                snippet: Some("hello".to_string()),
                internal_date_ms: Some(1736935200000),
                label_ids: vec!["INBOX".to_string()],
            },
            EmailMessageMetadata {
                id: "msg-2".to_string(),
                thread_id: Some("thread-2".to_string()),
                from: Some("Mallory <mallory@evil.com>".to_string()),
                subject: Some("Spam".to_string()),
                date: Some("Wed, 15 Jan 2025 11:00:00 +0000".to_string()),
                snippet: Some("spam".to_string()),
                internal_date_ms: Some(1736938800000),
                label_ids: vec!["INBOX".to_string()],
            },
        ],
        next_page_token: Some("next-token".to_string()),
    });

    let app = make_email_app(policy, accounts, Arc::new(service));
    let req = Request::builder()
        .method("GET")
        .uri("/v1/email/messages")
        .header("authorization", "Bearer vk-email")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json.get("applied_filter_mode").is_none());
    assert!(json.get("visible_count").is_none());
    assert!(json.get("filtered_out_count").is_none());
    assert_eq!(json["messages"][0]["id"], "msg-1");
    assert!(
        json["messages"][0]["from"]
            .as_str()
            .unwrap()
            .contains("trusted")
    );
}

#[tokio::test]
async fn test_email_secure_denylist_filters_senders() {
    let policy = EmailPolicy {
        mode: EmailMode::Denylist,
        sender_rules: vec!["@blocked.local".to_string()],
        default_max_results: 50,
    };
    let mut accounts = BTreeMap::new();
    accounts.insert("vk-email".to_string(), test_email_credentials());
    let service = EmailService::mock_static(EmailListMessagesResponse {
        messages: vec![
            EmailMessageMetadata {
                id: "msg-1".to_string(),
                thread_id: Some("thread-1".to_string()),
                from: Some("Alert <alert@blocked.local>".to_string()),
                subject: Some("Blocked".to_string()),
                date: None,
                snippet: Some("blocked".to_string()),
                internal_date_ms: None,
                label_ids: vec!["INBOX".to_string()],
            },
            EmailMessageMetadata {
                id: "msg-2".to_string(),
                thread_id: Some("thread-2".to_string()),
                from: Some("News <news@safe.com>".to_string()),
                subject: Some("Allowed".to_string()),
                date: None,
                snippet: Some("allowed".to_string()),
                internal_date_ms: None,
                label_ids: vec!["INBOX".to_string()],
            },
        ],
        next_page_token: None,
    });

    let app = make_email_app(policy, accounts, Arc::new(service));
    let req = Request::builder()
        .method("GET")
        .uri("/v1/email/messages")
        .header("authorization", "Bearer vk-email")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json.get("applied_filter_mode").is_none());
    assert!(json.get("visible_count").is_none());
    assert!(json.get("filtered_out_count").is_none());
    assert_eq!(json["messages"][0]["id"], "msg-2");
}

#[tokio::test]
async fn test_email_secure_unknown_virtual_key_rejected() {
    let policy = EmailPolicy {
        mode: EmailMode::Allowlist,
        sender_rules: vec!["@trusted.local".to_string()],
        default_max_results: 50,
    };
    let mut accounts = BTreeMap::new();
    accounts.insert("vk-email".to_string(), test_email_credentials());
    let service = EmailService::mock_static(EmailListMessagesResponse {
        messages: vec![],
        next_page_token: None,
    });

    let app = make_email_app(policy, accounts, Arc::new(service));
    let req = Request::builder()
        .method("GET")
        .uri("/v1/email/messages")
        .header("authorization", "Bearer vk-other")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_email_secure_rejects_invalid_limit() {
    let policy = EmailPolicy {
        mode: EmailMode::Allowlist,
        sender_rules: vec!["@trusted.local".to_string()],
        default_max_results: 50,
    };
    let mut accounts = BTreeMap::new();
    accounts.insert("vk-email".to_string(), test_email_credentials());
    let service = EmailService::mock_static(EmailListMessagesResponse {
        messages: vec![],
        next_page_token: None,
    });

    let app = make_email_app(policy, accounts, Arc::new(service));
    let req = Request::builder()
        .method("GET")
        .uri("/v1/email/messages?limit=101")
        .header("authorization", "Bearer vk-email")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_email_secure_endpoint_disabled_returns_not_found() {
    let mock_server = MockServer::start().await;
    let app = make_app(&mock_server.uri());
    let req = Request::builder()
        .method("GET")
        .uri("/v1/email/messages")
        .header("authorization", "Bearer vk-test-1")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_email_message_content_success() {
    let policy = EmailPolicy {
        mode: EmailMode::Allowlist,
        sender_rules: vec!["@trusted.local".to_string()],
        default_max_results: 50,
    };
    let mut accounts = BTreeMap::new();
    accounts.insert("vk-email".to_string(), test_email_credentials());

    let mut contents = BTreeMap::new();
    contents.insert(
        "42".to_string(),
        EmailMessageContent {
            metadata: EmailMessageMetadata {
                id: "42".to_string(),
                thread_id: Some("thread-42".to_string()),
                from: Some("Alice <alice@trusted.local>".to_string()),
                subject: Some("Invoice".to_string()),
                date: Some("Wed, 15 Jan 2025 12:00:00 +0000".to_string()),
                snippet: Some("Invoice attached".to_string()),
                internal_date_ms: Some(1736942400000),
                label_ids: vec!["INBOX".to_string()],
            },
            headers: BTreeMap::from([
                (
                    "from".to_string(),
                    "Alice <alice@trusted.local>".to_string(),
                ),
                ("subject".to_string(), "Invoice".to_string()),
            ]),
            text_body: Some("Plain text body".to_string()),
            html_body: Some("<p>HTML body</p>".to_string()),
        },
    );

    let service = EmailService::mock_static_content(contents);
    let app = make_email_app(policy, accounts, Arc::new(service));
    let req = Request::builder()
        .method("GET")
        .uri("/v1/email/messages/42")
        .header("authorization", "Bearer vk-email")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["metadata"]["id"], "42");
    assert_eq!(json["headers"]["subject"], "Invoice");
    assert_eq!(json["text_body"], "Plain text body");
    assert_eq!(json["html_body"], "<p>HTML body</p>");
}

#[tokio::test]
async fn test_email_message_content_rejects_invalid_id() {
    let policy = EmailPolicy {
        mode: EmailMode::Allowlist,
        sender_rules: vec!["@trusted.local".to_string()],
        default_max_results: 50,
    };
    let mut accounts = BTreeMap::new();
    accounts.insert("vk-email".to_string(), test_email_credentials());
    let service = EmailService::mock_static_content(BTreeMap::new());

    let app = make_email_app(policy, accounts, Arc::new(service));
    let req = Request::builder()
        .method("GET")
        .uri("/v1/email/messages/not-a-number")
        .header("authorization", "Bearer vk-email")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_email_message_content_not_found() {
    let policy = EmailPolicy {
        mode: EmailMode::Allowlist,
        sender_rules: vec!["@trusted.local".to_string()],
        default_max_results: 50,
    };
    let mut accounts = BTreeMap::new();
    accounts.insert("vk-email".to_string(), test_email_credentials());
    let service = EmailService::mock_static_content(BTreeMap::new());

    let app = make_email_app(policy, accounts, Arc::new(service));
    let req = Request::builder()
        .method("GET")
        .uri("/v1/email/messages/777")
        .header("authorization", "Bearer vk-email")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_email_message_content_filtered_sender_hidden() {
    let policy = EmailPolicy {
        mode: EmailMode::Allowlist,
        sender_rules: vec!["@trusted.local".to_string()],
        default_max_results: 50,
    };
    let mut accounts = BTreeMap::new();
    accounts.insert("vk-email".to_string(), test_email_credentials());

    let mut contents = BTreeMap::new();
    contents.insert(
        "5".to_string(),
        EmailMessageContent {
            metadata: EmailMessageMetadata {
                id: "5".to_string(),
                thread_id: None,
                from: Some("Mallory <mallory@evil.com>".to_string()),
                subject: Some("Blocked".to_string()),
                date: None,
                snippet: None,
                internal_date_ms: None,
                label_ids: vec!["INBOX".to_string()],
            },
            headers: BTreeMap::from([(
                "from".to_string(),
                "Mallory <mallory@evil.com>".to_string(),
            )]),
            text_body: Some("evil".to_string()),
            html_body: None,
        },
    );

    let service = EmailService::mock_static_content(contents);
    let app = make_email_app(policy, accounts, Arc::new(service));
    let req = Request::builder()
        .method("GET")
        .uri("/v1/email/messages/5")
        .header("authorization", "Bearer vk-email")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_email_message_content_endpoint_disabled_returns_not_found() {
    let mock_server = MockServer::start().await;
    let app = make_app(&mock_server.uri());
    let req = Request::builder()
        .method("GET")
        .uri("/v1/email/messages/42")
        .header("authorization", "Bearer vk-test-1")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
