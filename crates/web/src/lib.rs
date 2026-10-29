//! The HTTP server: the JSON API under `/api`, sign-in and setup, and the dashboard (the built
//! `ui/`, embedded). The API types derive `TS`. `cargo test` writes them to `ui/src/api/`.

mod api;
mod auth;
mod billing;
mod ui;
mod webhook;

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
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
    let state = AppState {
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
    };
    Router::new()
        .route("/api/health", get(|| async { "ok" }))
        .route("/api/session", get(auth::session))
        .route("/api/setup/unlock", post(auth::unlock))
        .route("/api/setup/manifest", post(auth::start_manifest))
        .route("/setup/callback", get(auth::manifest_callback))
        .route("/setup/installed", get(auth::installed))
        .route("/auth/login", get(auth::login))
        .route("/auth/callback", get(auth::login_callback))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/tray/token", post(auth::tray_token))
        .route(
            "/webhooks/github",
            post(webhook::receive).layer(DefaultBodyLimit::max(webhook::MAX_BODY_BYTES)),
        )
        .route("/webhooks/stripe", post(billing::webhook))
        // A workspace is one installation of the App: `{id}` is its installation ID.
        .route("/api/w/{id}/lanes", get(api::lanes))
        .route("/api/w/{id}/events", get(api::events))
        .route("/api/w/{id}/fix", get(api::fix))
        .route("/api/w/{id}/history", get(api::history))
        .route("/api/w/{id}/history/stats", get(api::history_stats))
        .route("/api/w/{id}/repos", get(api::repos))
        .route("/api/w/{id}/repos/{owner}/{name}", put(api::set_repo))
        .route(
            "/api/w/{id}/repos/{owner}/{name}/open",
            get(api::open_pulls),
        )
        .route(
            "/api/w/{id}/repos/{owner}/{name}/pulls/{number}/queue",
            put(api::queue_pull).delete(api::dequeue_pull),
        )
        .route(
            "/api/w/{id}/repos/{owner}/{name}/config",
            get(api::repo_config).put(api::propose_repo_config),
        )
        .route("/api/w/{id}/settings", get(api::settings))
        .route(
            "/api/w/{id}/settings/api-keys/{provider}",
            put(api::set_api_key).delete(api::delete_api_key),
        )
        .route(
            "/api/w/{id}/settings/chatgpt",
            post(api::start_chatgpt_login).delete(api::delete_chatgpt_login),
        )
        .route(
            "/api/w/{id}/settings/chatgpt/poll",
            post(api::poll_chatgpt_login),
        )
        .route("/api/w/{id}/billing", get(billing::billing))
        .route("/api/w/{id}/billing/checkout", post(billing::checkout))
        .route("/api/w/{id}/billing/portal", post(billing::portal))
        .route(
            "/api/w/{id}/billing/seats/{login}",
            axum::routing::delete(billing::free_seat),
        )
        .fallback(ui::serve)
        .with_state(state)
}

enum ApiError {
    /// A message for the user.
    Status(StatusCode, String),
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
            Some(limited) => Self::Status(StatusCode::SERVICE_UNAVAILABLE, format!("{limited}.")),
            None => Self::Internal(e),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            Self::Status(code, m) => (code, m).into_response(),
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
}
