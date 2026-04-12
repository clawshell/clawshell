use crate::config::{Config, KeyAuthMethod, Provider};
use crate::dlp::DlpScanner;
use crate::email::{
    EmailAccountCredentials, EmailGetMessageRequest, EmailListMessagesRequest, EmailMessageContent,
    EmailMessageMetadata, EmailPolicy, EmailService, EmailServiceError, ImapEmailService,
    normalize_sender_rule,
};
use crate::keys::{KeyManager, KeySource, ResolvedKey};
use crate::oauth::OAuthRegistry;
use crate::proxy::ProxyClient;
use crate::stats::Stats;

use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use bytes::Bytes;
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, error, info, trace, warn};

#[derive(Debug, Clone)]
pub struct AppState {
    pub key_manager: Arc<KeyManager>,
    pub dlp_scanner: Arc<DlpScanner>,
    pub proxy_client: Arc<ProxyClient>,
    pub oauth_registry: Arc<OAuthRegistry>,
    pub email_enabled: bool,
    pub email_policy: Option<EmailPolicy>,
    pub email_accounts: Arc<BTreeMap<String, EmailAccountCredentials>>,
    pub email_service: Arc<EmailService>,
    pub stats: Arc<Stats>,
}

impl AppState {
    #[allow(dead_code)]
    pub fn from_config(config: &Config) -> Result<Self, String> {
        Self::from_config_with_registry(config, None)
    }

    pub fn from_config_with_registry(
        config: &Config,
        oauth_registry: Option<OAuthRegistry>,
    ) -> Result<Self, String> {
        let mut upstream_urls = BTreeMap::new();
        upstream_urls.insert(Provider::Openai, config.upstream_url(Provider::Openai));
        upstream_urls.insert(
            Provider::Openrouter,
            config.upstream_url(Provider::Openrouter),
        );
        upstream_urls.insert(
            Provider::Anthropic,
            config.upstream_url(Provider::Anthropic),
        );
        upstream_urls.insert(Provider::Minimax, config.upstream_url(Provider::Minimax));

        // Build key mappings for both static and OAuth keys
        let mut key_mappings: BTreeMap<String, ResolvedKey> = BTreeMap::new();

        for key in &config.keys {
            let source = match key.auth {
                KeyAuthMethod::Static => KeySource::Static {
                    real_key: key.real_key.clone().unwrap_or_default(),
                },
                KeyAuthMethod::OAuth => KeySource::OAuth {
                    provider_id: key.oauth_provider.clone().unwrap_or_default(),
                },
            };
            key_mappings.insert(
                key.virtual_key.clone(),
                ResolvedKey {
                    source,
                    provider: key.provider,
                },
            );
        }

        let oauth_registry =
            oauth_registry.unwrap_or_else(|| OAuthRegistry::new(Default::default()));

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
            oauth_registry: Arc::new(oauth_registry),
            email_enabled: config.email.enabled,
            email_policy,
            email_accounts: Arc::new(email_accounts),
            email_service: Arc::new(EmailService::Imap(ImapEmailService::default())),
            stats: Arc::new(Stats::new(Some(config.stats.persist_path.clone()))),
        })
    }
}

/// Maximum request body size (10 MiB).
const MAX_BODY_SIZE: usize = 10 * 1024 * 1024;

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/v1/email/messages", get(handle_email_secure_messages))
        .route("/v1/email/messages/{id}", get(handle_email_message_content))
        .route("/admin/stats", get(handle_stats))
        .route("/", any(handle_request))
        .route("/{*path}", any(handle_request))
        .layer(DefaultBodyLimit::max(MAX_BODY_SIZE))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            log_request_completion,
        ))
        .with_state(state)
}

async fn log_request_completion(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let start = Instant::now();
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let query = request.uri().query().map(|q| q.to_string());
    let response = next.run(request).await;

    state.stats.record_request();

    info!(
        method = %method,
        path = %path,
        query = %query.as_deref().unwrap_or(""),
        status = %response.status(),
        latency_ms = start.elapsed().as_millis(),
        "Request completed"
    );

    response
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
            state.stats.record_email_filtered(from_header);
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

    info!(
        method = %method,
        path = %path,
        query = ?query,
        virtual_key = %virtual_key,
        "fetched messages metadata"
    );

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
        if let Some(from) = from_header {
            state.stats.record_email_filtered(from);
        }
        return Err(error_response(
            StatusCode::NOT_FOUND,
            "Email message not found",
        ));
    }

    info!(
        virtual_key = %virtual_key,
        message_id = %message_id,
        "Fetched message content"
    );

    let response = EmailMessageContentResponse {
        metadata: content.metadata,
        headers: content.headers,
        text_body: content.text_body,
        html_body: content.html_body,
    };

    Ok(axum::Json(response).into_response())
}

/// Management endpoint that returns running counters for total requests,
/// upstream token usage, and per-sender email-filter activity. Only
/// reachable from loopback peers; streaming (SSE) responses are not
/// included in token totals (see `handle_request`).
async fn handle_stats(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> Result<Response, Response> {
    if !peer.ip().is_loopback() {
        warn!(
            peer = %peer,
            "Non-loopback client tried to hit /admin/stats"
        );
        return Err(error_response(
            StatusCode::FORBIDDEN,
            "stats endpoint is loopback-only",
        ));
    }
    Ok(axum::Json(state.stats.snapshot()).into_response())
}

async fn handle_request(
    State(state): State<AppState>,
    request: Request,
) -> Result<Response, Response> {
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
    let source = resolved.source.clone();
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

    // 3b. Try injecting stream_options so the provider returns token usage
    // in SSE chunks. If the upstream rejects it (400 "unknown_parameter"),
    // we retry without it in step 4b.
    let (body_bytes, original_body) = match ensure_stream_options(&body_bytes, provider) {
        Some(patched) => (Bytes::from(patched), Some(body_bytes)),
        None => (body_bytes, None),
    };
    let retry_headers = if original_body.is_some() {
        Some(headers.clone())
    } else {
        None
    };
    let retry_source = if original_body.is_some() {
        Some(source.clone())
    } else {
        None
    };

    // 4. Forward to upstream
    info!(
        method = %method,
        path = %path,
        virtual_key = %virtual_key,
        "Forwarding request to upstream"
    );

    let response = match source {
        KeySource::Static { real_key } => state
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
            })?,
        KeySource::OAuth { provider_id } => forward_oauth_request(
            &state,
            method.clone(),
            &uri,
            headers,
            body_bytes,
            provider,
            &provider_id,
        )
        .await
        .map_err(|e| {
            error!(
                method = %method,
                path = %path,
                virtual_key = %virtual_key,
                oauth_provider = %provider_id,
                error = %e,
                "OAuth proxy error"
            );
            error_response(StatusCode::BAD_GATEWAY, &format!("OAuth proxy error: {e}"))
        })?,
    };

    // 4b. If we injected stream_options and the upstream returned 400 with
    // "unknown_parameter", the model doesn't support it — retry without.
    let response = if let (Some(original_body), Some(retry_headers), Some(retry_source)) =
        (original_body, retry_headers, retry_source)
    {
        if response.status() == StatusCode::BAD_REQUEST {
            let (parts, err_body) = response.into_parts();
            let err_bytes = err_body
                .collect()
                .await
                .map(|b| b.to_bytes())
                .unwrap_or_default();
            if String::from_utf8_lossy(&err_bytes).contains("unknown_parameter") {
                debug!(
                    method = %method,
                    path = %path,
                    "Upstream rejected stream_options; retrying without it"
                );
                match retry_source {
                    KeySource::Static { real_key } => state
                        .proxy_client
                        .forward(
                            method.clone(),
                            &uri,
                            retry_headers,
                            &real_key,
                            original_body,
                            provider,
                        )
                        .await
                        .map_err(|e| {
                            error!(error = %e, "Proxy error on stream_options retry");
                            e.into_response()
                        })?,
                    KeySource::OAuth { provider_id } => forward_oauth_request(
                        &state,
                        method.clone(),
                        &uri,
                        retry_headers,
                        original_body,
                        provider,
                        &provider_id,
                    )
                    .await
                    .map_err(|e| {
                        error!(error = %e, "OAuth proxy error on stream_options retry");
                        error_response(StatusCode::BAD_GATEWAY, &format!("OAuth proxy error: {e}"))
                    })?,
                }
            } else {
                Response::from_parts(parts, Body::from(err_bytes))
            }
        } else {
            response
        }
    } else {
        response
    };

    // 5. Response processing.
    // Non-streaming responses are buffered so we can (a) record upstream
    // token usage for stats and (b) optionally DLP-redact before sending.
    // SSE streams are handled by the existing DLP SSE wrapper; we do not
    // count tokens from SSE usage events.
    let is_streaming = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.contains("text/event-stream"));

    let response = if is_streaming {
        trace!(
            method = %method,
            path = %path,
            "Streaming response (SSE) — wrapping for DLP + token counting"
        );
        let (parts, body) = response.into_parts();
        let dlp_body = crate::translate::wrap_body_with_dlp_sse_stream(
            body,
            state.dlp_scanner.clone(),
            state.stats.clone(),
        );
        Response::from_parts(parts, dlp_body)
    } else {
        let (parts, body) = response.into_parts();
        let body_bytes = body
            .collect()
            .await
            .map_err(|e| {
                error!(error = %e, "Failed to read response body");
                error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to process response",
                )
            })?
            .to_bytes();

        // Count tokens from the upstream `usage` block before any DLP mutation.
        state.stats.record_tokens_from_usage(&body_bytes);

        if state.dlp_scanner.scan_responses() {
            let (redacted, redacted_names) = state.dlp_scanner.redact_all(&body_bytes);
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
                Response::from_parts(parts, Body::from(body_bytes))
            }
        } else {
            Response::from_parts(parts, Body::from(body_bytes))
        }
    };

    Ok(response)
}

async fn forward_oauth_request(
    state: &AppState,
    method: Method,
    uri: &Uri,
    headers: HeaderMap,
    body_bytes: Bytes,
    provider: Provider,
    oauth_provider_id: &str,
) -> Result<Response, String> {
    // 1. Inject auth headers
    let mut auth_headers = HeaderMap::new();
    state
        .oauth_registry
        .inject_auth(oauth_provider_id, &mut auth_headers)
        .await
        .map_err(|e| format!("OAuth auth injection failed: {e}"))?;

    // 2. Optionally transform the body
    let body = match state
        .oauth_registry
        .prepare_request_body(oauth_provider_id, &body_bytes)
        .await
        .map_err(|e| format!("OAuth body preparation failed: {e}"))?
    {
        Some(transformed) => Bytes::from(transformed),
        None => body_bytes.clone(),
    };

    // 2b. Check if path needs rewriting (e.g., /v1/chat/completions → /v1/responses)
    let original_path = uri.path().to_string();
    let rewritten_path = state
        .oauth_registry
        .rewrite_request_path(oauth_provider_id, &original_path)
        .map_err(|e| format!("OAuth path rewrite failed: {e}"))?;
    let needs_translation = state
        .oauth_registry
        .needs_response_translation(oauth_provider_id, &original_path)
        .map_err(|e| format!("OAuth translation check failed: {e}"))?;
    let response_format = state
        .oauth_registry
        .response_format(oauth_provider_id, &original_path)
        .map_err(|e| format!("OAuth response format check failed: {e}"))?;
    // Check the transformed body for stream flag (fixups may force stream: true)
    let stream_requested = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("stream")?.as_bool())
        .unwrap_or(false);

    let effective_uri = if let Some(ref new_path) = rewritten_path {
        build_rewritten_uri(uri, new_path)?
    } else {
        uri.clone()
    };

    if rewritten_path.is_some() {
        debug!(
            oauth_provider = %oauth_provider_id,
            original_path = %original_path,
            effective_path = %effective_uri.path(),
            "Rewrote request path for OAuth provider"
        );
    }

    // 3. Optionally get upstream URL override
    let upstream_url = state
        .oauth_registry
        .upstream_url(oauth_provider_id)
        .await
        .map_err(|e| format!("OAuth upstream URL resolution failed: {e}"))?;

    // 4. Forward the request
    let response = state
        .proxy_client
        .forward_oauth(
            method.clone(),
            &effective_uri,
            headers.clone(),
            body.clone(),
            provider,
            auth_headers.clone(),
            upstream_url.as_deref(),
        )
        .await
        .map_err(|e| format!("OAuth forward failed: {e}"))?;

    // 5. If we got a 401, refresh the token and retry once
    if response.status() == StatusCode::UNAUTHORIZED {
        info!(
            oauth_provider = %oauth_provider_id,
            effective_path = %effective_uri.path(),
            "Got 401 from upstream, attempting token refresh and retry"
        );
        if let Err(e) = state.oauth_registry.refresh(oauth_provider_id).await {
            warn!(
                oauth_provider = %oauth_provider_id,
                error = %e,
                "Token refresh failed after 401"
            );
            return maybe_translate_response(
                response,
                needs_translation,
                stream_requested,
                response_format,
            )
            .await;
        }

        // Re-inject auth with refreshed token
        let mut retry_auth_headers = HeaderMap::new();
        state
            .oauth_registry
            .inject_auth(oauth_provider_id, &mut retry_auth_headers)
            .await
            .map_err(|e| format!("OAuth retry auth injection failed: {e}"))?;

        // Optionally re-transform the body (tokens may have changed affecting body)
        let retry_body = match state
            .oauth_registry
            .prepare_request_body(oauth_provider_id, &body_bytes)
            .await
            .map_err(|e| format!("OAuth retry body preparation failed: {e}"))?
        {
            Some(transformed) => Bytes::from(transformed),
            None => body_bytes,
        };

        let retry_response = state
            .proxy_client
            .forward_oauth(
                method,
                &effective_uri,
                headers,
                retry_body,
                provider,
                retry_auth_headers,
                upstream_url.as_deref(),
            )
            .await
            .map_err(|e| format!("OAuth retry forward failed: {e}"))?;

        if retry_response.status() == StatusCode::UNAUTHORIZED {
            warn!(
                oauth_provider = %oauth_provider_id,
                effective_path = %effective_uri.path(),
                "Retry after token refresh still returned 401"
            );
        }

        return maybe_translate_response(
            retry_response,
            needs_translation,
            stream_requested,
            response_format,
        )
        .await;
    }

    // Log error response bodies for debugging upstream issues
    if response.status().is_client_error() || response.status().is_server_error() {
        let status = response.status();
        let (parts, body) = response.into_parts();
        let body_bytes_resp = body
            .collect()
            .await
            .map(|b| b.to_bytes())
            .unwrap_or_default();
        if let Ok(body_str) = std::str::from_utf8(&body_bytes_resp) {
            warn!(
                oauth_provider = %oauth_provider_id,
                effective_path = %effective_uri.path(),
                status = %status,
                response_body = %body_str,
                "Upstream returned error"
            );
        }
        let response = Response::from_parts(parts, Body::from(body_bytes_resp));
        return maybe_translate_response(
            response,
            needs_translation,
            stream_requested,
            response_format,
        )
        .await;
    }

    maybe_translate_response(
        response,
        needs_translation,
        stream_requested,
        response_format,
    )
    .await
}

/// Optionally translate an upstream response back to chat/completions format.
async fn maybe_translate_response(
    response: Response,
    needs_translation: bool,
    stream_requested: bool,
    response_format: Option<crate::oauth::ResponseFormat>,
) -> Result<Response, String> {
    // Use response_format if available; fall back to needs_translation for backwards compat
    let format = match response_format {
        Some(f) => f,
        None if needs_translation => crate::oauth::ResponseFormat::ResponsesApi,
        None => return Ok(response),
    };

    let is_streaming = stream_requested
        || response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.contains("text/event-stream"));

    debug!(format = ?format, is_streaming, "maybe_translate_response: translating response");

    if is_streaming {
        let (parts, body) = response.into_parts();
        let translated_body = match format {
            crate::oauth::ResponseFormat::ResponsesApi => {
                debug!("Wrapping streaming response with ResponsesApi translator");
                crate::translate::wrap_body_with_translate_stream(body)
            }
        };
        return Ok(Response::from_parts(parts, translated_body));
    }

    // Non-streaming: only translate successful responses
    let status = response.status();
    if !status.is_success() {
        return Ok(response);
    }

    let (mut parts, body) = response.into_parts();
    let body_bytes = body
        .collect()
        .await
        .map_err(|e| format!("failed to read response body for translation: {e}"))?
        .to_bytes();

    match format {
        crate::oauth::ResponseFormat::ResponsesApi => {
            match crate::translate::responses_to_chat_completion(&body_bytes) {
                Ok(translated) => {
                    parts.headers.remove("content-length");
                    Ok(Response::from_parts(parts, Body::from(translated)))
                }
                Err(e) => {
                    warn!(error = %e, "Response translation failed, returning original");
                    Ok(Response::from_parts(parts, Body::from(body_bytes)))
                }
            }
        }
    }
}

/// Build a new URI with a rewritten path, preserving query string.
/// Incoming axum URIs are path-only (no scheme/authority), so we build path-only too.
fn build_rewritten_uri(original: &Uri, new_path: &str) -> Result<Uri, String> {
    let path_and_query = if let Some(query) = original.query() {
        format!("{new_path}?{query}")
    } else {
        new_path.to_string()
    };
    path_and_query
        .parse::<Uri>()
        .map_err(|e| format!("failed to build rewritten URI: {e}"))
}

fn ensure_stream_options(body: &[u8], provider: Provider) -> Option<Vec<u8>> {
    if !matches!(provider, Provider::Openai | Provider::Openrouter) {
        return None;
    }
    let mut json: serde_json::Value = serde_json::from_slice(body).ok()?;
    let obj = json.as_object_mut()?;
    if obj.get("stream").and_then(serde_json::Value::as_bool) != Some(true) {
        return None;
    }
    let already_set = obj
        .get("stream_options")
        .and_then(serde_json::Value::as_object)
        .and_then(|so| so.get("include_usage"))
        .and_then(serde_json::Value::as_bool)
        == Some(true);
    if already_set {
        return None;
    }
    obj.insert(
        "stream_options".to_string(),
        serde_json::json!({"include_usage": true}),
    );
    serde_json::to_vec(&json).ok()
}

fn error_response(status: StatusCode, message: &str) -> Response {
    let body = serde_json::json!({ "error": message });
    (status, axum::Json(body)).into_response()
}

#[cfg(test)]
mod tests;
