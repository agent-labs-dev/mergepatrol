//! The model APIs as a job sees them. The workspace's keys stay in the orchestrator: each job
//! gets a token instead, which this proxy swaps for the key while the job runs.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, post};
use mergepatrol_core::Provider;
use serde_json::json;

/// Well above what a job's agent runs use, which their turn limit bounds.
pub const REQUESTS_PER_JOB: u32 = 1000;
/// The Messages API's own limit on a request.
const MAX_REQUEST_BYTES: usize = 32 * 1024 * 1024;
/// The request headers that go upstream. The key replaces the job's token.
const FORWARDED: [&str; 5] = [
    "content-type",
    "accept",
    "anthropic-version",
    "anthropic-beta",
    "openai-beta",
];

/// The APIs the agents speak: the Claude CLI the Messages API, Codex the Responses API.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Api {
    Messages,
    Responses,
}

impl Api {
    /// Where the proxy serves it. A job's base URL is `<orchestrator><prefix>`.
    pub fn prefix(self) -> &'static str {
        match self {
            Api::Messages => "/anthropic",
            Api::Responses => "/openai",
        }
    }
}

/// Where each provider's APIs are. Others in tests.
#[derive(Clone)]
pub struct Upstreams {
    pub anthropic: String,
    pub openai: String,
    /// Both of OpenRouter's APIs: `/v1/messages` and `/v1/responses`.
    pub openrouter: String,
}

impl Default for Upstreams {
    fn default() -> Self {
        Self {
            anthropic: "https://api.anthropic.com".into(),
            openai: "https://api.openai.com".into(),
            openrouter: "https://openrouter.ai/api".into(),
        }
    }
}

impl Upstreams {
    /// The base URL of `provider`'s `api`, and whether the key goes as a bearer token rather
    /// than `x-api-key`. `None` when the provider does not serve the API.
    fn of(&self, provider: Provider, api: Api) -> Option<(&str, bool)> {
        match (provider, api) {
            (Provider::Anthropic, Api::Messages) => Some((&self.anthropic, false)),
            (Provider::OpenAi, Api::Responses) => Some((&self.openai, true)),
            (Provider::OpenRouter, _) => Some((&self.openrouter, true)),
            _ => None,
        }
    }
}

/// The jobs' tokens, while their jobs run.
pub struct ApiProxy {
    grants: Mutex<HashMap<String, Grant>>,
    upstreams: Upstreams,
    http: reqwest::Client,
}

struct Grant {
    provider: Provider,
    key: String,
    requests_left: u32,
}

impl ApiProxy {
    pub fn new(upstreams: Upstreams) -> Self {
        Self {
            grants: Mutex::default(),
            upstreams,
            http: reqwest::Client::new(),
        }
    }

    /// `POST /anthropic/v1/messages` and `/anthropic/v1/messages/count_tokens`,
    /// `/anthropic/api/hello` (the Claude CLI's check that the API is up), and
    /// `POST /openai/v1/responses`.
    pub fn router(self: Arc<Self>) -> Router {
        let messages = Api::Messages.prefix();
        Router::new()
            .route(&format!("{messages}/v1/messages"), post(messages_api))
            .route(
                &format!("{messages}/v1/messages/count_tokens"),
                post(messages_api),
            )
            .route(
                &format!("{messages}/api/hello"),
                any(|| async { StatusCode::OK }),
            )
            .route(
                &format!("{}/v1/responses", Api::Responses.prefix()),
                post(responses_api),
            )
            .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
            .with_state(self)
    }

    /// A token for `provider` with `key`, good for `requests` until the returned `Granted` is
    /// dropped.
    pub fn grant(self: &Arc<Self>, provider: Provider, key: String, requests: u32) -> Granted {
        let token = crate::remote::random_token();
        self.grants.lock().unwrap().insert(
            token.clone(),
            Grant {
                provider,
                key,
                requests_left: requests,
            },
        );
        Granted {
            proxy: self.clone(),
            token,
        }
    }

    /// The provider and key behind `token`, counting one request.
    fn take(&self, token: &str) -> Option<(Provider, String)> {
        let mut grants = self.grants.lock().unwrap();
        let grant = grants.get_mut(token)?;
        if grant.requests_left == 0 {
            return None;
        }
        grant.requests_left -= 1;
        Some((grant.provider, grant.key.clone()))
    }
}

/// Revokes its token when dropped: when the job is over.
pub struct Granted {
    proxy: Arc<ApiProxy>,
    pub token: String,
}

impl Drop for Granted {
    fn drop(&mut self) {
        self.proxy.grants.lock().unwrap().remove(&self.token);
    }
}

async fn messages_api(
    State(proxy): State<Arc<ApiProxy>>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    forward(&proxy, Api::Messages, &uri, &headers, body).await
}

async fn responses_api(
    State(proxy): State<Arc<ApiProxy>>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    forward(&proxy, Api::Responses, &uri, &headers, body).await
}

async fn forward(
    proxy: &ApiProxy,
    api: Api,
    uri: &Uri,
    headers: &HeaderMap,
    body: Bytes,
) -> Response {
    // The Claude CLI sends its key as `x-api-key`, Codex as a bearer token.
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let token = header("x-api-key")
        .or_else(|| header("authorization").and_then(|v| v.strip_prefix("Bearer ")))
        .unwrap_or_default();
    let Some((provider, key)) = proxy.take(token) else {
        return error(
            api,
            StatusCode::UNAUTHORIZED,
            "unknown or expired job token, or the job is out of requests",
        );
    };
    let Some((upstream, bearer)) = proxy.upstreams.of(provider, api) else {
        return error(
            api,
            StatusCode::BAD_REQUEST,
            &format!("{provider:?} does not serve this API"),
        );
    };
    let path = uri.path().strip_prefix(api.prefix()).unwrap_or(uri.path());
    let query = uri.query().map(|q| format!("?{q}")).unwrap_or_default();
    let mut req = proxy
        .http
        .post(format!("{upstream}{path}{query}"))
        .body(body);
    req = if bearer {
        req.bearer_auth(key)
    } else {
        req.header("x-api-key", key)
    };
    for name in FORWARDED {
        for value in headers.get_all(name) {
            req = req.header(name, value);
        }
    }
    let res = match req.send().await {
        Ok(res) => res,
        Err(e) => {
            tracing::warn!(error = %e, ?provider, "could not reach the model API");
            return error(
                api,
                StatusCode::BAD_GATEWAY,
                "could not reach the model API",
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

/// An error in the shape of the API's own, so the agent's CLI reports it.
fn error(api: Api, status: StatusCode, message: &str) -> Response {
    let body = match api {
        Api::Messages => {
            json!({ "type": "error", "error": { "type": "proxy_error", "message": message } })
        }
        Api::Responses => json!({ "error": { "type": "proxy_error", "message": message } }),
    };
    (status, axum::Json(body)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Echoes what reached it, and which upstream it is.
    async fn upstream(name: &'static str) -> String {
        let app = Router::new().fallback(post(
            move |uri: Uri, headers: HeaderMap, body: Bytes| async move {
                let header = |n: &str| headers.get(n).and_then(|v| v.to_str().ok());
                let seen = json!({
                    "upstream": name,
                    "path": uri.to_string(),
                    "key": header("x-api-key"),
                    "auth": header("authorization"),
                    "beta": header("anthropic-beta"),
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

    /// The proxy, and its URL.
    async fn proxy() -> (Arc<ApiProxy>, String) {
        let proxy = Arc::new(ApiProxy::new(Upstreams {
            anthropic: upstream("anthropic").await,
            openai: upstream("openai").await,
            openrouter: format!("{}/api", upstream("openrouter").await),
        }));
        let url = serve(proxy.clone().router()).await;
        (proxy, url)
    }

    /// What the Claude CLI sends.
    async fn messages(url: &str, token: &str) -> reqwest::Response {
        reqwest::Client::new()
            .post(format!("{url}/anthropic/v1/messages?beta=true"))
            .header("x-api-key", token)
            .header("anthropic-beta", "b1")
            .header("authorization", "Bearer other")
            .body("{\"model\":\"m\"}")
            .send()
            .await
            .unwrap()
    }

    /// What Codex sends.
    async fn responses(url: &str, token: &str) -> reqwest::Response {
        reqwest::Client::new()
            .post(format!("{url}/openai/v1/responses"))
            .bearer_auth(token)
            .body("{\"model\":\"m\"}")
            .send()
            .await
            .unwrap()
    }

    async fn seen(res: reqwest::Response) -> serde_json::Value {
        assert_eq!(res.status(), StatusCode::CREATED);
        let text = res.text().await.unwrap();
        serde_json::from_str(text.strip_prefix("data: ").unwrap()).unwrap()
    }

    #[tokio::test]
    async fn swaps_the_token_for_the_anthropic_key() {
        let (proxy, url) = proxy().await;
        let granted = proxy.grant(Provider::Anthropic, "sk-ant-key".into(), 5);
        let seen = seen(messages(&url, &granted.token).await).await;
        assert_eq!(seen["upstream"], "anthropic");
        assert_eq!(seen["path"], "/v1/messages?beta=true");
        assert_eq!(seen["key"], "sk-ant-key");
        assert_eq!(seen["auth"], serde_json::Value::Null);
        assert_eq!(seen["beta"], "b1");
        assert_eq!(seen["body"], "{\"model\":\"m\"}");
    }

    #[tokio::test]
    async fn swaps_the_token_for_the_openai_key() {
        let (proxy, url) = proxy().await;
        let granted = proxy.grant(Provider::OpenAi, "sk-openai".into(), 5);
        let seen = seen(responses(&url, &granted.token).await).await;
        assert_eq!(seen["upstream"], "openai");
        assert_eq!(seen["path"], "/v1/responses");
        assert_eq!(seen["auth"], "Bearer sk-openai");
        assert_eq!(seen["key"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn openrouter_serves_both_apis() {
        let (proxy, url) = proxy().await;
        let granted = proxy.grant(Provider::OpenRouter, "sk-or".into(), 5);
        for (seen, path) in [
            (
                seen(messages(&url, &granted.token).await).await,
                "/api/v1/messages?beta=true",
            ),
            (
                seen(responses(&url, &granted.token).await).await,
                "/api/v1/responses",
            ),
        ] {
            assert_eq!(seen["upstream"], "openrouter");
            assert_eq!(seen["path"], path);
            assert_eq!(seen["auth"], "Bearer sk-or");
            assert_eq!(seen["key"], serde_json::Value::Null);
        }
    }

    #[tokio::test]
    async fn refuses_an_api_the_provider_does_not_serve() {
        let (proxy, url) = proxy().await;
        let anthropic = proxy.grant(Provider::Anthropic, "sk-ant-key".into(), 5);
        let res = responses(&url, &anthropic.token).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let openai = proxy.grant(Provider::OpenAi, "sk-openai".into(), 5);
        let res = messages(&url, &openai.token).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn refuses_an_unknown_token() {
        let (proxy, url) = proxy().await;
        let _granted = proxy.grant(Provider::Anthropic, "sk-ant-key".into(), 5);
        let res = messages(&url, "guess").await;
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert!(!res.text().await.unwrap().contains("sk-ant-key"));
        assert_eq!(
            responses(&url, "guess").await.status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn a_token_ends_with_its_job() {
        let (proxy, url) = proxy().await;
        let token = proxy
            .grant(Provider::Anthropic, "sk-ant-key".into(), 5)
            .token
            .clone();
        assert_eq!(
            messages(&url, &token).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn a_token_runs_out_of_requests() {
        let (proxy, url) = proxy().await;
        let granted = proxy.grant(Provider::Anthropic, "sk-ant-key".into(), 2);
        for _ in 0..2 {
            assert_eq!(
                messages(&url, &granted.token).await.status(),
                StatusCode::CREATED
            );
        }
        assert_eq!(
            messages(&url, &granted.token).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn forwards_only_the_model_apis() {
        let (proxy, url) = proxy().await;
        let granted = proxy.grant(Provider::OpenRouter, "sk-or".into(), 5);
        let http = reqwest::Client::new();
        for path in [
            "/anthropic/v1/models",
            "/openai/v1/models",
            "/openai/v1/files",
        ] {
            let res = http
                .post(format!("{url}{path}"))
                .bearer_auth(&granted.token)
                .send()
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::NOT_FOUND, "{path}");
        }
        let res = http
            .head(format!("{url}/anthropic/api/hello"))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
}
