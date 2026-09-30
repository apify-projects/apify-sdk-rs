//! The Apify API client of the SDK: `apify-client` on a reqwest transport that counts
//! rate-limited responses, for the storage load signal of crawlee-rs autoscaling.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use apify_client::http_client::{HttpBackend, HttpMethod, HttpRequest, HttpResponse};
use apify_client::{ApifyClient, ApifyClientError, ApifyClientResult};
use async_trait::async_trait;

use crate::configuration::Configuration;

/// How many API responses were `429 Too Many Requests`. Clones share the count.
#[derive(Clone, Debug, Default)]
pub struct RateLimitCounter(Arc<AtomicU64>);

impl RateLimitCounter {
    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }

    fn increment(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

/// An [`HttpBackend`] on reqwest that counts `429` responses.
#[derive(Clone, Debug)]
pub struct ReqwestBackend {
    client: reqwest::Client,
    rate_limits: RateLimitCounter,
}

impl ReqwestBackend {
    pub fn new(rate_limits: RateLimitCounter) -> Self {
        ReqwestBackend::with_client(reqwest::Client::new(), rate_limits)
    }

    pub fn with_client(client: reqwest::Client, rate_limits: RateLimitCounter) -> Self {
        ReqwestBackend { client, rate_limits }
    }
}

fn transport_error(err: reqwest::Error) -> ApifyClientError {
    if err.is_timeout() {
        return ApifyClientError::Timeout;
    }
    let mut message = err.to_string();
    let mut source = std::error::Error::source(&err);
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    ApifyClientError::Http(message)
}

#[async_trait]
impl HttpBackend for ReqwestBackend {
    async fn send(&self, request: HttpRequest) -> ApifyClientResult<HttpResponse> {
        let method = match request.method {
            HttpMethod::Get => reqwest::Method::GET,
            HttpMethod::Post => reqwest::Method::POST,
            HttpMethod::Put => reqwest::Method::PUT,
            HttpMethod::Delete => reqwest::Method::DELETE,
            HttpMethod::Head => reqwest::Method::HEAD,
        };
        let mut builder = self.client.request(method, &request.url).timeout(request.timeout);
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        if let Some(body) = request.body {
            builder = builder.body(body);
        }

        let response = builder.send().await.map_err(transport_error)?;
        let status = response.status().as_u16();
        if status == 429 {
            self.rate_limits.increment();
        }
        let headers: HashMap<String, String> = response
            .headers()
            .iter()
            .filter_map(|(name, value)| Some((name.as_str().to_owned(), value.to_str().ok()?.to_owned())))
            .collect();
        let body = response.bytes().await.map_err(transport_error)?.to_vec();
        Ok(HttpResponse { status, headers, body })
    }
}

/// The `User-Agent` suffix of the SDK's requests, as in the JS SDK.
pub fn user_agent_suffix() -> String {
    format!("SDK/{}; Crawlee/{}", env!("CARGO_PKG_VERSION"), crawlee::VERSION)
}

/// A client for the API `configuration` points to, authenticated with `token` or else with
/// [`Configuration::token`]. Its rate-limited responses are counted in `rate_limits`.
pub fn new_client(configuration: &Configuration, token: Option<&str>, rate_limits: RateLimitCounter) -> ApifyClient {
    let mut builder = ApifyClient::builder()
        .base_url(configuration.api_base_url.as_str())
        .public_base_url(configuration.api_public_base_url.as_str())
        .user_agent_suffix(user_agent_suffix())
        .http_backend(Arc::new(ReqwestBackend::new(rate_limits)));
    if let Some(token) = token.or(configuration.token.as_deref()) {
        builder = builder.token(token);
    }
    builder.build()
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::get;

    use super::*;

    #[tokio::test]
    async fn requests_are_authenticated_and_429s_are_counted() {
        let hits = Arc::new(AtomicU64::new(0));
        let seen = hits.clone();
        let app = Router::new().route(
            "/v2/datasets/{id}",
            get(move |headers: HeaderMap| {
                let seen = seen.clone();
                async move {
                    assert_eq!(headers["authorization"], "Bearer secret");
                    let user_agent = headers["user-agent"].to_str().unwrap().to_owned();
                    assert!(user_agent.ends_with(&user_agent_suffix()), "{user_agent}");
                    if seen.fetch_add(1, Ordering::SeqCst) < 2 {
                        return (StatusCode::TOO_MANY_REQUESTS, String::new());
                    }
                    let dataset = serde_json::json!({ "data": {
                        "id": "abc", "name": null, "userId": "u", "createdAt": "2026-01-01T00:00:00.000Z",
                        "modifiedAt": "2026-01-01T00:00:00.000Z", "accessedAt": "2026-01-01T00:00:00.000Z",
                        "itemCount": 3, "cleanItemCount": 3
                    }});
                    (StatusCode::OK, dataset.to_string())
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let configuration = Configuration { api_base_url: base, ..Configuration::default() };
        let rate_limits = RateLimitCounter::default();
        let client = new_client(&configuration, Some("secret"), rate_limits.clone());
        let dataset = client.dataset("abc").get().await.unwrap().unwrap();
        assert_eq!(dataset.item_count, Some(3));
        assert_eq!(rate_limits.get(), 2, "both rate-limited attempts were retried and counted");
        assert_eq!(hits.load(Ordering::SeqCst), 3);
    }
}
