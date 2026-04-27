use crate::config::Provider;

use axum::body::Body;
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::TryStreamExt;
use reqwest::Client;
use std::collections::BTreeMap;
use std::io::Error as IoError;
use tracing::{debug, trace};

#[derive(Debug)]
pub struct ProxyClient {
    client: Client,
    upstream_urls: BTreeMap<Provider, String>,
    anthropic_version: String,
}

impl ProxyClient {
    pub fn with_upstream_urls(
        upstream_urls: BTreeMap<Provider, String>,
        anthropic_version: String,
    ) -> Self {
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("Failed to build reqwest client");
        Self {
            client,
            upstream_urls,
            anthropic_version,
        }
    }

    pub async fn forward(
        &self,
        method: Method,
        uri: &Uri,
        headers: HeaderMap,
        real_key: &str,
        body: Bytes,
        provider: Provider,
    ) -> Result<Response, ProxyError> {
        let base_url = self.upstream_urls.get(&provider).ok_or_else(|| {
            ProxyError::Internal(format!("No upstream URL for provider {:?}", provider))
        })?;
        let upstream_url = format!(
            "{}{}",
            base_url,
            uri.path_and_query()
                .map(|pq| pq.as_str())
                .unwrap_or(uri.path())
        );

        debug!(
            %upstream_url,
            %method,
            provider = ?provider,
            body_size = body.len(),
            "Preparing upstream request"
        );

        let mut req_headers = filter_hop_by_hop_headers(&headers);

        trace!(
            forwarded_header_count = req_headers.len(),
            "Filtered request headers"
        );

        // Inject the real API key based on provider
        match provider {
            Provider::Openai | Provider::Openrouter | Provider::Minimax | Provider::Opencode => {
                req_headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", real_key))
                        .map_err(|_| ProxyError::Internal("Invalid real key format".into()))?,
                );
            }
            Provider::Anthropic => {
                req_headers.insert(
                    "x-api-key",
                    HeaderValue::from_str(real_key)
                        .map_err(|_| ProxyError::Internal("Invalid real key format".into()))?,
                );
                req_headers.insert(
                    "anthropic-version",
                    HeaderValue::from_str(&self.anthropic_version)
                        .map_err(|_| ProxyError::Internal("Invalid anthropic version".into()))?,
                );
            }
        }

        self.send_upstream(method, &upstream_url, req_headers, body)
            .await
    }

    /// Forward a request using OAuth-injected auth headers and optional overrides.
    #[allow(clippy::too_many_arguments)]
    pub async fn forward_oauth(
        &self,
        method: Method,
        uri: &Uri,
        original_headers: HeaderMap,
        body: Bytes,
        provider: Provider,
        auth_headers: HeaderMap,
        upstream_url_override: Option<&str>,
    ) -> Result<Response, ProxyError> {
        let upstream_url = if let Some(base) = upstream_url_override {
            format!(
                "{}{}",
                base,
                uri.path_and_query()
                    .map(|pq| pq.as_str())
                    .unwrap_or(uri.path())
            )
        } else {
            let base_url = self.upstream_urls.get(&provider).ok_or_else(|| {
                ProxyError::Internal(format!("No upstream URL for provider {:?}", provider))
            })?;
            format!(
                "{}{}",
                base_url,
                uri.path_and_query()
                    .map(|pq| pq.as_str())
                    .unwrap_or(uri.path())
            )
        };

        debug!(
            %upstream_url,
            %method,
            provider = ?provider,
            body_size = body.len(),
            "Preparing OAuth upstream request"
        );

        let mut req_headers = filter_hop_by_hop_headers(&original_headers);

        // Apply OAuth auth headers (these may include Authorization, x-goog-api-client, etc.)
        for (name, value) in &auth_headers {
            req_headers.insert(name.clone(), value.clone());
        }

        trace!(
            forwarded_header_count = req_headers.len(),
            "Filtered request headers (OAuth)"
        );

        self.send_upstream(method, &upstream_url, req_headers, body)
            .await
    }

    async fn send_upstream(
        &self,
        method: Method,
        upstream_url: &str,
        req_headers: HeaderMap,
        body: Bytes,
    ) -> Result<Response, ProxyError> {
        let reqwest_method = match method {
            Method::GET => reqwest::Method::GET,
            Method::POST => reqwest::Method::POST,
            Method::PUT => reqwest::Method::PUT,
            Method::DELETE => reqwest::Method::DELETE,
            Method::PATCH => reqwest::Method::PATCH,
            Method::HEAD => reqwest::Method::HEAD,
            Method::OPTIONS => reqwest::Method::OPTIONS,
            _ => {
                return Err(ProxyError::MethodNotAllowed(method.to_string()));
            }
        };

        trace!(%upstream_url, "Sending request to upstream");

        let upstream_resp = self
            .client
            .request(reqwest_method, upstream_url)
            .headers(req_headers)
            .body(body)
            .send()
            .await
            .map_err(|e| ProxyError::Upstream(e.to_string()))?;

        // Build the response to send back to the client
        let status = StatusCode::from_u16(upstream_resp.status().as_u16())
            .unwrap_or(StatusCode::BAD_GATEWAY);

        debug!(
            upstream_status = %status,
            "Received upstream response"
        );

        let mut resp_headers = HeaderMap::new();
        for (name, value) in upstream_resp.headers() {
            // Skip hop-by-hop and transfer-encoding (axum handles chunked encoding)
            let name_str = name.as_str().to_lowercase();
            if name_str == "transfer-encoding" || name_str == "connection" {
                continue;
            }
            resp_headers.insert(name.clone(), value.clone());
        }

        // Check if this is a streaming response
        let is_streaming = upstream_resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.contains("text/event-stream"));

        if is_streaming {
            debug!("Streaming response detected, proxying without buffering");
            // Stream the response body without buffering
            let byte_stream = upstream_resp.bytes_stream().map_err(IoError::other);
            let body = Body::from_stream(byte_stream);

            let status: StatusCode = status;
            let mut response = Response::builder().status(status);
            *response.headers_mut().unwrap() = resp_headers;
            Ok(response.body(body).unwrap())
        } else {
            // Buffer the full response
            let resp_body = upstream_resp
                .bytes()
                .await
                .map_err(|e| ProxyError::Upstream(e.to_string()))?;

            trace!(
                response_body_size = resp_body.len(),
                "Buffered upstream response body"
            );

            let status: StatusCode = status;
            let mut response = Response::builder().status(status);
            *response.headers_mut().unwrap() = resp_headers;
            Ok(response.body(Body::from(resp_body)).unwrap())
        }
    }
}

fn filter_hop_by_hop_headers(headers: &HeaderMap) -> HeaderMap {
    let mut filtered = HeaderMap::new();
    for (name, value) in headers {
        let name_str = name.as_str().to_lowercase();
        // Skip hop-by-hop headers and the original auth header
        if name_str == "host"
            || name_str == "authorization"
            || name_str == "connection"
            || name_str == "content-length"
            || name_str == "transfer-encoding"
            || name_str == "x-api-key"
        {
            trace!(header = %name_str, "Skipping hop-by-hop/auth header");
            continue;
        }
        filtered.insert(name.clone(), value.clone());
    }
    filtered
}

#[derive(Debug)]
pub enum ProxyError {
    Upstream(String),
    Internal(String),
    MethodNotAllowed(String),
}

impl std::fmt::Display for ProxyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProxyError::Upstream(msg) => write!(f, "Upstream error: {}", msg),
            ProxyError::Internal(msg) => write!(f, "Internal error: {}", msg),
            ProxyError::MethodNotAllowed(method) => {
                write!(f, "Method not allowed: {}", method)
            }
        }
    }
}

impl IntoResponse for ProxyError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            ProxyError::Upstream(msg) => (axum::http::StatusCode::BAD_GATEWAY, msg),
            ProxyError::Internal(msg) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, msg),
            ProxyError::MethodNotAllowed(method) => (
                axum::http::StatusCode::METHOD_NOT_ALLOWED,
                format!("Method not allowed: {}", method),
            ),
        };
        let body = serde_json::json!({ "error": message });
        (status, axum::Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use http_body_util::BodyExt;

    #[test]
    fn test_proxy_error_display_upstream() {
        let err = ProxyError::Upstream("connection refused".to_string());
        assert_eq!(format!("{err}"), "Upstream error: connection refused");
    }

    #[test]
    fn test_proxy_error_display_internal() {
        let err = ProxyError::Internal("bad key".to_string());
        assert_eq!(format!("{err}"), "Internal error: bad key");
    }

    #[tokio::test]
    async fn test_proxy_error_into_response_upstream() {
        let err = ProxyError::Upstream("timeout".to_string());
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "timeout");
    }

    #[tokio::test]
    async fn test_proxy_error_into_response_internal() {
        let err = ProxyError::Internal("fail".to_string());
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn test_proxy_error_display_method_not_allowed() {
        let err = ProxyError::MethodNotAllowed("TRACE".to_string());
        assert_eq!(format!("{err}"), "Method not allowed: TRACE");
    }

    #[tokio::test]
    async fn test_forward_missing_provider_url() {
        // Create a ProxyClient with only OpenAI, then try to forward for Anthropic
        let mut urls = BTreeMap::new();
        urls.insert(Provider::Openai, "http://localhost:1".to_string());

        let client = ProxyClient::with_upstream_urls(urls, "2023-06-01".to_string());
        let result = client
            .forward(
                Method::POST,
                &"/v1/messages".parse::<Uri>().unwrap(),
                HeaderMap::new(),
                "sk-test",
                Bytes::from("{}"),
                Provider::Anthropic,
            )
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            ProxyError::Internal(msg) => assert!(msg.contains("No upstream URL")),
            other => panic!("Expected Internal error, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_proxy_error_into_response_method_not_allowed() {
        let err = ProxyError::MethodNotAllowed("TRACE".to_string());
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["error"].as_str().unwrap().contains("TRACE"));
    }

    #[test]
    fn test_filter_hop_by_hop_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer vk-test".parse().unwrap());
        headers.insert("content-type", "application/json".parse().unwrap());
        headers.insert("host", "localhost".parse().unwrap());
        headers.insert("x-custom", "custom-value".parse().unwrap());

        let filtered = filter_hop_by_hop_headers(&headers);
        assert!(filtered.get("authorization").is_none());
        assert!(filtered.get("host").is_none());
        assert!(filtered.get("content-type").is_some());
        assert!(filtered.get("x-custom").is_some());
    }
}
