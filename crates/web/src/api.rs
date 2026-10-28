use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use mergepatrol_core::{EntryState, RepoName};
use mergepatrol_github::App;
use mergepatrol_resolver::{KeyCheck, check_anthropic_key};
use mergepatrol_store::{ANTHROPIC_API_KEY, Scope};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::auth::SignedIn;
use crate::{ApiError, AppState};

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
pub struct RepoView {
    pub repo: String,
    pub private: bool,
    pub enabled: bool,
}

#[derive(Deserialize, TS)]
#[ts(export)]
pub struct SetRepo {
    pub enabled: bool,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct SettingsView {
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

#[derive(Deserialize)]
pub(crate) struct EventsQuery {
    repo: String,
    pr: Option<u64>,
}

pub(crate) async fn lanes(
    State(app): State<AppState>,
    user: SignedIn,
    Path(id): Path<u64>,
) -> Result<Json<Vec<LaneView>>, ApiError> {
    let id = user.workspace(id)?;
    let lanes = app.store.lanes(id).await?;
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

pub(crate) async fn events(
    State(app): State<AppState>,
    user: SignedIn,
    Path(id): Path<u64>,
    Query(q): Query<EventsQuery>,
) -> Result<Json<Vec<EventView>>, ApiError> {
    let id = user.workspace(id)?;
    let repo = RepoName::parse(&q.repo).ok_or(ApiError::bad_request("repo must be owner/name"))?;
    let events = app.store.events(id, &repo, q.pr, 200).await?;
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

/// The repos this workspace's installation can access, and whether Mergepatrol runs on each.
pub(crate) async fn repos(
    State(app): State<AppState>,
    user: SignedIn,
    Path(id): Path<u64>,
) -> Result<Json<Vec<RepoView>>, ApiError> {
    let id = user.workspace(id)?;
    let creds = app
        .app_credentials()
        .await?
        .ok_or_else(|| ApiError::Status(StatusCode::CONFLICT, "No GitHub App yet.".into()))?;
    let enabled: HashMap<RepoName, bool> = app
        .store
        .repo_settings()
        .await?
        .into_iter()
        .filter(|s| s.installation_id == id)
        .map(|s| (s.repo, s.enabled))
        .collect();
    let mut views: Vec<RepoView> = App::from_credentials(&creds)?
        .installation(id)
        .repositories()
        .await?
        .into_iter()
        .map(|r| RepoView {
            enabled: enabled.get(&r.repo).copied().unwrap_or(false),
            repo: r.repo.to_string(),
            private: r.private,
        })
        .collect();
    views.sort_by(|a, b| a.repo.cmp(&b.repo));
    Ok(Json(views))
}

pub(crate) async fn set_repo(
    State(app): State<AppState>,
    user: SignedIn,
    Path((id, owner, name)): Path<(u64, String, String)>,
    Json(body): Json<SetRepo>,
) -> Result<StatusCode, ApiError> {
    let id = user.workspace(id)?;
    let repo = RepoName { owner, name };
    app.store.set_repo_enabled(id, &repo, body.enabled).await?;
    tracing::info!(installation = id, %repo, enabled = body.enabled, "repo setting changed");
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn settings(
    State(app): State<AppState>,
    user: SignedIn,
    Path(id): Path<u64>,
) -> Result<Json<SettingsView>, ApiError> {
    settings_view(&app, user.workspace(id)?).await
}

async fn settings_view(app: &AppState, id: u64) -> Result<Json<SettingsView>, ApiError> {
    let key = app
        .vault
        .status(Scope::Installation(id), ANTHROPIC_API_KEY)
        .await?;
    Ok(Json(SettingsView {
        anthropic_api_key: key.map(|k| SecretView {
            hint: k.hint,
            updated_at: k.updated_at,
        }),
    }))
}

/// Checks the key with Anthropic before it stores it, so a typo fails here and not in a rebase.
pub(crate) async fn set_anthropic_key(
    State(app): State<AppState>,
    user: SignedIn,
    Path(id): Path<u64>,
    Json(body): Json<SetSecret>,
) -> Result<Json<SettingsView>, ApiError> {
    let id = user.workspace(id)?;
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
    app.vault
        .set(Scope::Installation(id), ANTHROPIC_API_KEY, key)
        .await?;
    tracing::info!(installation = id, "the Anthropic API key was changed");
    settings_view(&app, id).await
}

pub(crate) async fn delete_anthropic_key(
    State(app): State<AppState>,
    user: SignedIn,
    Path(id): Path<u64>,
) -> Result<Json<SettingsView>, ApiError> {
    let id = user.workspace(id)?;
    app.vault
        .delete(Scope::Installation(id), ANTHROPIC_API_KEY)
        .await?;
    tracing::info!(installation = id, "the Anthropic API key was removed");
    settings_view(&app, id).await
}
