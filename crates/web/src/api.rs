use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use mergepatrol_core::{
    EFFORTS, Entry, EntryState, FixCiSettings, Person, REPO_CONFIG_PATH, RepoName, RepoSettings,
    ResolverSettings, Step,
};
use mergepatrol_github::{App, Installation, Proposal, PullRef};
use mergepatrol_resolver::{KeyCheck, check_anthropic_key};
use mergepatrol_store::{ANTHROPIC_API_KEY, ExitQuery, ExitRecord, ExitStats, RepoState, Scope};
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
    /// The draft PR that tests the active PRs together.
    pub batch: Option<u32>,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct EntryView {
    pub number: u32,
    pub priority: bool,
    pub state: EntryStateView,
    /// Why the PR is held.
    pub detail: Option<String>,
    pub title: String,
    pub url: String,
    /// `None` for a deleted account.
    pub author: Option<PersonView>,
    pub created_at: String,
    pub head_ref: String,
    /// From a fork.
    pub fork: bool,
    pub additions: u32,
    pub deletions: u32,
    pub changed_files: u32,
    pub review_decision: Option<ReviewDecisionView>,
    /// Each reviewer's latest approval or change request.
    pub reviews: Vec<ReviewView>,
    /// Users asked to review who have not yet.
    pub requested: Vec<PersonView>,
    /// The head commit's combined checks. `None` without checks.
    pub checks: Option<ChecksView>,
    /// A stack's PRs, bottom to top: `number` is the stack's. Empty for a PR.
    pub layers: Vec<u32>,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct PersonView {
    pub login: String,
    pub avatar_url: String,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct ReviewView {
    pub reviewer: PersonView,
    pub approved: bool,
}

#[derive(Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ReviewDecisionView {
    Approved,
    ReviewRequired,
    ChangesRequested,
}

#[derive(Serialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum ChecksView {
    Passed,
    Failed,
    Pending,
}

impl From<Person> for PersonView {
    fn from(p: Person) -> Self {
        PersonView {
            login: p.login,
            avatar_url: p.avatar_url,
        }
    }
}

impl EntryView {
    fn new(e: Entry) -> Self {
        let Entry {
            number,
            priority,
            state,
            summary: pr,
            layers,
            heads: _,
        } = e;
        let (state, detail) = match state {
            EntryState::Waiting => (EntryStateView::Waiting, None),
            EntryState::Held(reason) => (EntryStateView::Held, Some(reason)),
            EntryState::Active => (EntryStateView::Active, None),
        };
        EntryView {
            number: number as u32,
            priority,
            state,
            detail,
            title: pr.title,
            url: pr.url,
            author: pr.author.map(PersonView::from),
            created_at: pr.created_at,
            head_ref: pr.head_ref,
            fork: pr.fork,
            additions: pr.additions as u32,
            deletions: pr.deletions as u32,
            changed_files: pr.changed_files as u32,
            review_decision: match pr.review_decision.as_deref() {
                Some("APPROVED") => Some(ReviewDecisionView::Approved),
                Some("REVIEW_REQUIRED") => Some(ReviewDecisionView::ReviewRequired),
                Some("CHANGES_REQUESTED") => Some(ReviewDecisionView::ChangesRequested),
                _ => None,
            },
            reviews: pr
                .reviews
                .into_iter()
                .map(|r| ReviewView {
                    approved: r.state == "APPROVED",
                    reviewer: r.reviewer.into(),
                })
                .collect(),
            requested: pr.requested.into_iter().map(PersonView::from).collect(),
            checks: match pr.checks.as_deref() {
                Some("SUCCESS") => Some(ChecksView::Passed),
                Some("FAILURE" | "ERROR") => Some(ChecksView::Failed),
                Some(_) => Some(ChecksView::Pending),
                None => None,
            },
            layers: layers.into_iter().map(|n| n as u32).collect(),
        }
    }
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

/// One page of the PRs that left the queue, the newest first.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct ExitsPage {
    pub exits: Vec<ExitView>,
    /// `before` for the next page. `None` on the last.
    #[ts(type = "number | null")]
    pub next: Option<i64>,
}

/// The queue's metrics over a window, and over the window before it, for the trend.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct HistoryStatsView {
    /// Each repo's, then every repo's together, with no `repo`.
    pub current: Vec<StatsView>,
    pub previous: Vec<StatsView>,
    /// The PRs merged on each UTC day of the window, today last.
    pub merged_by_day: Vec<u32>,
    /// Where the merged PRs' time went, in the queue's order. Empty without a merge.
    pub steps: Vec<StepStatsView>,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct StepStatsView {
    pub step: StepView,
    pub mean_secs: f64,
    pub p50_secs: f64,
    pub p95_secs: f64,
}

#[derive(Clone, Copy, Serialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum StepView {
    Waiting,
    Held,
    Rebasing,
    Checks,
    Fixing,
}

impl From<Step> for StepView {
    fn from(s: Step) -> Self {
        match s {
            Step::Waiting => StepView::Waiting,
            Step::Held => StepView::Held,
            Step::Rebasing => StepView::Rebasing,
            Step::Checks => StepView::Checks,
            Step::Fixing => StepView::Fixing,
        }
    }
}

/// Seconds in one step.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct StepTimeView {
    pub step: StepView,
    pub secs: f64,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct StatsView {
    pub repo: Option<String>,
    pub merged: u32,
    pub ejected: u32,
    pub removed: u32,
    /// Seconds from joining the queue to merging. `None` without a merge.
    pub p50_secs: Option<f64>,
    pub p95_secs: Option<f64>,
}

/// A time a PR left the queue, and the PR as it was then.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct ExitView {
    #[ts(type = "number")]
    pub id: i64,
    pub repo: String,
    pub trunk: String,
    pub number: u32,
    pub outcome: OutcomeView,
    /// The merge commit, or why.
    pub detail: String,
    pub title: String,
    pub url: String,
    pub author: Option<PersonView>,
    /// From a fork.
    pub fork: bool,
    /// `None` for a PR ejected before it joined, such as a fork PR the queue cannot rebase.
    pub enqueued_at: Option<String>,
    /// Seconds in each step, in the queue's order. Empty with no `enqueued_at`.
    pub steps: Vec<StepTimeView>,
    pub at: String,
}

#[derive(Clone, Copy, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum OutcomeView {
    Merged,
    Ejected,
    /// The label came off, or the PR closed.
    Removed,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct RepoView {
    pub repo: String,
    pub private: bool,
    /// Whether its queue runs: `enabled`, or else whether it has the config file.
    pub on: bool,
    /// Set in the dashboard. `None` follows the config file.
    pub enabled: Option<bool>,
    /// The default branch has `.mergepatrol/mergepatrol.yml`, as of the last look.
    pub has_config: bool,
    /// Why the queue is paused: the config file does not parse.
    pub config_error: Option<String>,
}

#[derive(Deserialize, TS)]
#[ts(export)]
pub struct SetRepo {
    /// `None` follows the config file.
    pub enabled: Option<bool>,
}

/// A repo's settings, as its config file sets them.
#[derive(Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RepoSettingsView {
    pub label: String,
    pub batch_size: u32,
    pub model: String,
    pub effort: String,
    pub verify: Vec<String>,
    /// The agent fixes failed required checks.
    pub fix_ci: bool,
    /// The most fixes a PR gets, before it is ejected.
    pub fix_ci_attempts: u32,
}

impl From<RepoSettings> for RepoSettingsView {
    fn from(s: RepoSettings) -> Self {
        RepoSettingsView {
            label: s.label,
            batch_size: s.batch_size as u32,
            model: s.resolver.model,
            effort: s.resolver.effort,
            verify: s.verify,
            fix_ci: s.fix_ci.enabled,
            fix_ci_attempts: s.fix_ci.attempts,
        }
    }
}

impl From<RepoSettingsView> for RepoSettings {
    fn from(v: RepoSettingsView) -> Self {
        RepoSettings {
            label: v.label.trim().to_owned(),
            batch_size: v.batch_size as usize,
            resolver: ResolverSettings {
                model: v.model.trim().to_owned(),
                effort: v.effort,
            },
            verify: v
                .verify
                .iter()
                .map(|c| c.trim().to_owned())
                .filter(|c| !c.is_empty())
                .collect(),
            fix_ci: FixCiSettings {
                enabled: v.fix_ci,
                attempts: v.fix_ci_attempts,
            },
        }
    }
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct RepoConfigView {
    /// What to edit: the open PR's file, else the default branch's, over the defaults. The
    /// defaults when the file does not parse.
    pub settings: RepoSettingsView,
    /// What a repo without the file runs with.
    pub defaults: RepoSettingsView,
    pub efforts: Vec<String>,
    pub path: String,
    pub default_branch: String,
    pub has_file: bool,
    /// Why the file on the default branch does not parse.
    pub error: Option<String>,
    /// Mergepatrol's open PR that changes the file.
    pub pull_request: Option<PullRequestRef>,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct PullRequestRef {
    pub number: u32,
    pub url: String,
}

impl From<PullRef> for PullRequestRef {
    fn from(p: PullRef) -> Self {
        PullRequestRef {
            number: p.number as u32,
            url: p.url,
        }
    }
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
                batch: lane.batch_pr.map(|n| n as u32),
                entries: in_merge_order(lane.entries)
                    .into_iter()
                    .map(EntryView::new)
                    .collect(),
            })
            .collect(),
    ))
}

/// The order the queue takes them in, as `Lane::next` picks: the job, then waiting priority PRs,
/// then waiting PRs, each by place. Held PRs last.
fn in_merge_order(mut entries: Vec<Entry>) -> Vec<Entry> {
    entries.sort_by_key(|e| match e.state {
        EntryState::Active => 0,
        EntryState::Waiting if e.priority => 1,
        EntryState::Waiting => 2,
        EntryState::Held(_) => 3,
    });
    entries
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

const HISTORY_PAGE: u32 = 50;

#[derive(Deserialize)]
pub(crate) struct HistoryQuery {
    /// Every repo without one.
    repo: Option<String>,
    /// The window, in days back from now.
    days: u32,
    /// Every outcome without one.
    outcome: Option<OutcomeView>,
    /// The `next` of the page before.
    before: Option<i64>,
}

impl HistoryQuery {
    fn repo(&self) -> Result<Option<RepoName>, ApiError> {
        self.repo
            .as_deref()
            .map(|r| RepoName::parse(r).ok_or(ApiError::bad_request("repo must be owner/name")))
            .transpose()
    }

    fn days(&self) -> Result<u32, ApiError> {
        match self.days {
            1..=365 => Ok(self.days),
            _ => Err(ApiError::bad_request("days must be from 1 to 365")),
        }
    }
}

pub(crate) async fn history(
    State(app): State<AppState>,
    user: SignedIn,
    Path(id): Path<u64>,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<ExitsPage>, ApiError> {
    let id = user.workspace(id)?;
    let repo = q.repo()?;
    let outcomes: &[&str] = match q.outcome {
        None => &[],
        Some(OutcomeView::Merged) => &["merged"],
        Some(OutcomeView::Ejected) => &["ejected"],
        Some(OutcomeView::Removed) => &["dequeued", "cancelled"],
    };
    let exits = app
        .store
        .exits(&ExitQuery {
            installation_id: id,
            repo: repo.as_ref(),
            outcomes,
            days: q.days()?,
            before: q.before,
            limit: HISTORY_PAGE,
        })
        .await?;
    let next = (exits.len() == HISTORY_PAGE as usize)
        .then(|| exits.last().map(|e| e.id))
        .flatten();
    Ok(Json(ExitsPage {
        exits: exits.into_iter().map(ExitView::from).collect(),
        next,
    }))
}

pub(crate) async fn history_stats(
    State(app): State<AppState>,
    user: SignedIn,
    Path(id): Path<u64>,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<HistoryStatsView>, ApiError> {
    let id = user.workspace(id)?;
    let (repo, days) = (q.repo()?, q.days()?);
    let (current, previous, merged_by_day, steps) = tokio::try_join!(
        app.store.exit_stats(id, days, 0),
        app.store.exit_stats(id, 2 * days, days),
        app.store.merged_by_day(id, repo.as_ref(), days),
        app.store.step_stats(id, repo.as_ref(), days),
    )?;
    Ok(Json(HistoryStatsView {
        current: current.into_iter().map(StatsView::from).collect(),
        previous: previous.into_iter().map(StatsView::from).collect(),
        merged_by_day: merged_by_day.into_iter().map(|n| n as u32).collect(),
        steps: steps
            .into_iter()
            .map(|s| StepStatsView {
                step: s.step.into(),
                mean_secs: s.mean_secs,
                p50_secs: s.p50_secs,
                p95_secs: s.p95_secs,
            })
            .collect(),
    }))
}

impl From<ExitStats> for StatsView {
    fn from(s: ExitStats) -> Self {
        StatsView {
            repo: s.repo,
            merged: s.merged as u32,
            ejected: s.ejected as u32,
            removed: s.removed as u32,
            p50_secs: s.p50_secs,
            p95_secs: s.p95_secs,
        }
    }
}

impl From<ExitRecord> for ExitView {
    fn from(e: ExitRecord) -> Self {
        ExitView {
            id: e.id,
            repo: e.repo,
            trunk: e.trunk,
            number: e.pr as u32,
            outcome: match e.outcome.as_str() {
                "merged" => OutcomeView::Merged,
                "ejected" => OutcomeView::Ejected,
                _ => OutcomeView::Removed,
            },
            detail: e.detail,
            title: e.summary.title,
            url: e.summary.url,
            author: e.summary.author.map(Into::into),
            fork: e.summary.fork,
            enqueued_at: e.enqueued_at,
            steps: e
                .steps
                .unwrap_or_default()
                .into_iter()
                .map(|(step, secs)| StepTimeView {
                    step: step.into(),
                    secs,
                })
                .collect(),
            at: e.at,
        }
    }
}

/// The repos this workspace's installation can access, and whether Mergepatrol runs on each.
pub(crate) async fn repos(
    State(app): State<AppState>,
    user: SignedIn,
    Path(id): Path<u64>,
) -> Result<Json<Vec<RepoView>>, ApiError> {
    let id = user.workspace(id)?;
    let known: HashMap<RepoName, RepoState> = app
        .store
        .repos()
        .await?
        .into_iter()
        .filter(|s| s.installation_id == id)
        .map(|s| (s.repo.clone(), s))
        .collect();
    let mut views: Vec<RepoView> = installation(&app, id)
        .await?
        .repositories()
        .await?
        .into_iter()
        .map(|r| {
            let state = known.get(&r.repo);
            RepoView {
                on: state.is_some_and(RepoState::on),
                enabled: state.and_then(|s| s.enabled),
                has_config: state.is_some_and(|s| s.has_config),
                config_error: state.and_then(|s| s.config_error.clone()),
                repo: r.repo.to_string(),
                private: r.private,
            }
        })
        .collect();
    views.sort_by(|a, b| a.repo.cmp(&b.repo));
    Ok(Json(views))
}

async fn installation(app: &AppState, id: u64) -> Result<Installation, ApiError> {
    let creds = app
        .app_credentials()
        .await?
        .ok_or_else(|| ApiError::Status(StatusCode::CONFLICT, "No GitHub App yet.".into()))?;
    Ok(App::from_credentials(&creds)?.installation(id))
}

pub(crate) async fn set_repo(
    State(app): State<AppState>,
    user: SignedIn,
    Path((id, owner, name)): Path<(u64, String, String)>,
    Json(body): Json<SetRepo>,
) -> Result<StatusCode, ApiError> {
    let id = user.workspace(id)?;
    let repo = repo_name(owner, name)?;
    app.store.set_repo_enabled(id, &repo, body.enabled).await?;
    tracing::info!(installation = id, %repo, enabled = ?body.enabled, "repo setting changed");
    Ok(StatusCode::NO_CONTENT)
}

/// The repo of a URL's `{owner}/{name}`. Each is one path segment, but `%2F` decodes to a
/// slash: a repo stored with one would break every read of the repos.
fn repo_name(owner: String, name: String) -> Result<RepoName, ApiError> {
    RepoName::parse(&format!("{owner}/{name}")).ok_or_else(|| ApiError::bad_request("not a repo"))
}

/// Mergepatrol's branch for the PR that changes a repo's config file.
const CONFIG_BRANCH: &str = "mergepatrol/config";

/// The skill that tells a repo's agents how its queue works. The setup PR adds it.
const SKILL: &str = include_str!("skill.md");
/// Where agents look for repo skills: `.agents` for most, `.claude` for Claude Code.
const SKILL_DIRS: [&str; 2] = [".agents/skills", ".claude/skills"];

/// The repo's settings as its default branch has them, and the PR that would change them.
pub(crate) async fn repo_config(
    State(app): State<AppState>,
    user: SignedIn,
    Path((id, owner, name)): Path<(u64, String, String)>,
) -> Result<Json<RepoConfigView>, ApiError> {
    let id = user.workspace(id)?;
    let repo = repo_name(owner, name)?;
    let gh = installation(&app, id).await?;
    let file = gh.default_branch_file(&repo, REPO_CONFIG_PATH).await?;
    let defaults: &RepoSettings = &app.repo_defaults;
    let error = RepoSettings::from_file(file.text.as_deref(), defaults).err();
    let pull_request = gh.open_pr_from(&repo, CONFIG_BRANCH).await?;
    // Edits start from the open PR, so updating it keeps the changes already in it.
    let text = match pull_request {
        Some(_) => {
            gh.branch_file(&repo, CONFIG_BRANCH, REPO_CONFIG_PATH)
                .await?
        }
        None => file.text.clone(),
    };
    let settings =
        RepoSettings::from_file(text.as_deref(), defaults).unwrap_or_else(|_| defaults.clone());
    Ok(Json(RepoConfigView {
        settings: settings.into(),
        defaults: defaults.clone().into(),
        efforts: EFFORTS.map(str::to_owned).into(),
        path: REPO_CONFIG_PATH.into(),
        has_file: file.text.is_some(),
        default_branch: file.branch,
        error,
        pull_request: pull_request.map(PullRequestRef::from),
    }))
}

/// Opens a PR that writes these settings to the repo's config file, or updates the one open.
pub(crate) async fn propose_repo_config(
    State(app): State<AppState>,
    user: SignedIn,
    Path((id, owner, name)): Path<(u64, String, String)>,
    Json(body): Json<RepoSettingsView>,
) -> Result<Json<PullRequestRef>, ApiError> {
    let id = user.workspace(id)?;
    let repo = repo_name(owner, name)?;
    let settings = RepoSettings::from(body);
    settings
        .check()
        .map_err(|e| ApiError::Status(StatusCode::UNPROCESSABLE_ENTITY, e))?;
    let content = settings.to_yaml();
    let gh = installation(&app, id).await?;
    let base = gh.default_branch_file(&repo, REPO_CONFIG_PATH).await?;
    if base.text.as_deref() == Some(content.as_str()) {
        return Err(ApiError::Status(
            StatusCode::CONFLICT,
            format!("`{}` already has these settings.", base.branch),
        ));
    }
    let skills = match base.text {
        Some(_) => Vec::new(),
        None => skill_paths(&gh, &repo).await?,
    };
    let (title, body) = config_pr(&settings, base.text.is_some(), &base.branch, &skills);
    let mut files = vec![(REPO_CONFIG_PATH, content.as_str())];
    files.extend(skills.iter().map(|p| (p.as_str(), SKILL)));
    let pr = gh
        .propose_file(
            &repo,
            Proposal {
                base: &base,
                files: &files,
                branch: CONFIG_BRANCH,
                title,
                body: &body,
            },
        )
        .await?;
    tracing::info!(installation = id, %repo, pr = pr.number, "proposed repo settings");
    Ok(Json(pr.into()))
}

/// The skill's path in each skills dir, but one that links to the other: a commit cannot write
/// through a symlink, and the link already shows the skill there.
async fn skill_paths(gh: &Installation, repo: &RepoName) -> Result<Vec<String>, ApiError> {
    let mut paths = Vec::new();
    for dir in SKILL_DIRS {
        // A symlink is a blob, so it has text. A dir has none.
        if gh.default_branch_file(repo, dir).await?.text.is_none() {
            paths.push(format!("{dir}/mergepatrol/SKILL.md"));
        }
    }
    Ok(paths)
}

fn config_pr(
    settings: &RepoSettings,
    update: bool,
    branch: &str,
    skills: &[String],
) -> (&'static str, String) {
    let verify = if settings.verify.is_empty() {
        "none".to_owned()
    } else {
        settings
            .verify
            .iter()
            .map(|c| format!("`{c}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let fix_ci = match settings.fix_ci.allowed() {
        0 => "off".to_owned(),
        1 => "on, one attempt".to_owned(),
        n => format!("on, up to {n} attempts"),
    };
    let list = format!(
        "- Trigger label: `{}`\n- Batch size: {}\n- Resolver: `{}`, effort `{}`\n- Verify commands: {verify}\n- Fix failed checks: {fix_ci}",
        settings.label, settings.batch_size, settings.resolver.model, settings.resolver.effort
    );
    let skills = if skills.is_empty() {
        String::new()
    } else {
        let paths: Vec<String> = skills.iter().map(|p| format!("`{p}`")).collect();
        format!(
            "\n\nIt also adds the `mergepatrol` agent skill, at {}, so coding agents in this repo queue their PRs instead of merging them.",
            paths.join(" and ")
        )
    };
    if update {
        (
            "Update the Mergepatrol settings",
            format!(
                "This PR changes `{REPO_CONFIG_PATH}`, as edited in the Mergepatrol dashboard. The queue uses the new settings once this PR merges into `{branch}`.\n\n{list}"
            ),
        )
    } else {
        (
            "Set up Mergepatrol",
            format!(
                "This PR adds `{REPO_CONFIG_PATH}` with the settings below. Once it merges into `{branch}`, Mergepatrol runs a merge queue on this repo: add the `{}` label to a PR to queue it.\n\n{list}{skills}\n\nTo change the settings later, edit the file, or use the Mergepatrol dashboard, which opens a PR like this one.",
                settings.label
            ),
        )
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use mergepatrol_core::PrSummary;

    fn entry(number: u64, priority: bool, state: EntryState) -> Entry {
        Entry {
            number,
            priority,
            state,
            summary: PrSummary::default(),
            layers: Vec::new(),
            heads: Vec::new(),
        }
    }

    #[test]
    fn lanes_list_prs_in_the_order_they_merge() {
        let entries = vec![
            entry(1, false, EntryState::Held("draft".into())),
            entry(2, false, EntryState::Waiting),
            entry(3, true, EntryState::Waiting),
            entry(4, false, EntryState::Active),
            entry(5, false, EntryState::Waiting),
        ];
        let order: Vec<u64> = in_merge_order(entries).iter().map(|e| e.number).collect();
        assert_eq!(order, [4, 3, 2, 5, 1]);
    }
}
