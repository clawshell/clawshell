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
use std::time::Duration;
use tracing::{debug, trace};

/// Configuration for HTTP client timeouts and connection pooling.
///
/// These settings control how the proxy client behaves when connecting to
/// upstream LLM APIs. The defaults are tuned for typical LLM workloads where
/// responses may take several minutes for long completions.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Maximum time to wait for TCP connection establishment.
    pub connect_timeout: Duration,
    /// Maximum time for the entire request/response cycle.
    pub request_timeout: Duration,
    /// How long idle connections stay in the pool before being closed.
    pub pool_idle_timeout: Duration,
    /// Maximum idle connections to keep per host.
    pub pool_max_idle_per_host: usize,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(30),
            request_timeout: Duration::from_secs(300), // 5 minutes for LLM completions
            pool_idle_timeout: Duration::from_secs(90),
            pool_max_idle_per_host: 32,
        }
    }
}

#[derive(Debug)]
pub struct ProxyClient {
    client: Client,
    upstream_urls: BTreeMap<Provider, String>,
    anthropic_version: String,
}

impl ProxyClient {
    /// Creates a new ProxyClient with default timeout configuration.
    pub fn with_upstream_urls(
        upstream_urls: BTreeMap<Provider, String>,
        anthropic_version: String,
    ) -> Self {
        Self::with_config(upstream_urls, anthropic_version, ClientConfig::default())
    }

    /// Creates a new ProxyClient with custom timeout and pool configuration.
    ///
    /// # Arguments
    /// * `upstream_urls` - Map of provider to base URL
    /// * `anthropic_version` - Anthropic API version header value
    /// * `config` - Timeout and connection pool settings
    ///
    /// # Panics
    /// Panics if the HTTP client fails to build (e.g., TLS initialization failure).
    pub fn with_config(
        upstream_urls: BTreeMap<Provider, String>,
        anthropic_version: String,
        config: ClientConfig,
    ) -> Self {
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(config.connect_timeout)
            .timeout(config.request_timeout)
            .pool_idle_timeout(config.pool_idle_timeout)
            .pool_max_idle_per_host(config.pool_max_idle_per_host)
            .build()
            .expect("Failed to build reqwest client");

        debug!(
            connect_timeout_ms = config.connect_timeout.as_millis(),
            request_timeout_ms = config.request_timeout.as_millis(),
            pool_idle_timeout_ms = config.pool_idle_timeout.as_millis(),
            pool_max_idle_per_host = config.pool_max_idle_per_host,
            "ProxyClient initialized with timeout configuration"
        );

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

        let mut req_headers = HeaderMap::new();
        for (name, value) in &headers {
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
            req_headers.insert(name.clone(), value.clone());
        }

        trace!(
            forwarded_header_count = req_headers.len(),
            "Filtered request headers"
        );

        // Inject the real API key based on provider
        match provider {
            Provider::Openai => {
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
            .request(reqwest_method, &upstream_url)
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
            provider = ?provider,
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

            // Rebind the `status` var to clarify the type for human developer:
            // it is guaranteed to be `StatusCode` due to the `.unwrap_or` in its assignment above.
            let status: StatusCode = status;
            let mut response = Response::builder().status(status);
            // INVARIANT: the `status` variable is guaranteed to be `StatusCode`,
            // so this `.unwrap` should never panic.
            *response.headers_mut().unwrap() = resp_headers;
            // INVARIANT: the builder should always succeed since we just added a valid status code and headers,
            // so this `.unwrap` should never panic.
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

            // Rebind the `status` var to clarify the type for human developer:
            // it is guaranteed to be `StatusCode` due to the `.unwrap_or` in its assignment above.
            let status: StatusCode = status;
            let mut response = Response::builder().status(status);
            // INVARIANT: the builder should always succeed since we just added a valid status code and headers,
            // so this `.unwrap` should never panic.
            *response.headers_mut().unwrap() = resp_headers;
            Ok(response.body(Body::from(resp_body)).unwrap())
        }
    }
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
    fn test_client_config_default() {
        let config = ClientConfig::default();
        assert_eq!(config.connect_timeout, Duration::from_secs(30));
        assert_eq!(config.request_timeout, Duration::from_secs(300));
        assert_eq!(config.pool_idle_timeout, Duration::from_secs(90));
        assert_eq!(config.pool_max_idle_per_host, 32);
    }

    #[test]
    fn test_proxy_client_with_custom_config() {
        let mut urls = BTreeMap::new();
        urls.insert(Provider::Openai, "https://api.openai.com".to_string());

        let custom_config = ClientConfig {
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(60),
            pool_idle_timeout: Duration::from_secs(30),
            pool_max_idle_per_host: 16,
        };

        // This should not panic - verifies the client builds successfully
        let _client = ProxyClient::with_config(urls, "2023-06-01".to_string(), custom_config);
    }

    #[test]
    fn test_proxy_client_with_default_config() {
        let mut urls = BTreeMap::new();
        urls.insert(Provider::Openai, "https://api.openai.com".to_string());

        // with_upstream_urls should use default config internally
        let _client = ProxyClient::with_upstream_urls(urls, "2023-06-01".to_string());
    }
}
