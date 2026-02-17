use crate::config::{Config, GmailAccountConfig, Provider};
use crate::dlp::DlpScanner;
use crate::gmail::{
    GmailAccountCredentials, GmailListMessagesRequest, GmailPolicy, GmailRefreshCredentials,
    GmailService, GoogleGmailService, normalize_sender_rule,
};
use crate::keys::{KeyManager, ResolvedKey};
use crate::proxy::ProxyClient;

use axum::Router;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Query, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use bytes::Bytes;
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, error, info, trace, warn};

#[derive(Debug, Clone)]
pub struct AppState {
    pub key_manager: Arc<KeyManager>,
    pub dlp_scanner: Arc<DlpScanner>,
    pub proxy_client: Arc<ProxyClient>,
    pub gmail_enabled: bool,
    pub gmail_policy: Option<GmailPolicy>,
    pub gmail_accounts: Arc<BTreeMap<String, GmailAccountCredentials>>,
    pub gmail_service: Arc<GmailService>,
}

impl AppState {
    pub fn from_config(config: &Config, config_path: Option<&Path>) -> Result<Self, String> {
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

        let gmail_policy = config.gmail.mode.map(|mode| {
            let sender_rules = match mode {
                crate::config::GmailMode::Allowlist => &config.gmail.allow_senders,
                crate::config::GmailMode::Denylist => &config.gmail.deny_senders,
            }
            .iter()
            .map(|rule| normalize_sender_rule(rule))
            .collect();

            GmailPolicy {
                mode,
                sender_rules,
                default_max_results: config.gmail.default_max_results,
            }
        });

        let gmail_accounts: BTreeMap<String, GmailAccountCredentials> = config
            .gmail
            .accounts
            .iter()
            .map(|account| {
                let refresh = resolve_gmail_refresh_credentials(account, config_path)
                    .map_err(|e| format!("gmail account '{}': {e}", account.virtual_key))?;
                Ok((
                    account.virtual_key.clone(),
                    GmailAccountCredentials {
                        refresh,
                        user_id: account.user_id.clone(),
                    },
                ))
            })
            .collect::<Result<_, String>>()?;

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
            gmail_enabled: config.gmail.enabled,
            gmail_policy,
            gmail_accounts: Arc::new(gmail_accounts),
            gmail_service: Arc::new(GmailService::Google(GoogleGmailService::new(
                config.gmail.api_base_url.clone(),
            ))),
        })
    }
}

#[derive(Debug, Deserialize)]
struct GoogleClientSecretFile {
    #[serde(default)]
    installed: Option<GoogleClientSecretEntry>,
    #[serde(default)]
    web: Option<GoogleClientSecretEntry>,
}

#[derive(Debug, Deserialize)]
struct GoogleClientSecretEntry {
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    client_secret: Option<String>,
    #[serde(default)]
    token_uri: Option<String>,
}

fn resolve_gmail_refresh_credentials(
    account: &GmailAccountConfig,
    config_path: Option<&Path>,
) -> Result<GmailRefreshCredentials, String> {
    let refresh_token = account
        .refresh_token
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("missing refresh_token")?
        .to_string();

    let client_secret_file = account
        .client_secret_file
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("missing client_secret_file")?;
    let resolved_path = resolve_client_secret_file_path(client_secret_file, config_path);
    let content = std::fs::read_to_string(&resolved_path).map_err(|e| {
        format!(
            "failed to read client_secret_file '{}': {e}",
            resolved_path.display()
        )
    })?;
    let parsed: GoogleClientSecretFile = serde_json::from_str(&content).map_err(|e| {
        format!(
            "failed to parse client_secret_file '{}': {e}",
            resolved_path.display()
        )
    })?;
    let oauth = parsed
        .installed
        .or(parsed.web)
        .ok_or("client_secret_file must contain either an 'installed' or 'web' object")?;
    let client_id = oauth
        .client_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("client_secret_file is missing non-empty client_id")?
        .to_string();
    let token_uri = oauth
        .token_uri
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("https://oauth2.googleapis.com/token")
        .to_string();

    Ok(GmailRefreshCredentials {
        token_uri,
        refresh_token,
        client_id,
        client_secret: oauth.client_secret.unwrap_or_default().trim().to_string(),
    })
}

fn resolve_client_secret_file_path(
    client_secret_file: &str,
    config_path: Option<&Path>,
) -> PathBuf {
    let path = PathBuf::from(client_secret_file);
    if path.is_absolute() {
        return path;
    }
    if let Some(base) = config_path.and_then(Path::parent) {
        return base.join(path);
    }
    path
}

/// Maximum request body size (10 MiB).
const MAX_BODY_SIZE: usize = 10 * 1024 * 1024;

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/v1/gmail/messages", get(handle_gmail_secure_messages))
        .route("/", any(handle_request))
        .route("/{*path}", any(handle_request))
        .layer(DefaultBodyLimit::max(MAX_BODY_SIZE))
        .with_state(state)
}

#[derive(Debug, Deserialize)]
struct GmailSecureMessagesQuery {
    q: Option<String>,
    max_results: Option<u32>,
    page_token: Option<String>,
    include_spam_trash: Option<bool>,
}

#[derive(Debug, Serialize)]
struct GmailSecureMessage {
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
struct GmailSecureMessagesResponse {
    messages: Vec<GmailSecureMessage>,
    next_page_token: Option<String>,
}

async fn handle_gmail_secure_messages(
    State(state): State<AppState>,
    Query(query): Query<GmailSecureMessagesQuery>,
    headers: axum::http::HeaderMap,
) -> Result<Response, Response> {
    let method = axum::http::Method::GET;
    let path = "/v1/gmail/messages";

    if !state.gmail_enabled {
        warn!(method = %method, path = %path, "Gmail endpoint is disabled");
        return Err(error_response(
            StatusCode::NOT_FOUND,
            "Gmail secure endpoint is disabled",
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

    let account = state.gmail_accounts.get(&virtual_key).ok_or_else(|| {
        warn!(
            method = %method,
            path = %path,
            virtual_key = %virtual_key,
            "Virtual key is not authorized for Gmail"
        );
        error_response(StatusCode::UNAUTHORIZED, "Unknown API key")
    })?;

    let policy = state.gmail_policy.as_ref().ok_or_else(|| {
        error!(
            method = %method,
            path = %path,
            "Gmail endpoint enabled without an active sender policy"
        );
        error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Gmail policy configuration error",
        )
    })?;

    let max_results = query.max_results.unwrap_or(policy.default_max_results);
    if max_results == 0 || max_results > 100 {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "max_results must be between 1 and 100",
        ));
    }

    let service_request = GmailListMessagesRequest {
        q: query.q.clone(),
        max_results,
        page_token: query.page_token.clone(),
        include_spam_trash: query.include_spam_trash.unwrap_or(false),
    };

    let gmail_response = state
        .gmail_service
        .list_message_metadata(&virtual_key, account, &service_request)
        .await
        .map_err(|e| {
            error!(
                method = %method,
                path = %path,
                virtual_key = %virtual_key,
                error = %e,
                "Failed to fetch Gmail messages"
            );
            error_response(StatusCode::BAD_GATEWAY, "Failed to fetch Gmail messages")
        })?;

    let mut visible_messages = Vec::new();
    for message in gmail_response.messages {
        let Some(from_header) = message.from.as_deref() else {
            continue;
        };
        if !policy.sender_visible(from_header) {
            continue;
        }
        visible_messages.push(GmailSecureMessage {
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

    let response = GmailSecureMessagesResponse {
        messages: visible_messages,
        next_page_token: gmail_response.next_page_token,
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
