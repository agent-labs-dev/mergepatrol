//! The HTTP server: the JSON API under `/api`, sign-in and setup, and the dashboard (the built
//! `ui/`, embedded). The API types derive `TS`. `cargo test` writes them to `ui/src/api/`.

mod api;
mod auth;
mod ui;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use mergepatrol_github::AppCredentials;
use mergepatrol_store::{GITHUB_APP, Scope, Store, Vault};

pub use api::*;
pub use auth::{
    AppView, ManifestForm, ManifestRequest, SessionStatus, SessionView, Unlock, UserView,
};

pub struct WebConfig {
    /// The hosted GitHub App, from the environment. `None` self-hosted: the App is created in
    /// the dashboard and kept in the vault.
    pub hosted_app: Option<AppCredentials>,
    /// Where users reach the dashboard, for example `http://localhost:8080`. GitHub redirects here.
    pub public_url: String,
    /// Unlocks the first-run setup. `None` once the GitHub App exists.
    pub setup_token: Option<String>,
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
}

impl AppState {
    async fn app_credentials(&self) -> anyhow::Result<Option<AppCredentials>> {
        if let Some(app) = &self.hosted_app {
            return Ok(Some((**app).clone()));
        }
        match self.vault.get(Scope::Instance, GITHUB_APP).await? {
            Some(json) => Ok(Some(serde_json::from_str(&json)?)),
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
        // A workspace is one installation of the App: `{id}` is its installation ID.
        .route("/api/w/{id}/lanes", get(api::lanes))
        .route("/api/w/{id}/events", get(api::events))
        .route("/api/w/{id}/repos", get(api::repos))
        .route("/api/w/{id}/repos/{owner}/{name}", put(api::set_repo))
        .route("/api/w/{id}/settings", get(api::settings))
        .route(
            "/api/w/{id}/settings/anthropic-api-key",
            put(api::set_anthropic_key).delete(api::delete_anthropic_key),
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

    /// A router, and the cookie of a user who can access installation 1 only.
    async fn signed_in(pool: PgPool) -> (Router, String) {
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
            hosted_app: None,
            public_url: "http://localhost:8080".into(),
            setup_token: None,
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
}
