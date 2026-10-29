//! The HTTP server: the JSON API under `/api`, sign-in and setup, and the dashboard (the built
//! `ui/`, embedded). The API types derive `TS`. `cargo test` writes them to `ui/src/api/`.

mod api;
mod auth;
mod ui;
mod webhook;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use mergepatrol_analytics::Analytics;
use mergepatrol_core::RepoSettings;
use mergepatrol_github::AppCredentials;
use mergepatrol_store::{GITHUB_APP, Scope, Store, Vault};
use secrecy::ExposeSecret;

pub use api::*;
pub use auth::{
    AnalyticsView, AppView, ManifestForm, ManifestRequest, SessionStatus, SessionView, Unlock,
    UserView,
};
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
}

#[derive(Clone)]
struct AppState {
    store: Arc<dyn Store>,
    vault: Arc<Vault>,
    public_url: Arc<str>,
    hosted_app: Option<Arc<AppCredentials>>,
    setup_token: Arc<Mutex<Option<String>>>,
    /// OAuth and manifest flows in progress, by their `state` parameter.
    pending: Arc<Mutex<HashMap<String, auth::Pending>>>,
    wake: Arc<dyn Wake>,
    repo_defaults: Arc<RepoSettings>,
    analytics: Analytics,
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
        pending: Arc::default(),
        wake: config.wake,
        repo_defaults: Arc::new(config.repo_defaults),
        analytics: config.analytics,
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
        .route(
            "/webhooks/github",
            post(webhook::receive).layer(DefaultBodyLimit::max(webhook::MAX_BODY_BYTES)),
        )
        // A workspace is one installation of the App: `{id}` is its installation ID.
        .route("/api/w/{id}/lanes", get(api::lanes))
        .route("/api/w/{id}/events", get(api::events))
        .route("/api/w/{id}/history", get(api::history))
        .route("/api/w/{id}/history/stats", get(api::history_stats))
        .route("/api/w/{id}/repos", get(api::repos))
        .route("/api/w/{id}/repos/{owner}/{name}", put(api::set_repo))
        .route(
            "/api/w/{id}/repos/{owner}/{name}/config",
            get(api::repo_config).put(api::propose_repo_config),
        )
        .route("/api/w/{id}/settings", get(api::settings))
        .route(
            "/api/w/{id}/settings/api-keys/{provider}",
            put(api::set_api_key).delete(api::delete_api_key),
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
        Self::Internal(e)
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
    use mergepatrol_store::{PgStore, Session, SessionKind};
    use sqlx::PgPool;
    use tower::ServiceExt;

    const KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

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
        store
            .create_session(&auth::hash("tok"), &Session { kind, expires_at })
            .await
            .unwrap();
        let config = WebConfig {
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
        };
        (router(store, vault, config), "mp_session=tok".into())
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
}
