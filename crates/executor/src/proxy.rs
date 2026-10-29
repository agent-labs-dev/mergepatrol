//! The Anthropic API as a job sees it. The workspace's key stays in the orchestrator: each job
//! gets a token instead, which this proxy swaps for the key while the job runs.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, post};
use serde_json::json;

pub const UPSTREAM: &str = "https://api.anthropic.com";
/// The path the proxy answers under. A job's base URL is `<orchestrator>/anthropic`.
pub const PREFIX: &str = "/anthropic";
/// Well above what a job's agent runs use, which their turn limit bounds.
pub const REQUESTS_PER_JOB: u32 = 1000;
/// The Messages API's own limit on a request.
const MAX_REQUEST_BYTES: usize = 32 * 1024 * 1024;
/// The request headers that go upstream. The key replaces the job's token.
const FORWARDED: [&str; 4] = [
    "content-type",
    "accept",
    "anthropic-version",
    "anthropic-beta",
];

/// The jobs' tokens, while their jobs run.
pub struct AnthropicProxy {
    grants: Mutex<HashMap<String, Grant>>,
    upstream: String,
    http: reqwest::Client,
}

struct Grant {
    key: String,
    requests_left: u32,
}

impl AnthropicProxy {
    pub fn new(upstream: &str) -> Self {
        Self {
            grants: Mutex::default(),
            upstream: upstream.trim_end_matches('/').to_owned(),
            http: reqwest::Client::new(),
        }
    }

    /// `POST /anthropic/v1/messages` and `/anthropic/v1/messages/count_tokens`, and
    /// `/anthropic/api/hello`, the CLI's check that the API is up.
    pub fn router(self: Arc<Self>) -> Router {
        Router::new()
            .route(&format!("{PREFIX}/v1/messages"), post(forward))
            .route(&format!("{PREFIX}/v1/messages/count_tokens"), post(forward))
            .route(
                &format!("{PREFIX}/api/hello"),
                any(|| async { StatusCode::OK }),
            )
            .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
            .with_state(self)
    }

    /// A token for `key`, good for `requests` until the returned `Granted` is dropped.
    pub fn grant(self: &Arc<Self>, key: String, requests: u32) -> Granted {
        let token = crate::remote::random_token();
        self.grants.lock().unwrap().insert(
            token.clone(),
            Grant {
                key,
                requests_left: requests,
            },
        );
        Granted {
            proxy: self.clone(),
            token,
        }
    }

    /// The key behind `token`, counting one request.
    fn take(&self, token: &str) -> Option<String> {
        let mut grants = self.grants.lock().unwrap();
        let grant = grants.get_mut(token)?;
        if grant.requests_left == 0 {
            return None;
        }
        grant.requests_left -= 1;
        Some(grant.key.clone())
    }
}

/// Revokes its token when dropped: when the job is over.
pub struct Granted {
    proxy: Arc<AnthropicProxy>,
    pub token: String,
}

impl Drop for Granted {
    fn drop(&mut self) {
        self.proxy.grants.lock().unwrap().remove(&self.token);
    }
}

async fn forward(
    State(proxy): State<Arc<AnthropicProxy>>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let token = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let Some(key) = proxy.take(token) else {
        return error(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "unknown or expired job token, or the job is out of requests",
        );
    };
    let path = uri.path().strip_prefix(PREFIX).unwrap_or(uri.path());
    let query = uri.query().map(|q| format!("?{q}")).unwrap_or_default();
    let mut req = proxy
        .http
        .post(format!("{}{path}{query}", proxy.upstream))
        .header("x-api-key", key)
        .body(body);
    for name in FORWARDED {
        for value in headers.get_all(name) {
            req = req.header(name, value);
        }
    }
    let res = match req.send().await {
        Ok(res) => res,
        Err(e) => {
            tracing::warn!(error = %e, "could not reach the Anthropic API");
            return error(
                StatusCode::BAD_GATEWAY,
                "api_error",
                "could not reach the Anthropic API",
            );
        }
    };
    let mut out = Response::builder().status(res.status());
    for (name, value) in res.headers() {
        // The body is streamed again, with its own framing.
        if !matches!(
            name.as_str(),
            "content-length" | "transfer-encoding" | "connection"
        ) {
            out = out.header(name, value);
        }
    }
    out.body(Body::from_stream(res.bytes_stream()))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

/// An error in the shape of the Anthropic API's, so the CLI reports it.
fn error(status: StatusCode, kind: &str, message: &str) -> Response {
    let body = json!({ "type": "error", "error": { "type": kind, "message": message } });
    (status, axum::Json(body)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::post;

    /// Echoes what reached it.
    async fn upstream() -> String {
        let app = Router::new().fallback(post(
            |uri: Uri, headers: HeaderMap, body: Bytes| async move {
                let seen = json!({
                    "path": uri.to_string(),
                    "key": headers.get("x-api-key").and_then(|v| v.to_str().ok()),
                    "beta": headers.get("anthropic-beta").and_then(|v| v.to_str().ok()),
                    "body": String::from_utf8_lossy(&body),
                })
                .to_string();
                (StatusCode::CREATED, format!("data: {seen}"))
            },
        ));
        serve(app).await
    }

    async fn serve(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(axum::serve(listener, app).into_future());
        url
    }

    async fn proxy() -> (Arc<AnthropicProxy>, String) {
        let proxy = Arc::new(AnthropicProxy::new(&upstream().await));
        let url = serve(proxy.clone().router()).await;
        (proxy, format!("{url}{PREFIX}"))
    }

    async fn messages(base: &str, token: &str) -> reqwest::Response {
        reqwest::Client::new()
            .post(format!("{base}/v1/messages?beta=true"))
            .header("x-api-key", token)
            .header("anthropic-beta", "b1")
            .header("authorization", "Bearer other")
            .body("{\"model\":\"m\"}")
            .send()
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn swaps_the_token_for_the_key() {
        let (proxy, base) = proxy().await;
        let granted = proxy.grant("sk-ant-key".into(), 5);
        let res = messages(&base, &granted.token).await;
        assert_eq!(res.status(), StatusCode::CREATED);
        let text = res.text().await.unwrap();
        let seen: serde_json::Value =
            serde_json::from_str(text.strip_prefix("data: ").unwrap()).unwrap();
        assert_eq!(seen["path"], "/v1/messages?beta=true");
        assert_eq!(seen["key"], "sk-ant-key");
        assert_eq!(seen["beta"], "b1");
        assert_eq!(seen["body"], "{\"model\":\"m\"}");
    }

    #[tokio::test]
    async fn refuses_an_unknown_token() {
        let (proxy, base) = proxy().await;
        let _granted = proxy.grant("sk-ant-key".into(), 5);
        let res = messages(&base, "guess").await;
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert!(!res.text().await.unwrap().contains("sk-ant-key"));
    }

    #[tokio::test]
    async fn a_token_ends_with_its_job() {
        let (proxy, base) = proxy().await;
        let token = proxy.grant("sk-ant-key".into(), 5).token.clone();
        assert_eq!(
            messages(&base, &token).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn a_token_runs_out_of_requests() {
        let (proxy, base) = proxy().await;
        let granted = proxy.grant("sk-ant-key".into(), 2);
        for _ in 0..2 {
            assert_eq!(
                messages(&base, &granted.token).await.status(),
                StatusCode::CREATED
            );
        }
        assert_eq!(
            messages(&base, &granted.token).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn forwards_only_the_messages_api() {
        let (proxy, base) = proxy().await;
        let granted = proxy.grant("sk-ant-key".into(), 5);
        let http = reqwest::Client::new();
        let res = http
            .get(format!("{base}/v1/models"))
            .header("x-api-key", &granted.token)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let res = http.head(format!("{base}/api/hello")).send().await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
}
