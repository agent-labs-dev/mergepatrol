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
use mergepatrol_store::{GITHUB_APP, Store, Vault};

pub use api::*;
pub use auth::{
    AppView, ManifestForm, ManifestRequest, SessionStatus, SessionView, Unlock, UserView,
};

pub struct WebConfig {
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
    setup_token: Arc<Mutex<Option<String>>>,
    /// OAuth and manifest flows in progress, by their `state` parameter.
    pending: Arc<Mutex<HashMap<String, auth::Pending>>>,
}

impl AppState {
    async fn app_credentials(&self) -> anyhow::Result<Option<AppCredentials>> {
        match self.vault.get(GITHUB_APP).await? {
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
        .route("/api/lanes", get(api::lanes))
        .route("/api/events", get(api::events))
        .route("/api/repos", get(api::repos))
        .route("/api/repos/{owner}/{name}", put(api::set_repo))
        .route("/api/settings", get(api::settings))
        .route(
            "/api/settings/anthropic-api-key",
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
