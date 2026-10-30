//! The HTTP server: the JSON API under `/api`, sign-in and setup, and the dashboard (the built
//! `ui/`, embedded). The API types derive `TS`. `cargo test` writes them to `ui/src/api/`.

mod api;
mod auth;
mod billing;
mod blog;
mod mcp;
mod oauth;
mod routes;
mod ui;
mod v1;
mod webhook;

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use mergepatrol_analytics::Analytics;
use mergepatrol_billing::Billing;
use mergepatrol_core::RepoSettings;
use mergepatrol_github::AppCredentials;
use mergepatrol_resolver::ChatGptAuth;
use mergepatrol_store::{GITHUB_APP, Scope, Store, Vault};
use secrecy::ExposeSecret;

pub use api::*;
pub use auth::{
    AnalyticsView, AppView, ManifestForm, ManifestRequest, SessionStatus, SessionView, Unlock,
    UserView,
};
pub use billing::{AccessView, BillingView, PricingView, RedirectView};
pub use webhook::Wake;

pub struct WebConfig {
    /// The hosted GitHub App, from the environment. `None` self-hosted: the App is created in
    /// the dashboard and kept in the vault.
    pub hosted_app: Option<AppCredentials>,
    /// Where users reach the dashboard, for example `http://localhost:8080`. GitHub redirects here.
    pub public_url: String,
    /// Unlocks the first-run setup. `None` once the GitHub App exists.
    pub setup_token: Option<String>,
    /// Webhooks wake the queue of the repo they name.
    pub wake: Arc<dyn Wake>,
    /// The settings of a repo whose config file leaves them out.
    pub repo_defaults: RepoSettings,
    /// Product analytics: off self-hosted.
    pub analytics: Analytics,
    /// Hosted billing: off self-hosted.
    pub billing: Option<Arc<Billing>>,
    /// OpenAI's sign-in, for ChatGPT logins.
    pub chatgpt: ChatGptAuth,
}

#[derive(Clone)]
struct AppState {
    store: Arc<dyn Store>,
    vault: Arc<Vault>,
    public_url: Arc<str>,
    hosted_app: Option<Arc<AppCredentials>>,
    setup_token: Arc<Mutex<Option<String>>>,
    wake: Arc<dyn Wake>,
    repo_defaults: Arc<RepoSettings>,
    analytics: Analytics,
    billing: Option<Arc<Billing>>,
    chatgpt: ChatGptAuth,
    /// What GitHub said lately of the tokens MCP calls come with.
    seen: Arc<auth::Seen>,
    /// The `/api/v1` calls each caller has left.
    budgets: Arc<v1::Budgets>,
}

impl AppState {
    async fn app_credentials(&self) -> anyhow::Result<Option<AppCredentials>> {
        if let Some(app) = &self.hosted_app {
            return Ok(Some((**app).clone()));
        }
        match self.vault.get(Scope::Instance, GITHUB_APP).await? {
            Some(json) => Ok(Some(serde_json::from_str(json.expose_secret())?)),
            None => Ok(None),
        }
    }

    fn secure_cookies(&self) -> bool {
        self.public_url.starts_with("https://")
    }
}

pub fn router(store: Arc<dyn Store>, vault: Arc<Vault>, config: WebConfig) -> Router {
    serve(state(store, vault, config))
}

fn state(store: Arc<dyn Store>, vault: Arc<Vault>, config: WebConfig) -> AppState {
    AppState {
        store,
        vault,
        public_url: config.public_url.trim_end_matches('/').into(),
        hosted_app: config.hosted_app.map(Arc::new),
        setup_token: Arc::new(Mutex::new(config.setup_token)),
        wake: config.wake,
        repo_defaults: Arc::new(config.repo_defaults),
        analytics: config.analytics,
        billing: config.billing,
        chatgpt: config.chatgpt,
        seen: Arc::default(),
        budgets: Arc::default(),
    }
}

fn serve(state: AppState) -> Router {
    routes::router().fallback(ui::serve).with_state(state)
}

enum ApiError {
    /// A message for the user.
    Status(StatusCode, String),
    /// GitHub's rate limit is used up, until this much later.
    GitHubLimited(std::time::Duration, String),
    Internal(anyhow::Error),
}

impl ApiError {
    fn bad_request(m: &str) -> Self {
        Self::Status(StatusCode::BAD_REQUEST, m.to_owned())
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        match e.downcast_ref::<mergepatrol_github::RateLimited>() {
            Some(limited) => Self::GitHubLimited(limited.remaining(), format!("{limited}.")),
            None => Self::Internal(e),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            Self::Status(code, m) => (code, m).into_response(),
            Self::GitHubLimited(wait, m) => {
                let secs = wait.as_secs().max(1).to_string();
                let retry = [(axum::http::header::RETRY_AFTER, secs)];
                (StatusCode::SERVICE_UNAVAILABLE, retry, m).into_response()
            }
            Self::Internal(e) => {
                tracing::error!(error = %format!("{e:#}"), "API error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use mergepatrol_core::RepoName;
    use mergepatrol_github::setup::{Pkce, UserToken};
    use mergepatrol_store::{LoginCode, PgStore, Session, SessionKind};
    use sqlx::PgPool;
    use tower::ServiceExt;

    const KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

    fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    #[derive(Default)]
    struct Woken(Mutex<Vec<(u64, String)>>);

    impl Wake for Woken {
        fn wake(&self, installation: u64, repo: mergepatrol_core::RepoName) {
            self.0
                .lock()
                .unwrap()
                .push((installation, repo.to_string()));
        }
    }

    /// A router, and the cookie of a user who can access installation 1 only.
    async fn signed_in(pool: PgPool) -> (Router, String) {
        with_app(pool, None, Arc::default()).await
    }

    async fn with_app(
        pool: PgPool,
        hosted_app: Option<AppCredentials>,
        wake: Arc<Woken>,
    ) -> (Router, String) {
        with_config(pool, web_config(hosted_app, wake)).await
    }

    async fn with_config(pool: PgPool, config: WebConfig) -> (Router, String) {
        let store: Arc<dyn Store> = Arc::new(PgStore::migrate(pool).await.unwrap());
        let vault = Arc::new(Vault::new(store.clone(), KEY).unwrap());
        let expires_at = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 600) as i64;
        let kind = SessionKind::User {
            login: "ada".into(),
            avatar_url: String::new(),
            installations: vec![1],
        };
        let session = Session {
            kind,
            expires_at,
            checked_at: now(),
            github: None,
        };
        store
            .create_session(&auth::hash("tok"), &session)
            .await
            .unwrap();
        (router(store, vault, config), "mp_session=tok".into())
    }

    fn web_config(hosted_app: Option<AppCredentials>, wake: Arc<Woken>) -> WebConfig {
        WebConfig {
            hosted_app,
            public_url: "http://localhost:8080".into(),
            setup_token: None,
            wake,
            repo_defaults: RepoSettings {
                label: "mergepatrol".into(),
                batch_size: 8,
                resolver: mergepatrol_core::ResolverSettings {
                    provider: Default::default(),
                    model: "m".into(),
                    effort: "medium".into(),
                },
                verify: Vec::new(),
                fix_ci: Default::default(),
                branches: Vec::new(),
            },
            analytics: Analytics::default(),
            billing: None,
            chatgpt: ChatGptAuth::default(),
        }
    }

    /// Another process on the same database: as after a restart, or on another instance.
    async fn restarted(pool: PgPool, hosted_app: Option<AppCredentials>) -> Router {
        let store: Arc<dyn Store> = Arc::new(PgStore::migrate(pool).await.unwrap());
        let vault = Arc::new(Vault::new(store.clone(), KEY).unwrap());
        router(store, vault, web_config(hosted_app, Arc::default()))
    }

    /// Where a redirect goes, and the cookies it sets.
    fn redirect(res: &Response) -> (String, Vec<String>) {
        let location = res.headers()["location"].to_str().unwrap().to_owned();
        let cookies = res
            .headers()
            .get_all("set-cookie")
            .iter()
            .map(|c| c.to_str().unwrap().split(';').next().unwrap().to_owned())
            .collect();
        (location, cookies)
    }

    fn query_param(url: &str, name: &str) -> String {
        url.split(['?', '&'])
            .find_map(|p| p.strip_prefix(&format!("{name}=")))
            .unwrap()
            .to_owned()
    }

    async fn send(router: &Router, req: Request<Body>) -> Response {
        router.clone().oneshot(req).await.unwrap()
    }

    #[sqlx::test(migrations = false)]
    async fn a_sign_in_survives_a_restart(pool: PgPool) {
        let (before, _) = with_app(pool.clone(), Some(app(None)), Arc::default()).await;
        let res = send(
            &before,
            Request::get("/auth/login").body(Body::empty()).unwrap(),
        )
        .await;
        let (to_github, cookies) = redirect(&res);
        let state = query_param(&to_github, "state");
        let oauth = cookies.iter().find(|c| c.starts_with("mp_oauth=")).unwrap();

        let after = restarted(pool, Some(app(None))).await;
        let callback = |cookie: Option<&str>| {
            let mut req = Request::get(format!("/auth/callback?code=c&state={state}"));
            if let Some(c) = cookie {
                req = req.header("cookie", c);
            }
            req.body(Body::empty()).unwrap()
        };
        // Past the check of the state, up to GitHub's exchange of the code, which fails here.
        let (back, _) = redirect(&send(&after, callback(Some(oauth))).await);
        assert!(back.starts_with("/?error="), "{back}");
        assert!(
            !back.contains("expired") && !back.contains("another"),
            "{back}"
        );
        // Without the browser's cookie, refused.
        let (back, _) = redirect(&send(&after, callback(None)).await);
        assert!(back.contains("another%20browser"), "{back}");
    }

    #[sqlx::test(migrations = false)]
    async fn a_tray_sign_in_returns_to_the_app(pool: PgPool) {
        let (router, _) = with_app(pool, Some(app(None)), Arc::default()).await;
        let app_state = "s".repeat(24);
        let challenge = Pkce::new().challenge;
        let login = Request::get(format!(
            "/auth/login?tray_port=4567&tray_state={app_state}&tray_challenge={challenge}"
        ))
        .body(Body::empty())
        .unwrap();
        let (to_github, cookies) = redirect(&send(&router, login).await);
        let state = query_param(&to_github, "state");
        let both = cookies.join("; ");
        assert!(
            both.contains(&format!("mp_tray=4567.{app_state}.{challenge}")),
            "{both}"
        );

        // Up to GitHub's exchange of the code, which fails here: the error goes to the app.
        let callback = Request::get(format!("/auth/callback?code=c&state={state}"))
            .header("cookie", &both)
            .body(Body::empty())
            .unwrap();
        let (back, cookies) = redirect(&send(&router, callback).await);
        assert!(
            back.starts_with(&format!(
                "http://127.0.0.1:4567/callback?state={app_state}&error="
            )),
            "{back}"
        );
        assert!(cookies.contains(&"mp_tray=".to_owned()), "{cookies:?}");

        // A malformed request never reaches GitHub.
        for bad in [
            format!("tray_port=4567&tray_state=x%26y&tray_challenge={challenge}"),
            format!("tray_port=4567&tray_state={app_state}"),
            format!("tray_port=4567&tray_state={app_state}&tray_challenge=short"),
        ] {
            let req = Request::get(format!("/auth/login?{bad}"))
                .body(Body::empty())
                .unwrap();
            let (back, _) = redirect(&send(&router, req).await);
            assert!(back.starts_with("/?error="), "{bad}: {back}");
        }
    }

    /// A store and vault on the router's database, as the router sees them.
    async fn store_and_vault(pool: PgPool) -> (Arc<dyn Store>, Vault) {
        let store: Arc<dyn Store> = Arc::new(PgStore::migrate(pool).await.unwrap());
        let vault = Vault::new(store.clone(), KEY).unwrap();
        (store, vault)
    }

    fn trade(code: &str, verifier: &str) -> Request<Body> {
        Request::post("/auth/tray/token")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({ "code": code, "verifier": verifier }).to_string(),
            ))
            .unwrap()
    }

    #[sqlx::test(migrations = false)]
    async fn the_app_trades_its_code_once_with_its_verifier(pool: PgPool) {
        let (router, _) = signed_in(pool.clone()).await;
        let (store, vault) = store_and_vault(pool).await;
        let pkce = Pkce::new();
        // The callback's work: the session "tok" (from `signed_in`), sealed under a code.
        let code = |name: &str| LoginCode {
            token_hash: auth::hash("tok"),
            challenge: pkce.challenge.clone(),
            token: vault
                .seal(&format!("login-code/{}", auth::hash(name)), "tok")
                .unwrap(),
            expires_at: now() + 60,
        };
        store
            .create_login_code(&auth::hash("c"), &code("c"))
            .await
            .unwrap();

        let res = send(&router, trade("c", &pkce.verifier)).await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), 1 << 16)
            .await
            .unwrap();
        assert_eq!(&body[..], br#"{"token":"tok"}"#);
        // Once only.
        let again = send(&router, trade("c", &pkce.verifier)).await;
        assert_eq!(again.status(), StatusCode::BAD_REQUEST);

        // Without the verifier, the code fails, and the session it carried goes with it.
        store
            .create_login_code(&auth::hash("d"), &code("d"))
            .await
            .unwrap();
        let stolen = send(&router, trade("d", &Pkce::new().verifier)).await;
        assert_eq!(stolen.status(), StatusCode::BAD_REQUEST);
        assert_eq!(store.session(&auth::hash("tok")).await.unwrap(), None);
    }

    /// The user session "old" for installation 1, as of `checked_at`, keeping `github`.
    async fn old_session(
        store: &dyn Store,
        vault: &Vault,
        checked_at: i64,
        github: Option<UserToken>,
    ) {
        let hash = auth::hash("old");
        let github = github.map(|g| {
            vault
                .seal(
                    &format!("session/{hash}"),
                    &serde_json::to_string(&g).unwrap(),
                )
                .unwrap()
        });
        let session = Session {
            kind: SessionKind::User {
                login: "ada".into(),
                avatar_url: String::new(),
                installations: vec![1],
            },
            expires_at: now() + 600,
            checked_at,
            github,
        };
        store.create_session(&hash, &session).await.unwrap();
    }

    fn expired_token() -> UserToken {
        UserToken {
            access_token: "ghu_x".into(),
            expires_at: Some(now() - 10),
            refresh_token: None,
        }
    }

    #[sqlx::test(migrations = false)]
    async fn a_session_ends_when_its_github_token_does(pool: PgPool) {
        let (router, _) = signed_in(pool.clone()).await;
        let (store, vault) = store_and_vault(pool).await;
        // Due a check, with an expired token and no refresh token: GitHub would refuse it, so
        // the session ends without asking.
        old_session(&*store, &vault, now() - 2 * 3600, Some(expired_token())).await;
        assert_eq!(
            get(&router, "/api/w/1/lanes", Some("mp_session=old")).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(store.session(&auth::hash("old")).await.unwrap(), None);
    }

    #[sqlx::test(migrations = false)]
    async fn a_session_checked_lately_or_from_before_is_not_checked(pool: PgPool) {
        let (router, _) = signed_in(pool.clone()).await;
        let (store, vault) = store_and_vault(pool).await;
        // An hour has not passed: the token, which GitHub would refuse, is not even read.
        old_session(&*store, &vault, now() - 60, Some(expired_token())).await;
        assert_eq!(
            get(&router, "/api/w/1/lanes", Some("mp_session=old")).await,
            StatusCode::OK
        );
        // A session from before sessions kept the token lasts out its time as it is.
        store.delete_session(&auth::hash("old")).await.unwrap();
        old_session(&*store, &vault, 0, None).await;
        assert_eq!(
            get(&router, "/api/w/1/lanes", Some("mp_session=old")).await,
            StatusCode::OK
        );
    }

    #[sqlx::test(migrations = false)]
    async fn the_queue_changes_only_as_the_user(pool: PgPool) {
        let (router, _) = signed_in(pool.clone()).await;
        let (store, vault) = store_and_vault(pool).await;
        let status = |method: &str, workspace: u64, cookie: &str| {
            let req = Request::builder()
                .method(method)
                .uri(format!("/api/w/{workspace}/repos/acme/app/pulls/7/queue"))
                .header("cookie", cookie)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"priority":true}"#))
                .unwrap();
            let router = router.clone();
            async move { send(&router, req).await.status() }
        };
        assert_eq!(
            status("PUT", 2, "mp_session=tok").await,
            StatusCode::NOT_FOUND
        );
        // Without the user's GitHub token, nothing can act as them: not the App in their stead.
        for method in ["PUT", "DELETE"] {
            assert_eq!(
                status(method, 1, "mp_session=tok").await,
                StatusCode::UNAUTHORIZED
            );
        }
        // Nor can it close a PR.
        let close = Request::post("/api/w/1/repos/acme/app/pulls/7/close")
            .header("cookie", "mp_session=tok")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            send(&router, close).await.status(),
            StatusCode::UNAUTHORIZED
        );
        old_session(&*store, &vault, now() - 60, Some(expired_token())).await;
        assert_eq!(
            status("PUT", 1, "mp_session=old").await,
            StatusCode::UNAUTHORIZED
        );
        // With a live token, the change goes on to GitHub: here, to the App there is none of yet.
        store.delete_session(&auth::hash("old")).await.unwrap();
        let live = UserToken {
            expires_at: Some(now() + 3600),
            ..expired_token()
        };
        old_session(&*store, &vault, now() - 60, Some(live)).await;
        for method in ["PUT", "DELETE"] {
            assert_eq!(
                status(method, 1, "mp_session=old").await,
                StatusCode::CONFLICT
            );
        }
    }

    #[sqlx::test(migrations = false)]
    async fn the_tray_reads_the_api_with_a_bearer_token(pool: PgPool) {
        let (router, _) = signed_in(pool).await;
        let req = |auth: &str| {
            Request::get("/api/w/1/lanes")
                .header("authorization", auth)
                .body(Body::empty())
                .unwrap()
        };
        assert_eq!(
            send(&router, req("Bearer tok")).await.status(),
            StatusCode::OK
        );
        assert_eq!(
            send(&router, req("Bearer nope")).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }

    /// Posts one JSON-RPC message to `/mcp`.
    async fn mcp(router: &Router, auth: Option<&str>, message: serde_json::Value) -> Response {
        let mut req = Request::post("/mcp").header("content-type", "application/json");
        if let Some(auth) = auth {
            req = req.header("authorization", auth);
        }
        send(router, req.body(Body::from(message.to_string())).unwrap()).await
    }

    async fn json_of(res: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await;
        serde_json::from_slice(&bytes.unwrap()).unwrap()
    }

    /// A router, with the session "old" holding a live GitHub token: a bearer for `/mcp` that
    /// GitHub need not check.
    async fn mcp_router(pool: PgPool) -> Router {
        let (router, _) = signed_in(pool.clone()).await;
        let (store, vault) = store_and_vault(pool).await;
        let live = UserToken {
            expires_at: Some(now() + 3600),
            ..expired_token()
        };
        old_session(&*store, &vault, now() - 60, Some(live)).await;
        router
    }

    #[sqlx::test(migrations = false)]
    async fn mcp_takes_a_github_token_or_a_session_that_holds_one(pool: PgPool) {
        let router = mcp_router(pool).await;
        let ping = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });
        let res = mcp(&router, None, ping.clone()).await;
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        // Where an MCP client finds the sign-in.
        assert_eq!(
            res.headers()["www-authenticate"],
            r#"Bearer resource_metadata="http://localhost:8080/.well-known/oauth-protected-resource/mcp""#
        );
        // The dashboard's cookie is not enough.
        let req = Request::post("/mcp")
            .header("content-type", "application/json")
            .header("cookie", "mp_session=old")
            .body(Body::from(ping.to_string()))
            .unwrap();
        assert_eq!(send(&router, req).await.status(), StatusCode::UNAUTHORIZED);
        // A session without a GitHub token cannot act: the client signs in again.
        let res = mcp(&router, Some("Bearer tok"), ping.clone()).await;
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let res = mcp(&router, Some("Bearer old"), ping).await;
        assert_eq!(json_of(res).await["result"], serde_json::json!({}));
    }

    #[sqlx::test(migrations = false)]
    async fn mcp_speaks_the_protocol(pool: PgPool) {
        let router = mcp_router(pool).await;
        let auth = Some("Bearer old");
        let init = |version: &str| {
            serde_json::json!({
                "jsonrpc": "2.0", "id": "a", "method": "initialize",
                "params": { "protocolVersion": version, "capabilities": {}, "clientInfo": { "name": "t", "version": "1" } },
            })
        };
        let res = json_of(mcp(&router, auth, init("2025-03-26")).await).await;
        assert_eq!(res["id"], "a");
        assert_eq!(res["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(res["result"]["serverInfo"]["name"], "mergepatrol");
        // A version it does not know gets the newest it does.
        let res = json_of(mcp(&router, auth, init("1999-01-01")).await).await;
        assert_eq!(res["result"]["protocolVersion"], "2025-06-18");

        let initialized =
            serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert_eq!(
            mcp(&router, auth, initialized).await.status(),
            StatusCode::ACCEPTED
        );

        let list = serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" });
        let res = json_of(mcp(&router, auth, list).await).await;
        let tools = res["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), routes::tools().count());
        let tool = |name: &str| tools.iter().find(|t| t["name"] == name).unwrap().clone();
        // The repo names the workspace.
        let queue = tool("queue_pull");
        assert_eq!(
            queue["inputSchema"]["required"],
            serde_json::json!(["repo", "number"])
        );
        assert_eq!(queue["annotations"]["readOnlyHint"], false);
        assert_eq!(
            tool("list_repos")["inputSchema"]["required"],
            serde_json::json!(["workspace"])
        );
        assert_eq!(
            tool("delete_api_key")["annotations"]["destructiveHint"],
            true
        );

        let unknown = serde_json::json!({ "jsonrpc": "2.0", "id": 3, "method": "resources/list" });
        let res = json_of(mcp(&router, auth, unknown).await).await;
        assert_eq!(res["error"]["code"], -32601);

        let req = Request::get("/mcp").body(Body::empty()).unwrap();
        assert_eq!(
            send(&router, req).await.status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
    }

    #[sqlx::test(migrations = false)]
    async fn an_mcp_tool_tells_the_agent_what_went_wrong(pool: PgPool) {
        let router = mcp_router(pool).await;
        let call = |name: &str, arguments: serde_json::Value| {
            serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": name, "arguments": arguments },
            })
        };
        let text = |res: serde_json::Value| {
            assert_eq!(res["result"]["isError"], true);
            res["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        let auth = Some("Bearer old");
        let res = mcp(&router, auth, call("merge_now", serde_json::json!({}))).await;
        assert_eq!(text(json_of(res).await), "No tool merge_now.");
        let res = mcp(
            &router,
            auth,
            call("queue_pull", serde_json::json!({ "repo": "acme/app" })),
        )
        .await;
        assert!(text(json_of(res).await).starts_with("Bad arguments: missing field `number`"));
        let args = serde_json::json!({ "repo": "acme", "number": 7 });
        let res = mcp(&router, auth, call("queue_pull", args)).await;
        assert_eq!(text(json_of(res).await), "`repo` must be owner/name.");
        // Before it asks GitHub anything.
        let args = serde_json::json!({ "repo": "acme/app", "number": 7 });
        let res = mcp(&router, auth, call("pull_status", args)).await;
        assert_eq!(text(json_of(res).await), "No GitHub App yet.");
    }

    /// No tool's route answers anyone it does not know: the session status alone is public.
    #[sqlx::test(migrations = false)]
    async fn every_tool_route_needs_a_caller(pool: PgPool) {
        let (router, _) = signed_in(pool).await;
        for (route, tool) in routes::tools() {
            if route.path == "/api/session" {
                continue;
            }
            let path = route
                .path
                .replace("{id}", "1")
                .replace("{owner}/{name}", "acme/app")
                .replace("{number}", "7")
                .replace("{provider}", "anthropic")
                .replace("{login}", "ada");
            for auth in [
                None,
                Some(("authorization", "Bearer nope")),
                Some(("cookie", "mp_session=nope")),
            ] {
                let mut req = Request::builder()
                    .method(route.method.clone())
                    .uri(&path)
                    .header("content-type", "application/json");
                if let Some((name, value)) = auth {
                    req = req.header(name, value);
                }
                let res = send(&router, req.body(Body::from("{}")).unwrap()).await;
                assert_eq!(
                    res.status(),
                    StatusCode::UNAUTHORIZED,
                    "{} with {auth:?}",
                    tool.name
                );
            }
        }
    }

    /// A tool is its route, called as the agent.
    #[sqlx::test(migrations = false)]
    async fn an_mcp_tool_calls_its_route(pool: PgPool) {
        let router = mcp_router(pool).await;
        let call = |name: &str, arguments: serde_json::Value| {
            serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": name, "arguments": arguments },
            })
        };
        let result = |res: serde_json::Value| {
            let text = res["result"]["content"][0]["text"].as_str().unwrap();
            (res["result"]["isError"].as_bool().unwrap(), text.to_owned())
        };
        let auth = Some("Bearer old");
        let res = mcp(&router, auth, call("get_session", serde_json::json!({}))).await;
        let (error, text) = result(json_of(res).await);
        assert!(!error);
        let session: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(session["user"]["login"], "ada");
        let res = mcp(
            &router,
            auth,
            call("list_queue", serde_json::json!({ "workspace": 1 })),
        )
        .await;
        assert_eq!(result(json_of(res).await), (false, "[]".into()));
        // The route checks who may: not the tool.
        let res = mcp(
            &router,
            auth,
            call("list_repos", serde_json::json!({ "workspace": 2 })),
        )
        .await;
        assert_eq!(
            result(json_of(res).await),
            (true, "no such workspace".into())
        );
        let args = serde_json::json!({ "workspace": 1, "days": 400 });
        let res = mcp(&router, auth, call("list_history", args)).await;
        assert_eq!(
            result(json_of(res).await),
            (true, "days must be from 1 to 365".into())
        );
    }

    #[sqlx::test(migrations = false)]
    async fn an_mcp_client_finds_how_to_sign_in(pool: PgPool) {
        let (router, _) = signed_in(pool).await;
        let get = |path: &str| {
            let req = Request::get(path).body(Body::empty()).unwrap();
            let router = router.clone();
            async move { json_of(send(&router, req).await).await }
        };
        let resource = get("/.well-known/oauth-protected-resource/mcp").await;
        assert_eq!(resource["resource"], "http://localhost:8080/mcp");
        assert_eq!(
            resource["authorization_servers"],
            serde_json::json!(["http://localhost:8080"])
        );
        let server = get("/.well-known/oauth-authorization-server").await;
        assert_eq!(server["issuer"], "http://localhost:8080");
        assert_eq!(
            server["authorization_endpoint"],
            "http://localhost:8080/oauth/authorize"
        );
        assert_eq!(
            server["code_challenge_methods_supported"],
            serde_json::json!(["S256"])
        );

        let register = |uris: serde_json::Value| {
            let body = serde_json::json!({ "client_name": "Claude Code", "redirect_uris": uris });
            let req = Request::post("/oauth/register")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap();
            let router = router.clone();
            async move { send(&router, req).await }
        };
        let res = register(serde_json::json!(["http://localhost:9999/callback"])).await;
        assert_eq!(res.status(), StatusCode::CREATED);
        assert!(json_of(res).await["client_id"].is_string());
        // A redirect off this machine could carry the code anywhere.
        let res = register(serde_json::json!(["https://evil.example/callback"])).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(json_of(res).await["error"], "invalid_redirect_uri");
    }

    #[sqlx::test(migrations = false)]
    async fn an_mcp_sign_in_returns_to_the_client(pool: PgPool) {
        let (router, _) = with_app(pool, Some(app(None)), Arc::default()).await;
        let challenge = Pkce::new().challenge;
        let authorize = |query: String| {
            let req = Request::get(format!("/oauth/authorize?{query}"))
                .body(Body::empty())
                .unwrap();
            let router = router.clone();
            async move { send(&router, req).await }
        };
        let redirect_uri = "http%3A%2F%2F127.0.0.1%3A9999%2Fcallback";
        let res = authorize(format!(
            "response_type=code&client_id=c&redirect_uri={redirect_uri}&state=s%20t&code_challenge={challenge}&code_challenge_method=S256"
        ))
        .await;
        let (to_github, cookies) = redirect(&res);
        assert!(to_github.starts_with("https://github.com/"), "{to_github}");
        let state = query_param(&to_github, "state");
        assert!(
            cookies
                .iter()
                .any(|c| c.starts_with("mp_mcp=") && c.len() > 7)
        );
        assert!(cookies.contains(&"mp_tray=".to_owned()), "{cookies:?}");

        // Up to GitHub's exchange of the code, which fails here: the error goes to the client.
        let callback = Request::get(format!("/auth/callback?code=c&state={state}"))
            .header("cookie", cookies.join("; "))
            .body(Body::empty())
            .unwrap();
        let (back, cookies) = redirect(&send(&router, callback).await);
        assert!(
            back.starts_with(
                "http://127.0.0.1:9999/callback?error=access_denied&error_description="
            ),
            "{back}"
        );
        assert!(back.ends_with("&state=s%20t"), "{back}");
        assert!(cookies.contains(&"mp_mcp=".to_owned()), "{cookies:?}");

        // A cookie planted to send the code off this machine is not followed.
        use base64::Engine;
        let planted = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            serde_json::json!({ "redirect_uri": "https://evil.example/cb", "state": null, "challenge": challenge })
                .to_string(),
        );
        let res = authorize(format!(
            "response_type=code&client_id=c&redirect_uri={redirect_uri}&code_challenge={challenge}&code_challenge_method=S256"
        ))
        .await;
        let (to_github, cookies) = redirect(&res);
        let oauth = cookies.iter().find(|c| c.starts_with("mp_oauth=")).unwrap();
        let callback = Request::get(format!(
            "/auth/callback?code=c&state={}",
            query_param(&to_github, "state")
        ))
        .header("cookie", format!("{oauth}; mp_mcp={planted}"))
        .body(Body::empty())
        .unwrap();
        let (back, _) = redirect(&send(&router, callback).await);
        assert!(back.starts_with("/?error="), "{back}");

        // Without PKCE, back to the client before GitHub.
        let res = authorize(format!(
            "response_type=code&client_id=c&redirect_uri={redirect_uri}&state=x"
        ))
        .await;
        let (back, _) = redirect(&res);
        assert!(
            back.starts_with("http://127.0.0.1:9999/callback?error=invalid_request"),
            "{back}"
        );
        assert!(back.ends_with("&state=x"), "{back}");
        // Without a loopback redirect, nowhere.
        let res = authorize(format!(
            "response_type=code&client_id=c&redirect_uri=https%3A%2F%2Fevil.example%2Fcb&code_challenge={challenge}&code_challenge_method=S256"
        ))
        .await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[sqlx::test(migrations = false)]
    async fn an_mcp_client_trades_its_code_for_a_session(pool: PgPool) {
        let (router, _) = signed_in(pool.clone()).await;
        let (store, vault) = store_and_vault(pool).await;
        let pkce = Pkce::new();
        let code = LoginCode {
            token_hash: auth::hash("tok"),
            challenge: pkce.challenge.clone(),
            token: vault
                .seal(&format!("login-code/{}", auth::hash("c")), "tok")
                .unwrap(),
            expires_at: now() + 60,
        };
        store
            .create_login_code(&auth::hash("c"), &code)
            .await
            .unwrap();
        let trade = |form: String| {
            let req = Request::post("/oauth/token")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(form))
                .unwrap();
            let router = router.clone();
            async move { send(&router, req).await }
        };
        let form = format!(
            "grant_type=authorization_code&code=c&code_verifier={}&client_id=x&redirect_uri=http%3A%2F%2F127.0.0.1%3A9999%2Fcallback",
            pkce.verifier
        );
        let res = trade(form.clone()).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers()["cache-control"], "no-store");
        let body = json_of(res).await;
        assert_eq!(body["access_token"], "tok");
        assert_eq!(body["token_type"], "Bearer");
        // Once only.
        let res = trade(form).await;
        assert_eq!(json_of(res).await["error"], "invalid_grant");
        let res = trade("grant_type=refresh_token&refresh_token=r".into()).await;
        assert_eq!(json_of(res).await["error"], "unsupported_grant_type");
    }

    #[sqlx::test(migrations = false)]
    async fn creating_the_app_survives_a_restart(pool: PgPool) {
        let (before, _) = with_app(pool.clone(), None, Arc::default()).await;
        let store = PgStore::migrate(pool.clone()).await.unwrap();
        let expires_at = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 600) as i64;
        let setup = Session {
            kind: SessionKind::Setup,
            expires_at,
            checked_at: 0,
            github: None,
        };
        store
            .create_session(&auth::hash("setup"), &setup)
            .await
            .unwrap();
        let start = Request::post("/api/setup/manifest")
            .header("cookie", "mp_session=setup")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"owner":"acme","organization":true}"#))
            .unwrap();
        let res = send(&before, start).await;
        assert_eq!(res.status(), StatusCode::OK);
        let manifest = res
            .headers()
            .get_all("set-cookie")
            .iter()
            .map(|c| c.to_str().unwrap().split(';').next().unwrap().to_owned())
            .find(|c| c.starts_with("mp_manifest="))
            .unwrap();
        let state = manifest.strip_prefix("mp_manifest=").unwrap().to_owned();

        let after = restarted(pool, None).await;
        let callback = |cookie: Option<&str>| {
            let mut req = Request::get(format!("/setup/callback?code=c&state={state}"));
            if let Some(c) = cookie {
                req = req.header("cookie", c);
            }
            req.body(Body::empty()).unwrap()
        };
        // Past the checks, up to GitHub's conversion of the code, which fails here.
        let both = format!("mp_session=setup; {manifest}");
        let (back, _) = redirect(&send(&after, callback(Some(&both))).await);
        assert!(back.starts_with("/?error="), "{back}");
        assert!(
            !back.contains("expired") && !back.contains("setup%20token"),
            "{back}"
        );
        let (back, _) = redirect(&send(&after, callback(Some("mp_session=setup"))).await);
        assert!(back.contains("expired"), "{back}");
        // Anyone can set the cookie to match the state: without the setup session, refused.
        let forged = Request::get("/setup/callback?code=c&state=x")
            .header("cookie", "mp_manifest=x")
            .body(Body::empty())
            .unwrap();
        let (back, _) = redirect(&send(&after, forged).await);
        assert!(back.contains("setup%20token"), "{back}");
    }

    async fn get(router: &Router, path: &str, cookie: Option<&str>) -> StatusCode {
        let mut req = Request::get(path);
        if let Some(c) = cookie {
            req = req.header("cookie", c);
        }
        router
            .clone()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }

    /// Another process on the same database, with billing on.
    async fn billed(pool: PgPool) -> Router {
        let pg = Arc::new(PgStore::migrate(pool).await.unwrap());
        let store: Arc<dyn Store> = pg.clone();
        let vault = Arc::new(Vault::new(store.clone(), KEY).unwrap());
        let stripe = mergepatrol_billing::StripeConfig {
            secret_key: "sk_test".into(),
            webhook_secret: "whsec".into(),
            price: "price".into(),
        };
        let mut config = web_config(None, Arc::default());
        config.billing = Some(Arc::new(Billing::new(pg, stripe, "http://localhost:8080")));
        router(store, vault, config)
    }

    async fn body(router: &Router, path: &str, cookie: &str) -> String {
        let req = Request::get(path).header("cookie", cookie);
        let res = send(router, req.body(Body::empty()).unwrap()).await;
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await;
        String::from_utf8(bytes.unwrap().to_vec()).unwrap()
    }

    #[sqlx::test(migrations = false)]
    async fn billing_shows_only_when_on(pool: PgPool) {
        let (off, cookie) = signed_in(pool.clone()).await;
        assert_eq!(body(&off, "/api/w/1/billing", &cookie).await, "null");
        let on = billed(pool).await;
        let view: serde_json::Value =
            serde_json::from_str(&body(&on, "/api/w/1/billing", &cookie).await).unwrap();
        assert_eq!(view["access"], "new");
        assert_eq!(view["pricing"]["price_usd"], 15);
        assert_eq!(
            get(&on, "/api/w/2/billing", Some(&cookie)).await,
            StatusCode::NOT_FOUND
        );
    }

    #[sqlx::test(migrations = false)]
    async fn a_stripe_webhook_needs_billing_and_its_signature(pool: PgPool) {
        let stripe = |router: Router, signature: &'static str| async move {
            let req = Request::post("/webhooks/stripe")
                .header("stripe-signature", signature)
                .body(Body::from("{}"))
                .unwrap();
            send(&router, req).await.status()
        };
        let off = restarted(pool.clone(), None).await;
        assert_eq!(stripe(off, "t=1,v1=00").await, StatusCode::NOT_FOUND);
        let on = billed(pool).await;
        assert_eq!(stripe(on, "t=1,v1=00").await, StatusCode::UNAUTHORIZED);
    }

    #[sqlx::test(migrations = false)]
    async fn a_user_reaches_only_their_workspaces(pool: PgPool) {
        let (router, cookie) = signed_in(pool).await;
        assert_eq!(
            get(&router, "/api/w/1/lanes", Some(&cookie)).await,
            StatusCode::OK
        );
        assert_eq!(
            get(&router, "/api/w/1/settings", Some(&cookie)).await,
            StatusCode::OK
        );
        assert_eq!(
            get(&router, "/api/w/2/lanes", Some(&cookie)).await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            get(&router, "/api/w/2/settings", Some(&cookie)).await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            get(&router, "/api/w/2/events?repo=o/r", Some(&cookie)).await,
            StatusCode::NOT_FOUND
        );
        // Checked before GitHub sees it: it goes into the request's path.
        assert_eq!(
            get(&router, "/api/w/1/fix?repo=o/r&sha=../../x", Some(&cookie)).await,
            StatusCode::BAD_REQUEST
        );
        let sha = "0".repeat(40);
        assert_eq!(
            get(
                &router,
                &format!("/api/w/2/fix?repo=o/r&sha={sha}"),
                Some(&cookie)
            )
            .await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            get(&router, "/api/w/2/repos", Some(&cookie)).await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            get(&router, "/api/w/2/repos/o/r/config", Some(&cookie)).await,
            StatusCode::NOT_FOUND
        );
    }

    #[sqlx::test(migrations = false)]
    async fn a_repo_with_a_slash_in_its_owner_is_refused(pool: PgPool) {
        let (router, cookie) = signed_in(pool).await;
        let put = |path: &str| {
            Request::put(path)
                .header("cookie", &cookie)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"enabled":true}"#))
                .unwrap()
        };
        let status =
            |req: Request<Body>| async { router.clone().oneshot(req).await.unwrap().status() };
        assert_eq!(
            status(put("/api/w/1/repos/o%2Fx/y")).await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status(put("/api/w/1/repos/o/y")).await,
            StatusCode::NO_CONTENT
        );
    }

    #[sqlx::test(migrations = false)]
    async fn crawlers_see_the_servers_own_address(pool: PgPool) {
        let (router, _) = signed_in(pool).await;
        assert!(
            body(&router, "/robots.txt", "")
                .await
                .contains("Sitemap: http://localhost:8080/sitemap.xml")
        );
        assert!(
            body(&router, "/sitemap.xml", "")
                .await
                .contains("<loc>http://localhost:8080/</loc>")
        );
        assert_eq!(get(&router, "/og.png", None).await, StatusCode::NOT_FOUND);
        let page = body(&router, "/ada/queue", "").await;
        // CI builds ui/ before the tests. Without the build there is no page to fill in.
        if !page.contains("not built") {
            assert!(page.contains(r#"<link rel="canonical" href="http://localhost:8080/""#));
            assert!(!page.contains("__PUBLIC_URL__"));
            // A link preview fetches og:image by its absolute URL; a 404 there is a blank card.
            let card = "/og/mergepatrol-og-1200x630.png";
            assert!(page.contains(&format!(r#"content="http://localhost:8080{card}""#)));
            assert_eq!(get(&router, card, None).await, StatusCode::OK);
        }
    }

    #[sqlx::test(migrations = false)]
    async fn signed_out_gets_nothing(pool: PgPool) {
        let (router, _) = signed_in(pool).await;
        assert_eq!(
            get(&router, "/api/w/1/lanes", None).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            get(&router, "/api/w/1/lanes", Some("mp_session=forged")).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(get(&router, "/api/session", None).await, StatusCode::OK);
    }

    fn app(webhook_secret: Option<&str>) -> AppCredentials {
        AppCredentials {
            id: 1,
            slug: "mp".into(),
            client_id: "c".into(),
            client_secret: "s".into(),
            webhook_secret: webhook_secret.map(str::to_owned),
            pem: String::new(),
            owner: "o".into(),
            html_url: "https://github.com/apps/mp".into(),
        }
    }

    async fn deliver(router: &Router, body: &str, signature: &str) -> StatusCode {
        let req = Request::post("/webhooks/github")
            .header("x-github-event", "pull_request")
            .header("x-hub-signature-256", signature)
            .body(Body::from(body.to_owned()))
            .unwrap();
        router.clone().oneshot(req).await.unwrap().status()
    }

    const DELIVERY: &str =
        r#"{"action":"labeled","installation":{"id":7},"repository":{"full_name":"o/r"}}"#;

    #[sqlx::test(migrations = false)]
    async fn a_signed_webhook_wakes_its_repo(pool: PgPool) {
        let woken = Arc::new(Woken::default());
        let (router, _) = with_app(pool, Some(app(Some("hush"))), woken.clone()).await;
        let signature = webhook::sign("hush", DELIVERY.as_bytes());
        assert_eq!(
            deliver(&router, DELIVERY, &signature).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(*woken.0.lock().unwrap(), [(7, "o/r".to_owned())]);
    }

    #[sqlx::test(migrations = false)]
    async fn a_big_delivery_still_wakes_its_repo(pool: PgPool) {
        let woken = Arc::new(Woken::default());
        let (router, _) = with_app(pool, Some(app(Some("hush"))), woken.clone()).await;
        // A push with many commits: over axum's default 2 MB.
        let filler = "x".repeat(3 * 1024 * 1024);
        let body = format!(
            r#"{{"action":"labeled","installation":{{"id":7}},"repository":{{"full_name":"o/r"}},"filler":"{filler}"}}"#
        );
        let signature = webhook::sign("hush", body.as_bytes());
        assert_eq!(
            deliver(&router, &body, &signature).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(*woken.0.lock().unwrap(), [(7, "o/r".to_owned())]);
    }

    #[sqlx::test(migrations = false)]
    async fn a_delivery_whose_write_fails_still_wakes_its_repo(pool: PgPool) {
        let woken = Arc::new(Woken::default());
        let (router, _) = with_app(pool.clone(), Some(app(Some("hush"))), woken.clone()).await;
        // The write fails, as when the database is away.
        sqlx::query("DROP TABLE label_heads")
            .execute(&pool)
            .await
            .unwrap();
        let body = r#"{"action":"labeled","installation":{"id":7},"repository":{"full_name":"o/r"},"label":{"name":"mp"},"pull_request":{"number":1,"head":{"sha":"s1","repo":{"full_name":"fork/r"}}}}"#;
        let signature = webhook::sign("hush", body.as_bytes());
        assert_eq!(
            deliver(&router, body, &signature).await,
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(*woken.0.lock().unwrap(), [(7, "o/r".to_owned())]);
    }

    #[sqlx::test(migrations = false)]
    async fn labeling_a_fork_pr_keeps_its_head_then(pool: PgPool) {
        let store: Arc<dyn Store> = Arc::new(PgStore::migrate(pool.clone()).await.unwrap());
        let (router, _) = with_app(pool, Some(app(Some("hush"))), Arc::default()).await;
        let labeled = |number: u32, head_repo: &str| {
            format!(
                r#"{{"action":"labeled","installation":{{"id":7}},"repository":{{"full_name":"o/r"}},"label":{{"name":"mp"}},"pull_request":{{"number":{number},"head":{{"sha":"s{number}","repo":{{"full_name":"{head_repo}"}}}}}}}}"#
            )
        };
        for body in [labeled(1, "fork/r"), labeled(2, "o/r")] {
            let signature = webhook::sign("hush", body.as_bytes());
            assert_eq!(
                deliver(&router, &body, &signature).await,
                StatusCode::NO_CONTENT
            );
        }
        let repo = mergepatrol_core::RepoName::parse("o/r").unwrap();
        let head = async |pr| store.label_head(7, &repo, pr, "mp").await.unwrap();
        assert_eq!(head(1).await.as_deref(), Some("s1"));
        // A PR from the repo itself needs no pin.
        assert_eq!(head(2).await, None);
    }

    #[sqlx::test(migrations = false)]
    async fn a_webhook_marks_what_it_changed_for_the_queue_to_read_again(pool: PgPool) {
        let store: Arc<dyn Store> = Arc::new(PgStore::migrate(pool.clone()).await.unwrap());
        let (router, _) = with_app(pool, Some(app(Some("hush"))), Arc::default()).await;
        let repo = mergepatrol_core::RepoName::parse("o/r").unwrap();
        // The queue reads the repo.
        store.github_reads(7, &repo).await.unwrap();

        let edited = r#"{"action":"edited","installation":{"id":7},"repository":{"full_name":"o/r"},"pull_request":{"number":3,"head":{"sha":"s","repo":{"full_name":"o/r"}}}}"#;
        let signature = webhook::sign("hush", edited.as_bytes());
        deliver(&router, edited, &signature).await;
        deliver_push(&router, &push("refs/heads/topic", "")).await;
        let reads = store.github_reads(7, &repo).await.unwrap();
        assert_eq!(reads.changes, 0);
        assert_eq!(
            reads
                .prs
                .iter()
                .map(|p| (p.number, p.changes))
                .collect::<Vec<_>>(),
            [(3, 1)]
        );

        deliver_push(&router, &push("refs/heads/main", "")).await;
        assert_eq!(store.github_reads(7, &repo).await.unwrap().changes, 1);
    }

    #[sqlx::test(migrations = false)]
    async fn a_label_change_from_the_dashboard_marks_its_prs_for_the_queue(pool: PgPool) {
        let store: Arc<dyn Store> = Arc::new(PgStore::migrate(pool).await.unwrap());
        let woken = Woken::default();
        let repo = mergepatrol_core::RepoName::parse("o/r").unwrap();
        store.github_reads(7, &repo).await.unwrap();

        api::relabeled(&*store, &woken, 7, repo.clone(), &[3, 4]).await;
        let reads = store.github_reads(7, &repo).await.unwrap();
        assert_eq!(
            reads
                .prs
                .iter()
                .map(|p| (p.number, p.changes))
                .collect::<Vec<_>>(),
            [(3, 1), (4, 1)]
        );
        assert_eq!(*woken.0.lock().unwrap(), [(7, "o/r".to_string())]);
    }

    #[sqlx::test(migrations = false)]
    async fn a_webhook_marks_the_reads_it_changed(pool: PgPool) {
        let store: Arc<dyn Store> = Arc::new(PgStore::migrate(pool.clone()).await.unwrap());
        let (router, _) = with_app(pool, Some(app(Some("hush"))), Arc::default()).await;
        let repo = mergepatrol_core::RepoName::parse("o/r").unwrap();
        let keys = ["checks:abc", "checks:def", "ref:topic", "stack:4", "pull:3"];
        for key in keys {
            store.github_value(7, &repo, key).await.unwrap();
        }
        let send = async |event: &str, body: &str| {
            let req = Request::post("/webhooks/github")
                .header("x-github-event", event)
                .header(
                    "x-hub-signature-256",
                    webhook::sign("hush", body.as_bytes()),
                )
                .body(Body::from(body.to_owned()))
                .unwrap();
            router.clone().oneshot(req).await.unwrap().status()
        };
        let base =
            r#""installation":{"id":7},"repository":{"full_name":"o/r","default_branch":"main"}"#;
        let check = format!(r#"{{"action":"completed",{base},"check_run":{{"head_sha":"abc"}}}}"#);
        assert_eq!(send("check_run", &check).await, StatusCode::NO_CONTENT);
        send(
            "push",
            &format!(r#"{{"ref":"refs/heads/topic",{base},"commits":[]}}"#),
        )
        .await;
        let pr = format!(
            r#"{{"action":"synchronize",{base},"pull_request":{{"number":3,"head":{{"sha":"s","repo":{{"full_name":"o/r"}}}}}}}}"#
        );
        send("pull_request", &pr).await;

        let mut changes = Vec::new();
        for key in keys {
            changes.push(store.github_value(7, &repo, key).await.unwrap().changes);
        }
        assert_eq!(changes, [1, 0, 1, 1, 1]);
    }

    async fn deliver_push(router: &Router, body: &str) -> StatusCode {
        let req = Request::post("/webhooks/github")
            .header("x-github-event", "push")
            .header(
                "x-hub-signature-256",
                webhook::sign("hush", body.as_bytes()),
            )
            .body(Body::from(body.to_owned()))
            .unwrap();
        router.clone().oneshot(req).await.unwrap().status()
    }

    async fn deliver_event(router: &Router, event: &str, body: &str) -> StatusCode {
        let req = Request::post("/webhooks/github")
            .header("x-github-event", event)
            .header(
                "x-hub-signature-256",
                webhook::sign("hush", body.as_bytes()),
            )
            .body(Body::from(body.to_owned()))
            .unwrap();
        router.clone().oneshot(req).await.unwrap().status()
    }

    /// A PR as the inbox reads it, by `author`, asked of the team `core`, with a comment by
    /// `ann`.
    fn inbox_node(number: u64, author: &str) -> serde_json::Value {
        serde_json::json!({
            "number": number, "state": "OPEN", "title": "t", "body": "", "isDraft": false,
            "isCrossRepository": false, "reviewDecision": "REVIEW_REQUIRED", "baseRefName": "main",
            "headRefName": "b", "headRefOid": "h", "labels": { "nodes": [] }, "stack": null,
            "url": "u", "createdAt": "2026-10-01T00:00:00Z", "updatedAt": "2026-10-02T00:00:00Z", "additions": 1, "deletions": 1,
            "changedFiles": 1, "author": { "__typename": "User", "login": author, "avatarUrl": "a" },
            "latestOpinionatedReviews": { "nodes": [] }, "reviewRequests": { "nodes": [] },
            "commits": { "nodes": [] }, "repository": { "nameWithOwner": "o/r" },
            "teamRequests": { "nodes": [{ "requestedReviewer": { "slug": "core" } }] },
            "activity": { "nodes": [{ "createdAt": "2026-10-02T00:00:00Z", "author": { "login": "ann" } }] },
        })
    }

    #[sqlx::test(migrations = false)]
    async fn webhooks_keep_the_inbox_and_the_you_tab_reads_it(pool: PgPool) {
        use mergepatrol_store::{InboxRead, InboxWrite};
        let store: Arc<dyn Store> = Arc::new(PgStore::migrate(pool.clone()).await.unwrap());
        let (router, cookie) = with_app(pool, Some(app(Some("hush"))), Arc::default()).await;
        let repo = mergepatrol_core::RepoName::parse("o/r").unwrap();

        // A comment on a PR marks it; one on an issue does not.
        let on_pr = r#"{"action":"created","installation":{"id":1},"repository":{"full_name":"o/r"},"issue":{"number":5,"pull_request":{"url":"u"}}}"#;
        let on_issue = r#"{"action":"created","installation":{"id":1},"repository":{"full_name":"o/r"},"issue":{"number":6}}"#;
        deliver_event(&router, "issue_comment", on_pr).await;
        deliver_event(&router, "issue_comment", on_issue).await;
        let stale = store
            .inbox_stale(std::time::Duration::ZERO, 10)
            .await
            .unwrap();
        assert_eq!(stale.iter().map(|s| s.number).collect::<Vec<_>>(), [5]);
        // Ada joins the team the PR asks.
        let joined = r#"{"action":"added","installation":{"id":1},"team":{"slug":"core"},"member":{"login":"Ada"}}"#;
        assert_eq!(
            deliver_event(&router, "membership", joined).await,
            StatusCode::NO_CONTENT
        );
        let node = inbox_node(5, "bo");
        let read = InboxRead {
            state: "open".into(),
            author: Some("bo".into()),
            requested_users: Vec::new(),
            requested_teams: vec!["core".into()],
            reviewers: Vec::new(),
            closed_at: None,
            node,
        };
        let write = InboxWrite {
            number: 5,
            changes: stale[0].changes,
            read: Some(read),
            settled: true,
        };
        store.save_inbox_reads(1, &repo, &[write]).await.unwrap();

        let you = |cookie: String| {
            let router = router.clone();
            async move {
                let req = Request::get("/api/you")
                    .header("cookie", cookie)
                    .body(Body::empty())
                    .unwrap();
                let res = router.oneshot(req).await.unwrap();
                assert_eq!(res.status(), StatusCode::OK);
                let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
                    .await
                    .unwrap();
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()
            }
        };
        let view = you(cookie.clone()).await;
        let asked = &view["review_requested"][0];
        assert_eq!(
            (asked["repo"].as_str(), asked["number"].as_u64()),
            (Some("o/r"), Some(5))
        );
        assert_eq!(
            (asked["workspace"].as_u64(), asked["unread"].as_u64()),
            (Some(1), Some(1))
        );
        assert_eq!(view["authored"], serde_json::json!([]));

        // Opened here: read.
        let seen = Request::put("/api/w/1/repos/o/r/pulls/5/seen")
            .header("cookie", cookie.clone())
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            router.clone().oneshot(seen).await.unwrap().status(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            you(cookie.clone()).await["review_requested"][0]["unread"],
            0
        );

        // Left the team: no longer asked.
        let left = joined.replace("added", "removed");
        deliver_event(&router, "membership", &left).await;
        assert_eq!(you(cookie).await["review_requested"], serde_json::json!([]));
    }

    fn push(r#ref: &str, commits: &str) -> String {
        format!(
            r#"{{"ref":"{}","installation":{{"id":7}},"repository":{{"full_name":"o/r","default_branch":"main"}},"commits":[{}]}}"#,
            r#ref, commits
        )
    }

    #[sqlx::test(migrations = false)]
    async fn a_push_of_the_config_file_turns_its_repo_on(pool: PgPool) {
        let store: Arc<dyn Store> = Arc::new(PgStore::migrate(pool.clone()).await.unwrap());
        let (router, _) = with_app(pool, Some(app(Some("hush"))), Arc::default()).await;
        let on = async || -> Vec<bool> {
            store
                .repos()
                .await
                .unwrap()
                .iter()
                .map(|s| s.on())
                .collect()
        };
        let added = r#"{"added":[".mergepatrol/mergepatrol.yml"],"removed":[],"modified":[]}"#;
        let removed = r#"{"added":[],"removed":[".mergepatrol/mergepatrol.yml"],"modified":[]}"#;
        let other = r#"{"added":["a.txt"],"removed":[],"modified":[]}"#;

        // Another branch, or another file, changes nothing.
        deliver_push(&router, &push("refs/heads/topic", added)).await;
        deliver_push(&router, &push("refs/heads/main", other)).await;
        assert!(on().await.is_empty());

        deliver_push(
            &router,
            &push("refs/heads/main", &format!("{added},{other}")),
        )
        .await;
        assert_eq!(on().await, [true]);
        deliver_push(
            &router,
            &push("refs/heads/main", &format!("{added},{removed}")),
        )
        .await;
        assert_eq!(on().await, [false]);
    }

    #[sqlx::test(migrations = false)]
    async fn a_badly_signed_webhook_wakes_nothing(pool: PgPool) {
        let woken = Arc::new(Woken::default());
        let (router, _) = with_app(pool, Some(app(Some("hush"))), woken.clone()).await;
        let forged = webhook::sign("guess", DELIVERY.as_bytes());
        assert_eq!(
            deliver(&router, DELIVERY, &forged).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            deliver(&router, DELIVERY, "").await,
            StatusCode::UNAUTHORIZED
        );
        assert!(woken.0.lock().unwrap().is_empty());
    }

    #[sqlx::test(migrations = false)]
    async fn without_a_webhook_secret_webhooks_are_off(pool: PgPool) {
        let woken = Arc::new(Woken::default());
        let (router, _) = with_app(pool, Some(app(None)), woken.clone()).await;
        let signature = webhook::sign("", DELIVERY.as_bytes());
        assert_eq!(
            deliver(&router, DELIVERY, &signature).await,
            StatusCode::NOT_FOUND
        );
        assert!(woken.0.lock().unwrap().is_empty());
    }

    #[sqlx::test(migrations = false)]
    async fn a_workspace_signs_in_to_chatgpt(pool: PgPool) {
        let issuer = mergepatrol_resolver::testing::issuer().await;
        let config = WebConfig {
            chatgpt: ChatGptAuth::new(&issuer.url),
            ..web_config(None, Arc::default())
        };
        let (router, cookie) = with_config(pool, config).await;
        let call = |method: &str, path: &str| {
            Request::builder()
                .method(method)
                .uri(format!("/api/w/1/settings/chatgpt{path}"))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap()
        };
        let settings = |res: Response| async move {
            assert_eq!(res.status(), StatusCode::OK);
            let body = axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .unwrap();
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["chatgpt"].clone()
        };

        let started = settings(send(&router, call("POST", "")).await).await;
        assert_eq!(started["sign_in"]["user_code"], "ABCD-1234");
        assert!(started["login"].is_null());
        // The user has yet to enter the code.
        let waiting = settings(send(&router, call("POST", "/poll")).await).await;
        assert_eq!(waiting["sign_in"]["user_code"], "ABCD-1234");

        let done = settings(send(&router, call("POST", "/poll")).await).await;
        assert!(done["sign_in"].is_null());
        assert_eq!(done["login"]["hint"], "me@example.com");
        let res = send(&router, call("POST", "/poll")).await;
        assert_eq!(res.status(), StatusCode::CONFLICT);

        let removed = settings(send(&router, call("DELETE", "")).await).await;
        assert!(removed["login"].is_null());
    }

    /// A router whose session "old" holds the GitHub token `ghu_x`, and GitHub as if it had
    /// said that token, and `ghp_y`, read `o/r` (installation 1) and `o/off` (no installation).
    async fn v1_router(pool: PgPool) -> (Router, Arc<dyn Store>) {
        let (store, vault) = store_and_vault(pool).await;
        let live = UserToken {
            expires_at: Some(now() + 3600),
            ..expired_token()
        };
        old_session(&*store, &vault, now() - 60, Some(live)).await;
        let state = state(
            store.clone(),
            Arc::new(vault),
            web_config(Some(app(None)), Arc::default()),
        );
        for token in ["ghu_x", "ghp_y"] {
            let r = |name: &str| RepoName::parse(name).unwrap();
            state.seen.assume(token, &r("o/r"), Some(1));
            state.seen.assume(token, &r("o/off"), None);
        }
        (serve(state), store)
    }

    async fn v1_get(router: &Router, path: &str, headers: &[(&str, &str)]) -> Response {
        let mut req = Request::get(path);
        for (name, value) in headers {
            req = req.header(*name, *value);
        }
        send(router, req.body(Body::empty()).unwrap()).await
    }

    const BEARER: (&str, &str) = ("authorization", "Bearer ghp_y");

    #[sqlx::test(migrations = false)]
    async fn v1_says_where_each_pr_is(pool: PgPool) {
        use mergepatrol_core::{Entry, EntryState, Hold, Lane, PrSummary};
        use mergepatrol_store::LaneKey;
        let (router, store) = v1_router(pool).await;
        let repo = RepoName::parse("o/r").unwrap();
        store.set_has_config(1, &repo, true).await.unwrap();
        let key = LaneKey {
            installation_id: 1,
            repo: repo.clone(),
            trunk: "main".into(),
        };
        let entry = |number, state, layers: Vec<u64>| Entry {
            number,
            priority: number == 5,
            state,
            summary: PrSummary::default(),
            layers,
            heads: Vec::new(),
        };
        let held = Hold::Layer {
            number: 6,
            hold: Box::new(Hold::Draft),
        };
        let lane = Lane {
            entries: vec![
                entry(7, EntryState::Held(held), vec![6, 7]),
                entry(5, EntryState::Waiting, Vec::new()),
                entry(4, EntryState::Active, Vec::new()),
            ],
            batch_pr: Some(99),
            ..Lane::default()
        };
        store.save_lane(&key, &lane).await.unwrap();
        let summary = PrSummary::default();
        store
            .record_exit(&key, 8, "merged", "abc", &summary)
            .await
            .unwrap();
        store
            .record_exit(&key, 9, "ejected", "checks failed", &summary)
            .await
            .unwrap();
        // Ejected once, and queued again: the ejection is over.
        store
            .record_exit(&key, 5, "ejected", "old", &summary)
            .await
            .unwrap();

        let res = v1_get(&router, "/api/v1/repos/o/r/pulls/6", &[BEARER]).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers()["x-ratelimit-limit"], "5000");
        assert_eq!(res.headers()["x-ratelimit-remaining"], "4999");
        let etag = res.headers()["etag"].to_str().unwrap().to_owned();
        assert_eq!(
            json_of(res).await,
            serde_json::json!({
                "repo": "o/r",
                "number": 6,
                "url": "http://localhost:8080/o/r/pull/6",
                "mergepatrol": {
                    "state": "on",
                    "installed": true,
                    "has_config": true,
                    "override": null,
                    "config_error": null,
                },
                "queue": {
                    "trunk": "main",
                    "state": "held",
                    "hold": {
                        "reason": "draft",
                        "layer": 6,
                        "message": "#6 is not ready: the PR is a draft",
                    },
                    "priority": false,
                    "position": 3,
                    "stack": { "number": 7, "layers": [6, 7] },
                    "batch_pr": null,
                },
                "merged": null,
                "ejected": null,
            })
        );
        // Unchanged: nothing to send again.
        let res = v1_get(
            &router,
            "/api/v1/repos/o/r/pulls/6",
            &[BEARER, ("if-none-match", &etag)],
        )
        .await;
        assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(res.headers()["etag"], etag.as_str());
        assert_eq!(res.headers()["x-ratelimit-remaining"], "4998");

        let res = v1_get(
            &router,
            "/api/v1/repos/o/r/pulls?numbers=4,5,8,9,10",
            &[BEARER],
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
        let pulls = json_of(res).await["pulls"].clone();
        let at = |i: usize, field: &str| pulls[i][field].clone();
        assert_eq!(at(0, "number"), 4);
        assert_eq!(at(0, "queue")["state"], "running");
        assert_eq!(at(0, "queue")["position"], 1);
        assert_eq!(at(0, "queue")["batch_pr"], 99);
        assert_eq!(at(1, "queue")["state"], "waiting");
        assert_eq!(at(1, "queue")["priority"], true);
        assert_eq!(at(1, "ejected"), serde_json::Value::Null);
        assert_eq!(at(2, "queue"), serde_json::Value::Null);
        assert_eq!(at(2, "merged")["sha"], "abc");
        assert_eq!(at(3, "ejected")["reason"], "checks failed");
        assert_eq!(at(3, "merged"), serde_json::Value::Null);
        // Never queued.
        for field in ["queue", "merged", "ejected"] {
            assert_eq!(at(4, field), serde_json::Value::Null);
        }
    }

    #[sqlx::test(migrations = false)]
    async fn v1_says_whether_a_repo_uses_mergepatrol(pool: PgPool) {
        let (router, store) = v1_router(pool).await;
        let repo = RepoName::parse("o/r").unwrap();
        let status = |router: Router, path: &'static str| async move {
            let res = v1_get(&router, path, &[BEARER]).await;
            assert_eq!(res.status(), StatusCode::OK);
            json_of(res).await["mergepatrol"].clone()
        };
        let off = status(router.clone(), "/api/v1/repos/o/off/pulls/1").await;
        assert_eq!(off["state"], "off");
        assert_eq!(off["installed"], false);
        // Installed, without a config file.
        let r = "/api/v1/repos/o/r/pulls/1";
        let no_file = status(router.clone(), r).await;
        assert_eq!(
            (&no_file["state"], &no_file["installed"]),
            (&"off".into(), &true.into())
        );
        store.set_repo_enabled(1, &repo, Some(true)).await.unwrap();
        let forced = status(router.clone(), r).await;
        assert_eq!(
            (&forced["state"], &forced["override"]),
            (&"on".into(), &"on".into())
        );
        store
            .set_config_error(1, &repo, Some("bad yaml"))
            .await
            .unwrap();
        let paused = status(router.clone(), r).await;
        assert_eq!(paused["state"], "paused");
        assert_eq!(paused["config_error"], "bad yaml");
        store.set_repo_enabled(1, &repo, Some(false)).await.unwrap();
        let turned_off = status(router.clone(), r).await;
        assert_eq!(
            (&turned_off["state"], &turned_off["override"]),
            (&"off".into(), &"off".into())
        );
    }

    #[sqlx::test(migrations = false)]
    async fn v1_takes_a_github_token_or_a_session(pool: PgPool) {
        let (router, _) = v1_router(pool).await;
        let path = "/api/v1/repos/o/off/pulls/1";
        let res = v1_get(&router, path, &[]).await;
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        // The session's own GitHub token reads it, by header or by the dashboard's cookie.
        let res = v1_get(&router, path, &[("authorization", "Bearer old")]).await;
        assert_eq!(res.status(), StatusCode::OK);
        let res = v1_get(&router, path, &[("cookie", "mp_session=old")]).await;
        assert_eq!(res.status(), StatusCode::OK);
        // Each credential has its own budget.
        assert_eq!(res.headers()["x-ratelimit-remaining"], "4998");
        let bad = "/api/v1/repos/o/off/pulls?numbers=1,x";
        assert_eq!(
            v1_get(&router, bad, &[BEARER]).await.status(),
            StatusCode::BAD_REQUEST
        );
    }
}
