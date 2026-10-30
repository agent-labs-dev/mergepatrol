//! The versioned API under `/api/v1`, for other apps: what Mergepatrol knows of a PR, read with
//! the caller's GitHub token or a Mergepatrol session, as MCP reads it. Each response carries an
//! `ETag`, and the caller's rate limit.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use mergepatrol_core::{EntryState, Hold, PrNumber, RepoName};
use mergepatrol_store::{ExitRecord, RepoState};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::api::in_merge_order;
use crate::auth::{Caller, hash, session_token};
use crate::{ApiError, AppState};

/// The most PRs one batch read takes.
const MAX_BATCH: usize = 100;
/// The calls a caller may make in each `WINDOW_SECS`.
const LIMIT: u32 = 5000;
const WINDOW_SECS: u64 = 3600;

/// What Mergepatrol knows of a PR. `queue`, `merged` and `ejected` are all null for a PR that
/// was never in a queue.
#[derive(Clone, Serialize, TS)]
#[ts(export)]
pub struct PullStateView {
    /// `owner/name`.
    pub repo: String,
    pub number: u32,
    /// The PR in the dashboard: a permalink that opens wherever the PR is, queued or not, merged
    /// or ejected.
    pub url: String,
    /// Whether the repo uses Mergepatrol.
    pub mergepatrol: RepoStatusView,
    /// Its place in the queue. Null when it is not queued.
    pub queue: Option<QueueView>,
    /// It last left the queue merged. Null while it is queued.
    pub merged: Option<MergedView>,
    /// It last left the queue ejected. Null while it is queued.
    pub ejected: Option<EjectedView>,
}

/// A repo's queue: whether it runs, and why.
#[derive(Clone, Serialize, TS)]
#[ts(export)]
pub struct RepoStatusView {
    pub state: RepoStateView,
    /// The Mergepatrol GitHub App is installed on the repo.
    pub installed: bool,
    /// The default branch has `.mergepatrol/mergepatrol.yml`, as of the last look.
    pub has_config: bool,
    /// Turned on or off in the dashboard, whatever the config file says. Null follows the file.
    #[serde(rename = "override")]
    pub forced: Option<SwitchView>,
    /// Why the queue is paused: the config file does not parse.
    pub config_error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum RepoStateView {
    /// The queue runs.
    On,
    /// No App on the repo, no config file, or turned off in the dashboard.
    Off,
    /// On, but the config file does not parse: nothing merges until it does.
    Paused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum SwitchView {
    On,
    Off,
}

/// A queued PR's place.
#[derive(Clone, Serialize, TS)]
#[ts(export)]
pub struct QueueView {
    /// The branch the queue merges it into.
    pub trunk: String,
    pub state: QueueStateView,
    /// Why it is held. Null unless `state` is `held`.
    pub hold: Option<HoldView>,
    pub priority: bool,
    /// Its place in the order the queue takes PRs, from 1: running PRs first, held PRs last.
    pub position: u32,
    /// The stack it is a layer of. Null for a PR on its own.
    pub stack: Option<StackView>,
    /// The draft PR that tests it in a batch, while it runs in one.
    pub batch_pr: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum QueueStateView {
    Waiting,
    /// Rebasing, testing or merging.
    Running,
    /// It keeps its place, and goes on once the hold lifts.
    Held,
}

#[derive(Clone, Serialize, TS)]
#[ts(export)]
pub struct HoldView {
    pub reason: HoldReasonView,
    /// The stack's layer that holds it, for a stack.
    pub layer: Option<u32>,
    /// The reason as the PR's comment says it.
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum HoldReasonView {
    Draft,
    /// A required review is missing.
    ReviewRequired,
    /// A reviewer whose approval is required requested changes.
    ChangesRequested,
    /// A fork PR's workflow runs wait for a maintainer to approve them.
    WorkflowApproval,
    /// A fork PR behind its trunk, whose author has not allowed edits by maintainers.
    ForkEditsOff,
    /// It targets a branch without a queue.
    NoQueue,
    /// A private repo without a Mergepatrol subscription.
    NoSubscription,
    /// The account's public repos are over Mergepatrol's fair-use limit for the hour.
    FairUseLimit,
}

#[derive(Clone, Serialize, TS)]
#[ts(export)]
pub struct StackView {
    /// The stack's number, as the queue lists it.
    pub number: u32,
    /// Its PRs, bottom to top.
    pub layers: Vec<u32>,
}

#[derive(Clone, Serialize, TS)]
#[ts(export)]
pub struct MergedView {
    pub at: String,
    /// The merge commit.
    pub sha: String,
}

#[derive(Clone, Serialize, TS)]
#[ts(export)]
pub struct EjectedView {
    pub at: String,
    pub reason: String,
}

/// The PRs of a batch read, in the order asked.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct PullsView {
    pub pulls: Vec<PullStateView>,
}

impl From<&Hold> for HoldView {
    fn from(hold: &Hold) -> Self {
        let reason = match hold.innermost() {
            Hold::Draft => HoldReasonView::Draft,
            Hold::ReviewRequired => HoldReasonView::ReviewRequired,
            Hold::ChangesRequested => HoldReasonView::ChangesRequested,
            Hold::WorkflowApproval => HoldReasonView::WorkflowApproval,
            Hold::ForkEditsOff { .. } => HoldReasonView::ForkEditsOff,
            Hold::NoQueue { .. } => HoldReasonView::NoQueue,
            Hold::NoSubscription => HoldReasonView::NoSubscription,
            Hold::FairUseLimit => HoldReasonView::FairUseLimit,
            Hold::Layer { .. } => unreachable!("innermost is no layer"),
        };
        HoldView {
            reason,
            layer: hold.layer().map(|n| n as u32),
            message: hold.to_string(),
        }
    }
}

impl RepoStatusView {
    fn new(installed: bool, state: Option<&RepoState>) -> Self {
        let on = installed && state.is_some_and(RepoState::on);
        let config_error = state.and_then(|s| s.config_error.clone());
        RepoStatusView {
            state: match (on, &config_error) {
                (false, _) => RepoStateView::Off,
                (true, Some(_)) => RepoStateView::Paused,
                (true, None) => RepoStateView::On,
            },
            installed,
            has_config: state.is_some_and(|s| s.has_config),
            forced: state.and_then(|s| s.enabled).map(|on| match on {
                true => SwitchView::On,
                false => SwitchView::Off,
            }),
            config_error,
        }
    }
}

/// What Mergepatrol knows of each of `numbers`, PRs of `repo`, which the App's `installation`
/// is on, if one is.
pub(crate) async fn statuses(
    app: &AppState,
    repo: &RepoName,
    installation: Option<u64>,
    numbers: &[PrNumber],
) -> Result<Vec<PullStateView>, ApiError> {
    let (state, lanes, exits) = match installation {
        Some(id) => (
            app.store.repo_state(id, repo).await?,
            app.store.repo_lanes(id, repo).await?,
            app.store.last_exits(id, repo, numbers).await?,
        ),
        None => (None, Vec::new(), Vec::new()),
    };
    let mergepatrol = RepoStatusView::new(installation.is_some(), state.as_ref());
    let lanes: Vec<_> = lanes
        .into_iter()
        .map(|(trunk, lane)| (trunk, lane.batch_pr, in_merge_order(lane.entries)))
        .collect();
    Ok(numbers
        .iter()
        .map(|&number| {
            let queue = lanes.iter().find_map(|(trunk, batch_pr, entries)| {
                let place = entries.iter().position(|e| e.prs().contains(&number))?;
                let e = &entries[place];
                let (state, hold) = match &e.state {
                    EntryState::Waiting => (QueueStateView::Waiting, None),
                    EntryState::Active => (QueueStateView::Running, None),
                    EntryState::Held(hold) => (QueueStateView::Held, Some(hold.into())),
                };
                Some(QueueView {
                    trunk: trunk.clone(),
                    state,
                    hold,
                    priority: e.priority,
                    position: place as u32 + 1,
                    stack: (!e.layers.is_empty()).then(|| StackView {
                        number: e.number as u32,
                        layers: e.layers.iter().map(|n| *n as u32).collect(),
                    }),
                    batch_pr: batch_pr
                        .filter(|_| e.state == EntryState::Active)
                        .map(|n| n as u32),
                })
            });
            // How it last left, unless it came back since.
            let exit = exits
                .iter()
                .find(|e| e.pr == number)
                .filter(|_| queue.is_none());
            PullStateView {
                repo: repo.to_string(),
                number: number as u32,
                url: permalink(app, repo, number),
                mergepatrol: mergepatrol.clone(),
                merged: exit.filter(|e| e.outcome == "merged").map(|e| MergedView {
                    at: e.at.clone(),
                    sha: e.detail.clone(),
                }),
                ejected: exit.filter(|e| e.outcome == "ejected").map(ejected),
                queue,
            }
        })
        .collect())
}

fn ejected(e: &ExitRecord) -> EjectedView {
    EjectedView {
        at: e.at.clone(),
        reason: e.detail.clone(),
    }
}

/// The dashboard's route for a PR, which finds its workspace, lane or history.
fn permalink(app: &AppState, repo: &RepoName, number: PrNumber) -> String {
    format!(
        "{}/{}/{}/pull/{number}",
        app.public_url, repo.owner, repo.name
    )
}

pub(crate) async fn pull(
    State(app): State<AppState>,
    caller: Caller,
    headers: HeaderMap,
    Path((owner, name, number)): Path<(String, String, u64)>,
) -> Response {
    read(
        &app,
        &caller,
        &headers,
        &owner,
        &name,
        Ok(vec![number]),
        |mut pulls| pulls.remove(0),
    )
    .await
}

#[derive(Deserialize)]
pub(crate) struct PullsQuery {
    /// The PRs' numbers, comma-separated.
    numbers: String,
}

pub(crate) async fn pulls(
    State(app): State<AppState>,
    caller: Caller,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Query(q): Query<PullsQuery>,
) -> Response {
    read(
        &app,
        &caller,
        &headers,
        &owner,
        &name,
        numbers(&q.numbers),
        |pulls| PullsView { pulls },
    )
    .await
}

fn numbers(list: &str) -> Result<Vec<PrNumber>, ApiError> {
    let bad = || ApiError::bad_request("`numbers` must be PR numbers, comma-separated.");
    let mut numbers = Vec::new();
    for n in list.split(',').map(str::trim).filter(|n| !n.is_empty()) {
        let n: PrNumber = n.parse().map_err(|_| bad())?;
        if !numbers.contains(&n) {
            numbers.push(n);
        }
    }
    if numbers.is_empty() {
        return Err(bad());
    }
    if numbers.len() > MAX_BATCH {
        return Err(ApiError::bad_request(&format!(
            "At most {MAX_BATCH} PRs at a time."
        )));
    }
    Ok(numbers)
}

/// Answers a read of `numbers` with what `body` makes of their statuses, once the caller may
/// read the repo and has calls left.
async fn read<T: Serialize>(
    app: &AppState,
    caller: &Caller,
    headers: &HeaderMap,
    owner: &str,
    name: &str,
    numbers: Result<Vec<PrNumber>, ApiError>,
    body: impl FnOnce(Vec<PullStateView>) -> T,
) -> Response {
    // `Caller` found the credential.
    let credential = session_token(headers).unwrap_or_default();
    let budget = app.budgets.spend(&hash(&credential), unix_now());
    let mut response = if budget.remaining.is_none() {
        let retry = budget.reset.saturating_sub(unix_now()).max(1);
        let message = "Rate limit used up. Try again after `retry-after` seconds.";
        let retry = [(header::RETRY_AFTER, retry.to_string())];
        (StatusCode::TOO_MANY_REQUESTS, retry, message).into_response()
    } else {
        match answer(app, caller, owner, name, numbers).await {
            Ok(pulls) => tagged(headers, &body(pulls)),
            Err(e) => e.into_response(),
        }
    };
    budget.write(response.headers_mut());
    response
}

async fn answer(
    app: &AppState,
    caller: &Caller,
    owner: &str,
    name: &str,
    numbers: Result<Vec<PrNumber>, ApiError>,
) -> Result<Vec<PullStateView>, ApiError> {
    let numbers = numbers?;
    let repo = RepoName::parse(&format!("{owner}/{name}"))
        .ok_or_else(|| ApiError::bad_request("The repo must be owner/name."))?;
    let installation = caller.installation_on(app, &repo).await?;
    statuses(app, &repo, installation, &numbers).await
}

/// The JSON, or a 304 when the caller has it already.
fn tagged<T: Serialize>(headers: &HeaderMap, body: &T) -> Response {
    let json = serde_json::to_string(body).expect("serializes");
    let etag = format!("\"{}\"", &hash(&json)[..32]);
    let fresh = headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|t| t.trim().trim_start_matches("W/"))
        .any(|t| t == etag || t == "*");
    let cache = [
        (header::ETAG, etag),
        (header::CACHE_CONTROL, "private, no-cache".to_owned()),
        (header::VARY, "authorization, cookie".to_owned()),
    ];
    if fresh {
        return (StatusCode::NOT_MODIFIED, cache).into_response();
    }
    let json_type = [(header::CONTENT_TYPE, "application/json".to_owned())];
    (cache, json_type, json).into_response()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Each caller's calls in its current window, by the hash of its credential. Each server keeps
/// its own count.
#[derive(Default)]
pub(crate) struct Budgets(Mutex<HashMap<String, (u64, u32)>>);

/// A caller's limit after one more call.
pub(crate) struct Budget {
    /// `None` when the call is over the limit.
    remaining: Option<u32>,
    used: u32,
    /// When the window ends, in Unix seconds.
    reset: u64,
}

impl Budgets {
    pub(crate) fn spend(&self, caller: &str, now: u64) -> Budget {
        let mut map = self.0.lock().unwrap();
        map.retain(|_, (start, _)| now < *start + WINDOW_SECS);
        let (start, used) = map.entry(caller.to_owned()).or_insert((now, 0));
        *used = used.saturating_add(1);
        Budget {
            remaining: LIMIT.checked_sub(*used),
            used: (*used).min(LIMIT),
            reset: *start + WINDOW_SECS,
        }
    }
}

impl Budget {
    fn write(&self, headers: &mut HeaderMap) {
        let values = [
            ("x-ratelimit-limit", LIMIT as u64),
            ("x-ratelimit-remaining", self.remaining.unwrap_or(0) as u64),
            ("x-ratelimit-used", self.used as u64),
            ("x-ratelimit-reset", self.reset),
        ];
        for (name, value) in values {
            headers.insert(name, HeaderValue::from(value));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_caller_runs_out_until_its_window_ends() {
        let budgets = Budgets::default();
        let first = budgets.spend("a", 100);
        assert_eq!(
            (first.remaining, first.used, first.reset),
            (Some(LIMIT - 1), 1, 3700)
        );
        for _ in 1..LIMIT {
            budgets.spend("a", 200);
        }
        let over = budgets.spend("a", 300);
        assert_eq!((over.remaining, over.used), (None, LIMIT));
        // Another caller has its own.
        assert_eq!(budgets.spend("b", 300).remaining, Some(LIMIT - 1));
        let next = budgets.spend("a", 3700);
        assert_eq!((next.remaining, next.reset), (Some(LIMIT - 1), 7300));
    }

    #[test]
    fn numbers_are_deduplicated_and_bounded() {
        assert_eq!(numbers("3, 1,3,").ok(), Some(vec![3, 1]));
        assert!(numbers("").is_err());
        assert!(numbers("1,x").is_err());
        let many: Vec<String> = (1..=101).map(|n| n.to_string()).collect();
        assert!(numbers(&many.join(",")).is_err());
    }

    #[test]
    fn a_layer_holds_its_stack_for_its_own_reason() {
        let hold = Hold::Layer {
            number: 4,
            hold: Box::new(Hold::ChangesRequested),
        };
        let view = HoldView::from(&hold);
        assert_eq!(view.reason, HoldReasonView::ChangesRequested);
        assert_eq!(view.layer, Some(4));
        assert_eq!(view.message, hold.to_string());
    }
}
