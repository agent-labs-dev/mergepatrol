//! The HTTP server: the JSON API under `/api`, and the dashboard (the built `ui/`, embedded).
//! The API types derive `TS`. `cargo test` writes them to `ui/src/api/`.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use mergepatrol_core::{EntryState, RepoName};
use mergepatrol_store::Store;
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

#[derive(Deserialize)]
struct EventsQuery {
    repo: String,
    pr: Option<u64>,
}

pub fn router(store: Arc<dyn Store>) -> Router {
    Router::new()
        .route("/api/health", get(|| async { "ok" }))
        .route("/api/lanes", get(lanes))
        .route("/api/events", get(events))
        .fallback(ui)
        .with_state(store)
}

async fn lanes(State(store): State<Arc<dyn Store>>) -> Result<Json<Vec<LaneView>>, ApiError> {
    let lanes = store.lanes().await?;
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
    State(store): State<Arc<dyn Store>>,
    Query(q): Query<EventsQuery>,
) -> Result<Json<Vec<EventView>>, ApiError> {
    let repo = RepoName::parse(&q.repo).ok_or(ApiError::BadRequest("repo must be owner/name"))?;
    let events = store.events(&repo, q.pr, 200).await?;
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
    BadRequest(&'static str),
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self::Internal(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            Self::BadRequest(m) => (StatusCode::BAD_REQUEST, m).into_response(),
            Self::Internal(e) => {
                tracing::error!(error = %e, "API error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
            }
        }
    }
}
