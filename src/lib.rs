#![deny(warnings)]
#![deny(unsafe_code)] // why would we need unsafe code in this project?
#![deny(missing_debug_implementations)]

pub mod cli;
pub mod config;
pub mod dlp;
pub mod keys;
pub mod onboard;
pub mod process;
pub mod proxy;
pub mod tui;

use crate::config::{Config, Provider};
use crate::dlp::DlpScanner;
use crate::keys::KeyManager;
use crate::proxy::ProxyClient;

use axum::Router;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use bytes::Bytes;
use http_body_util::BodyExt;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, error, info, trace, warn};

#[derive(Debug, Clone)]
pub struct AppState {
    pub key_manager: Arc<KeyManager>,
    pub dlp_scanner: Arc<DlpScanner>,
    pub proxy_client: Arc<ProxyClient>,
}

impl AppState {
    pub fn from_config(config: &Config) -> Self {
        let mut upstream_urls = BTreeMap::new();
        upstream_urls.insert(Provider::Openai, config.upstream_url(Provider::Openai));
        upstream_urls.insert(
            Provider::Anthropic,
            config.upstream_url(Provider::Anthropic),
        );
        Self {
            key_manager: Arc::new(KeyManager::new(config.key_map())),
            dlp_scanner: Arc::new(
                DlpScanner::with_response_scanning(&config.dlp.patterns, config.dlp.scan_responses)
                    .expect("Failed to compile DLP patterns"),
            ),
            proxy_client: Arc::new(ProxyClient::with_upstream_urls(
                upstream_urls,
                config.upstream.anthropic_version.clone(),
            )),
        }
    }
}

/// Maximum request body size (10 MiB).
const MAX_BODY_SIZE: usize = 10 * 1024 * 1024;

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", any(handle_request))
        .route("/{*path}", any(handle_request))
        .layer(DefaultBodyLimit::max(MAX_BODY_SIZE))
        .with_state(state)
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
