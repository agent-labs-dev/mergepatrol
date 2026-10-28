//! The HTTP server: the JSON API under `/api`, and the dashboard (the built `ui/`, embedded).
//! The API types derive `TS`. `cargo test` writes them to `ui/src/api/`.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use axum::{Json, Router};
use mergepatrol_core::{EntryState, RepoName};
use mergepatrol_resolver::{KeyCheck, check_anthropic_key};
use mergepatrol_store::{ANTHROPIC_API_KEY, Store, Vault};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, TS)]
#[ts(export)]
pub struct LaneView {
    pub repo: String,
    pub trunk: String,
    pub entries: Vec<EntryView>,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct EntryView {
    pub number: u32,
    pub priority: bool,
    pub state: EntryStateView,
    /// Why the PR is held.
    pub detail: Option<String>,
}

#[derive(Serialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum EntryStateView {
    Waiting,
    Held,
    Active,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct EventView {
    pub repo: String,
    pub pr: u32,
    pub at: String,
    pub kind: String,
    pub message: String,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct SettingsView {
    /// False when MERGEPATROL_SECRET_KEY is not set: secrets cannot be stored.
    pub secrets_enabled: bool,
    pub anthropic_api_key: Option<SecretView>,
}

/// A stored secret. The API never returns the value.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct SecretView {
    /// The last four characters.
    pub hint: String,
    pub updated_at: String,
}

#[derive(Deserialize, TS)]
#[ts(export)]
pub struct SetSecret {
    pub value: String,
}

#[derive(Clone)]
struct AppState {
    store: Arc<dyn Store>,
    vault: Arc<Vault>,
}

#[derive(Deserialize)]
struct EventsQuery {
    repo: String,
    pr: Option<u64>,
}

pub fn router(store: Arc<dyn Store>, vault: Arc<Vault>) -> Router {
    Router::new()
        .route("/api/health", get(|| async { "ok" }))
        .route("/api/lanes", get(lanes))
        .route("/api/events", get(events))
        .route("/api/settings", get(settings))
        .route(
            "/api/settings/anthropic-api-key",
            put(set_anthropic_key).delete(delete_anthropic_key),
        )
        .fallback(ui)
        .with_state(AppState { store, vault })
}

async fn lanes(State(app): State<AppState>) -> Result<Json<Vec<LaneView>>, ApiError> {
    let lanes = app.store.lanes().await?;
    Ok(Json(
        lanes
            .into_iter()
            .map(|(key, lane)| LaneView {
                repo: key.repo.to_string(),
                trunk: key.trunk,
                entries: lane
                    .entries
                    .into_iter()
                    .map(|e| {
                        let (state, detail) = match e.state {
                            EntryState::Waiting => (EntryStateView::Waiting, None),
                            EntryState::Held(reason) => (EntryStateView::Held, Some(reason)),
                            EntryState::Active => (EntryStateView::Active, None),
                        };
                        EntryView {
                            number: e.number as u32,
                            priority: e.priority,
                            state,
                            detail,
                        }
                    })
                    .collect(),
            })
            .collect(),
    ))
}

async fn events(
    State(app): State<AppState>,
    Query(q): Query<EventsQuery>,
) -> Result<Json<Vec<EventView>>, ApiError> {
    let repo = RepoName::parse(&q.repo).ok_or(ApiError::bad_request("repo must be owner/name"))?;
    let events = app.store.events(&repo, q.pr, 200).await?;
    Ok(Json(
        events
            .into_iter()
            .map(|e| EventView {
                repo: e.repo,
                pr: e.pr as u32,
                at: e.at,
                kind: e.kind,
                message: e.message,
            })
            .collect(),
    ))
}

async fn settings(State(app): State<AppState>) -> Result<Json<SettingsView>, ApiError> {
    let key = app.vault.status(ANTHROPIC_API_KEY).await?;
    Ok(Json(SettingsView {
        secrets_enabled: app.vault.enabled(),
        anthropic_api_key: key.map(|k| SecretView {
            hint: k.hint,
            updated_at: k.updated_at,
        }),
    }))
}

/// Checks the key with Anthropic before it stores it, so a typo fails here and not in a rebase.
async fn set_anthropic_key(
    State(app): State<AppState>,
    Json(body): Json<SetSecret>,
) -> Result<Json<SettingsView>, ApiError> {
    if !app.vault.enabled() {
        return Err(ApiError::Status(
            StatusCode::CONFLICT,
            "Set MERGEPATROL_SECRET_KEY first. Generate one with `openssl rand -base64 32`.".into(),
        ));
    }
    let key = body.value.trim();
    if key.is_empty() {
        return Err(ApiError::bad_request("the key is empty"));
    }
    match check_anthropic_key(key).await {
        Ok(KeyCheck::Valid) => {}
        Ok(KeyCheck::Invalid(why)) => {
            return Err(ApiError::Status(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("Anthropic rejected the key: {why}"),
            ));
        }
        Err(e) => return Err(ApiError::Status(StatusCode::BAD_GATEWAY, format!("{e:#}"))),
    }
    app.vault.set(ANTHROPIC_API_KEY, key).await?;
    tracing::info!("the Anthropic API key was changed");
    settings(State(app)).await
}

async fn delete_anthropic_key(State(app): State<AppState>) -> Result<Json<SettingsView>, ApiError> {
    app.vault.delete(ANTHROPIC_API_KEY).await?;
    tracing::info!("the Anthropic API key was removed");
    settings(State(app)).await
}

#[derive(rust_embed::RustEmbed)]
#[folder = "../../ui/dist"]
#[allow_missing = true]
struct UiAssets;

/// Serves the dashboard. Unknown paths get `index.html`, so client-side routes work.
async fn ui(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let (path, file) = match UiAssets::get(path) {
        Some(f) if !path.is_empty() => (path, f),
        _ => match UiAssets::get("index.html") {
            Some(f) => ("index.html", f),
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    "The dashboard is not built. Run `pnpm build` in ui/.",
                )
                    .into_response();
            }
        },
    };
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    ([(header::CONTENT_TYPE, mime.as_ref())], file.data).into_response()
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
                tracing::error!(error = %e, "API error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
            }
        }
    }
}
