use std::collections::{HashMap, HashSet};

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use mergepatrol_core::{
    EFFORTS, Entry, EntryState, FixCiSettings, Labels, Lane, Person, PrSummary, Provider,
    REPO_CONFIG_PATH, RepoName, RepoSettings, ResolverSettings, Spend, SpendTask, Step,
};
use mergepatrol_github::{
    App, AsUser, BranchFile, CheckRunState, InboxPull, Installation, Proposal, PullDetail, PullRef,
    TimelineEvent, setup,
};
use mergepatrol_resolver::{DeviceLogin, KeyCheck, check_api_key};
use mergepatrol_store::{
    CHATGPT_DEVICE_LOGIN, CHATGPT_LOGIN, Change, DeveloperStats, ExitQuery, ExitRecord, ExitStats, LaneKey,
    RepoState, Scope, Slice, Store, api_key_secret,
};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::auth::SignedIn;
use crate::{ApiError, AppState, Wake};

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
    #[serde(flatten)]
    pub pull: PullView,
    /// A stack's PRs, bottom to top: `number` is the stack's. Empty for a PR.
    pub layers: Vec<u32>,
}

/// An open PR that is not in the queue.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct OpenPullView {
    pub number: u32,
    pub draft: bool,
    #[serde(flatten)]
    pub pull: PullView,
}

/// What the dashboard shows of a PR, queued or not.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct PullView {
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
            pull: pr.into(),
            layers: layers.into_iter().map(|n| n as u32).collect(),
        }
    }
}

impl From<PrSummary> for PullView {
    fn from(pr: PrSummary) -> Self {
        PullView {
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
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum EntryStateView {
    Waiting,
    Held,
    Active,
}

/// The signed-in user's PRs, and the PRs that ask for their review, across their workspaces.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct YouView {
    /// Theirs, updated last first.
    pub authored: Vec<InboxPullView>,
    /// Asked of them, or of a team of theirs, updated last first.
    pub review_requested: Vec<InboxPullView>,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct InboxPullView {
    pub repo: String,
    pub number: u32,
    /// The branch it merges into.
    pub base: String,
    pub draft: bool,
    pub updated_at: String,
    /// The workspace the repo is in, to act on the PR in. `None` outside the user's.
    pub workspace: Option<u32>,
    /// Whether Mergepatrol runs on the repo, so the PR can be queued.
    pub queues: bool,
    /// Where the queue has it. `None` when it is not queued.
    pub queue: Option<QueueSpotView>,
    #[serde(flatten)]
    pub pull: PullView,
}

/// A PR's place in its lane, in merge order.
#[derive(Clone, Debug, Serialize, TS)]
#[ts(export)]
pub struct QueueSpotView {
    pub state: EntryStateView,
    /// From 1. A held PR has one too: last, after those waiting.
    pub place: u32,
    /// The PRs in the lane.
    pub of: u32,
    pub priority: bool,
    /// Why it is held.
    pub detail: Option<String>,
}

#[derive(Deserialize, TS)]
#[ts(export)]
pub struct ReviewPull {
    pub verdict: VerdictView,
    pub body: String,
    /// Then queue it, as the Queue button would.
    pub queue: bool,
}

#[derive(Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum VerdictView {
    Approve,
    RequestChanges,
    Comment,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct EventView {
    pub repo: String,
    pub pr: u32,
    pub at: String,
    pub kind: String,
    pub message: String,
    /// What the agent spent, for an event of its run: `resolved`, `unresolved`, `fix` or
    /// `unfixed`.
    pub spend: Option<SpendView>,
}

/// One agent run, from its event.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct SpendView {
    pub task: SpendTaskView,
    /// Whether the run did its task. One that did not still spent.
    pub done: bool,
    /// The conflicted files, comma-separated. Empty for a fix.
    pub files: String,
    pub turns: u32,
    pub cost_usd: Option<f64>,
    /// `None` for runs from before events named them.
    pub model: Option<String>,
    pub effort: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum SpendTaskView {
    Rebase,
    Fix,
}

impl From<SpendTask> for SpendTaskView {
    fn from(t: SpendTask) -> Self {
        match t {
            SpendTask::Rebase => SpendTaskView::Rebase,
            SpendTask::Fix => SpendTaskView::Fix,
        }
    }
}

impl From<Spend> for SpendView {
    fn from(s: Spend) -> Self {
        SpendView {
            task: s.task.into(),
            done: s.done,
            files: s.files,
            turns: s.turns,
            cost_usd: s.cost_usd,
            model: s.model,
            effort: s.effort,
        }
    }
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
    /// Each repo's, then every repo's together, with no `repo`. Of the author's PRs with one,
    /// whatever the repo: it picks a row.
    pub current: Vec<StatsView>,
    pub previous: Vec<StatsView>,
    /// Each author's, by login, of the repo's PRs with one, whatever the author. Not the PRs
    /// of deleted accounts.
    pub developers: Vec<DeveloperStatsView>,
    /// The PRs merged on each UTC day of the window, today last.
    pub merged_by_day: Vec<u32>,
    /// Where the merged PRs' time went, in the queue's order. Empty without a merge.
    pub steps: Vec<StepStatsView>,
    /// The agent runs of the window on the repo's and author's PRs, by model and effort: the
    /// costliest first.
    pub llms: Vec<LlmStatsView>,
}

/// What one model, at one effort, spent over a window.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct LlmStatsView {
    /// `None` for runs from before events named them.
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Resolving conflicts in rebases.
    pub rebase: LlmRunsView,
    /// Fixing failed checks.
    pub fix: LlmRunsView,
}

#[derive(Default, Serialize, TS)]
#[ts(export)]
pub struct LlmRunsView {
    /// Every run, whether it did its task or not.
    pub runs: u32,
    /// The runs that did not: the agent gave up, broke, or left a fix that fails.
    pub failed: u32,
    pub turns: u32,
    /// Of the runs whose CLI told their cost.
    pub cost_usd: f64,
    /// The runs whose CLI did not, such as Codex's: `cost_usd` leaves them out.
    pub unpriced: u32,
}

impl LlmRunsView {
    fn add(&mut self, s: &Spend) {
        self.runs += 1;
        if !s.done {
            self.failed += 1;
        }
        self.turns += s.turns;
        match s.cost_usd {
            Some(c) => self.cost_usd += c,
            None => self.unpriced += 1,
        }
    }
}

/// The runs by model and effort, the costliest first.
fn llm_stats(spends: Vec<Spend>) -> Vec<LlmStatsView> {
    let mut by: Vec<LlmStatsView> = Vec::new();
    for s in spends {
        let i = match by
            .iter()
            .position(|l| l.model == s.model && l.effort == s.effort)
        {
            Some(i) => i,
            None => {
                by.push(LlmStatsView {
                    model: s.model.clone(),
                    effort: s.effort.clone(),
                    rebase: LlmRunsView::default(),
                    fix: LlmRunsView::default(),
                });
                by.len() - 1
            }
        };
        match s.task {
            SpendTask::Rebase => by[i].rebase.add(&s),
            SpendTask::Fix => by[i].fix.add(&s),
        }
    }
    let cost = |l: &LlmStatsView| l.rebase.cost_usd + l.fix.cost_usd;
    by.sort_by(|a, b| cost(b).total_cmp(&cost(a)));
    by
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

/// One author's PRs over the window.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct DeveloperStatsView {
    pub author: PersonView,
    /// A GitHub App, such as Dependabot, rather than a user.
    pub bot: bool,
    pub merged: u32,
    pub ejected: u32,
    pub removed: u32,
    /// Seconds from joining the queue to merging. `None` without a merge.
    pub p50_secs: Option<f64>,
    pub p95_secs: Option<f64>,
}

impl From<DeveloperStats> for DeveloperStatsView {
    fn from(s: DeveloperStats) -> Self {
        DeveloperStatsView {
            bot: s.author.bot,
            author: s.author.into(),
            merged: s.merged as u32,
            ejected: s.ejected as u32,
            removed: s.removed as u32,
            p50_secs: s.p50_secs,
            p95_secs: s.p95_secs,
        }
    }
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
    #[ts(type = "\"anthropic\" | \"openai\" | \"openrouter\"")]
    pub provider: Provider,
    pub model: String,
    pub effort: String,
    pub verify: Vec<String>,
    /// The agent fixes failed required checks.
    pub fix_ci: bool,
    /// The most fixes a PR gets, before it is ejected.
    pub fix_ci_attempts: u32,
    /// The branches with a queue besides the default branch.
    pub branches: Vec<String>,
}

impl From<RepoSettings> for RepoSettingsView {
    fn from(s: RepoSettings) -> Self {
        RepoSettingsView {
            label: s.label,
            batch_size: s.batch_size as u32,
            provider: s.resolver.provider,
            model: s.resolver.model,
            effort: s.resolver.effort,
            verify: s.verify,
            fix_ci: s.fix_ci.enabled,
            fix_ci_attempts: s.fix_ci.attempts,
            branches: s.branches,
        }
    }
}

impl From<RepoSettingsView> for RepoSettings {
    fn from(v: RepoSettingsView) -> Self {
        RepoSettings {
            label: v.label.trim().to_owned(),
            batch_size: v.batch_size as usize,
            resolver: ResolverSettings {
                provider: v.provider,
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
            branches: v
                .branches
                .iter()
                .map(|b| b.trim().to_owned())
                .filter(|b| !b.is_empty())
                .collect(),
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
    /// The providers a repo can choose.
    pub providers: Vec<ProviderView>,
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
pub struct ProviderView {
    /// As in the config file, and in the key's URL.
    pub id: String,
    pub name: String,
}

impl From<Provider> for ProviderView {
    fn from(p: Provider) -> Self {
        ProviderView {
            id: p.as_str().into(),
            name: p.name().into(),
        }
    }
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct SettingsView {
    /// One per provider a repo can choose.
    pub api_keys: Vec<ApiKeyView>,
    pub chatgpt: ChatGptView,
}

/// The workspace's ChatGPT plan, which Codex uses instead of the OpenAI API key.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct ChatGptView {
    /// The login, with the account's email as the hint.
    pub login: Option<SecretView>,
    /// A sign-in the user has yet to finish.
    pub sign_in: Option<ChatGptSignInView>,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct ChatGptSignInView {
    /// The user enters `user_code` at `verification_url`.
    pub user_code: String,
    pub verification_url: String,
    /// How often to ask whether they have.
    pub interval_secs: u32,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct ApiKeyView {
    pub provider: ProviderView,
    pub key: Option<SecretView>,
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
    #[ts(type = "string")]
    pub value: SecretString,
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
pub(crate) fn in_merge_order(mut entries: Vec<Entry>) -> Vec<Entry> {
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
                spend: Spend::parse(&e.kind, &e.message).map(SpendView::from),
                repo: e.repo,
                pr: e.pr as u32,
                at: e.at,
                kind: e.kind,
                message: e.message,
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
pub(crate) struct FixQuery {
    repo: String,
    sha: String,
}

/// A fix the agent pushed, as `git diff` prints it.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct FixView {
    pub patch: String,
}

/// The changes of the fix agent's commit `sha`. Not of any other commit.
pub(crate) async fn fix(
    State(app): State<AppState>,
    user: SignedIn,
    Path(id): Path<u64>,
    Query(q): Query<FixQuery>,
) -> Result<Json<FixView>, ApiError> {
    let id = user.workspace(id)?;
    let repo = RepoName::parse(&q.repo).ok_or(ApiError::bad_request("repo must be owner/name"))?;
    if q.sha.len() != 40 || !q.sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ApiError::bad_request("sha must be a commit SHA"));
    }
    let patch = installation(&app, id)
        .await?
        .fix_patch(&repo, &q.sha)
        .await?
        .ok_or_else(|| ApiError::Status(StatusCode::NOT_FOUND, "no such fix".into()))?;
    Ok(Json(FixView { patch }))
}

const HISTORY_PAGE: u32 = 50;

#[derive(Deserialize)]
pub(crate) struct HistoryQuery {
    /// Every repo without one.
    repo: Option<String>,
    /// A GitHub login. Every author without one.
    author: Option<String>,
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

    fn slice<'a>(&'a self, repo: Option<&'a RepoName>) -> Slice<'a> {
        Slice {
            repo,
            author: self.author.as_deref(),
        }
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
            slice: q.slice(repo.as_ref()),
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
    let slice = q.slice(repo.as_ref());
    // Each repo's of the author's, and each author's of the repo's, so either can break down
    // the other.
    let by_repo = Slice {
        repo: None,
        ..slice
    };
    let by_author = Slice {
        author: None,
        ..slice
    };
    let (current, previous, developers, merged_by_day, steps, spends) = tokio::try_join!(
        app.store.exit_stats(id, by_repo, days, 0),
        app.store.exit_stats(id, by_repo, 2 * days, days),
        app.store.developer_stats(id, by_author, days),
        app.store.merged_by_day(id, slice, days),
        app.store.step_stats(id, slice, days),
        app.store.spends(id, slice, days),
    )?;
    Ok(Json(HistoryStatsView {
        current: current.into_iter().map(StatsView::from).collect(),
        previous: previous.into_iter().map(StatsView::from).collect(),
        developers: developers.into_iter().map(Into::into).collect(),
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
        llms: llm_stats(spends),
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
    Ok(github_app(app).await?.installation(id))
}

pub(crate) async fn github_app(app: &AppState) -> Result<App, ApiError> {
    let creds = app
        .app_credentials()
        .await?
        .ok_or_else(|| ApiError::Status(StatusCode::CONFLICT, "No GitHub App yet.".into()))?;
    Ok(App::from_credentials(&creds)?)
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

/// How many open PRs the queue page lists below the queue.
const OPEN_PULLS: u32 = 50;

/// The repo's open PRs into its default branch that are not in its queue, updated last first.
/// Read as the user: each viewer's polling spends their own rate limit, not the queue's. A
/// revoked token means signing in again.
pub(crate) async fn open_pulls(
    State(app): State<AppState>,
    user: SignedIn,
    Path((id, owner, name)): Path<(u64, String, String)>,
) -> Result<Json<Vec<OpenPullView>>, ApiError> {
    let id = user.workspace(id)?;
    let repo = repo_name(owner, name)?;
    let gh = AsUser::new(user.github_token(&app)?)?;
    let file = as_user(gh.default_branch_file(&repo, REPO_CONFIG_PATH).await, &repo)?;
    let (labels, trunk) = labels_of(&app, file)?;
    // A stack's layers are in the queue with it, though one layer alone may have the label.
    let queued: HashSet<u64> = app
        .store
        .lanes(id)
        .await?
        .into_iter()
        .filter(|(key, _)| key.repo == repo)
        .flat_map(|(_, lane)| lane.entries)
        .flat_map(|e| std::iter::once(e.number).chain(e.layers))
        .collect();
    let pulls = as_user(
        gh.open_pull_requests(&repo, &trunk, OPEN_PULLS).await,
        &repo,
    )?;
    Ok(Json(
        pulls
            .into_iter()
            .filter(|pr| !pr.has_label(&labels.trigger) && !queued.contains(&pr.number))
            .map(|pr| OpenPullView {
                number: pr.number as u32,
                draft: pr.is_draft,
                pull: pr.summary().into(),
            })
            .collect(),
    ))
}

/// What the detail pane shows of an open PR beyond its [`OpenPullView`].
#[derive(Serialize, TS)]
#[ts(export)]
pub struct PullDetailView {
    /// The description as GitHub renders it, sanitized by GitHub.
    pub body_html: String,
    pub labels: Vec<LabelView>,
    /// The head commit's check runs and commit statuses.
    pub checks: Vec<CheckRunView>,
    /// Oldest first, up to the last 100 events.
    pub timeline: Vec<TimelineItemView>,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct LabelView {
    pub name: String,
    /// Hex, without `#`.
    pub color: String,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct CheckRunView {
    /// `workflow / job` for an Actions job.
    pub name: String,
    pub state: CheckRunStateView,
    pub url: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(Serialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum CheckRunStateView {
    Passed,
    Failed,
    Pending,
    Skipped,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct TimelineItemView {
    pub at: String,
    /// `None` for a deleted account.
    pub actor: Option<PersonView>,
    pub event: TimelineEventView,
}

#[derive(Serialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum TimelineEventView {
    Commit {
        sha: String,
        headline: String,
    },
    Review {
        approval: ReviewStateView,
        body_html: String,
    },
    Comment {
        body_html: String,
    },
    /// A user's login, or a team's name.
    ReviewRequested {
        reviewer: String,
    },
    Labeled {
        label: LabelView,
        added: bool,
    },
    ForcePushed {
        before: String,
        after: String,
    },
    ReadyForReview,
    ConvertedToDraft,
}

#[derive(Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ReviewStateView {
    Approved,
    ChangesRequested,
    Commented,
    Dismissed,
}

impl From<mergepatrol_github::Label> for LabelView {
    fn from(l: mergepatrol_github::Label) -> Self {
        LabelView {
            name: l.name,
            color: l.color,
        }
    }
}

impl From<PullDetail> for PullDetailView {
    fn from(d: PullDetail) -> Self {
        PullDetailView {
            body_html: d.body_html,
            labels: d.labels.into_iter().map(LabelView::from).collect(),
            checks: d
                .checks
                .into_iter()
                .map(|c| CheckRunView {
                    name: c.name,
                    state: match c.state {
                        CheckRunState::Passed => CheckRunStateView::Passed,
                        CheckRunState::Failed => CheckRunStateView::Failed,
                        CheckRunState::Pending => CheckRunStateView::Pending,
                        CheckRunState::Skipped => CheckRunStateView::Skipped,
                    },
                    url: c.url,
                    started_at: c.started_at,
                    completed_at: c.completed_at,
                })
                .collect(),
            timeline: d
                .timeline
                .into_iter()
                .map(|t| TimelineItemView {
                    at: t.at,
                    actor: t.actor.map(PersonView::from),
                    event: match t.event {
                        TimelineEvent::Commit { sha, headline } => {
                            TimelineEventView::Commit { sha, headline }
                        }
                        TimelineEvent::Review { state, body_html } => TimelineEventView::Review {
                            approval: match state.as_str() {
                                "APPROVED" => ReviewStateView::Approved,
                                "CHANGES_REQUESTED" => ReviewStateView::ChangesRequested,
                                "DISMISSED" => ReviewStateView::Dismissed,
                                _ => ReviewStateView::Commented,
                            },
                            body_html,
                        },
                        TimelineEvent::Comment { body_html } => {
                            TimelineEventView::Comment { body_html }
                        }
                        TimelineEvent::ReviewRequested { reviewer } => {
                            TimelineEventView::ReviewRequested { reviewer }
                        }
                        TimelineEvent::Labeled { label, added } => TimelineEventView::Labeled {
                            label: label.into(),
                            added,
                        },
                        TimelineEvent::ForcePushed { before, after } => {
                            TimelineEventView::ForcePushed { before, after }
                        }
                        TimelineEvent::ReadyForReview => TimelineEventView::ReadyForReview,
                        TimelineEvent::ConvertedToDraft => TimelineEventView::ConvertedToDraft,
                    },
                })
                .collect(),
        }
    }
}

/// An open PR's description, labels, checks and timeline, read as the user like the open PRs.
pub(crate) async fn pull_detail(
    State(app): State<AppState>,
    user: SignedIn,
    Path((id, owner, name, number)): Path<(u64, String, String, u64)>,
) -> Result<Json<PullDetailView>, ApiError> {
    user.workspace(id)?;
    let repo = repo_name(owner, name)?;
    let gh = AsUser::new(user.github_token(&app)?)?;
    let detail = as_user(gh.pull_detail(&repo, number).await, &repo)?;
    Ok(Json(detail.into()))
}

#[derive(Deserialize)]
pub(crate) struct PullDiffQuery {
    /// One of the PR's commits. The whole PR without one.
    commit: Option<String>,
}

/// A PR's changes, or one commit's, as `git diff` prints them.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct PullDiffView {
    pub patch: String,
}

/// An open PR's diff, or one of its commits', read as the user.
pub(crate) async fn pull_diff(
    State(app): State<AppState>,
    user: SignedIn,
    Path((id, owner, name, number)): Path<(u64, String, String, u64)>,
    Query(q): Query<PullDiffQuery>,
) -> Result<Json<PullDiffView>, ApiError> {
    user.workspace(id)?;
    let repo = repo_name(owner, name)?;
    if let Some(sha) = &q.commit
        && (sha.len() != 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(ApiError::bad_request("commit must be a commit SHA"));
    }
    let gh = AsUser::new(user.github_token(&app)?)?;
    let patch = as_user(
        gh.pull_diff(&repo, number, q.commit.as_deref()).await,
        &repo,
    )?
    .ok_or_else(|| {
        ApiError::Status(
            StatusCode::UNPROCESSABLE_ENTITY,
            "This diff is too large to show here.".into(),
        )
    })?;
    Ok(Json(PullDiffView { patch }))
}

#[derive(Deserialize, TS)]
#[ts(export)]
pub struct QueuePull {
    /// To the front of the queue.
    pub priority: bool,
}

/// Queues the PR, or changes the priority of a queued one. The labels do it, put on as the
/// user, so GitHub checks that they may.
pub(crate) async fn queue_pull(
    State(app): State<AppState>,
    user: SignedIn,
    Path((id, owner, name, number)): Path<(u64, String, String, u64)>,
    Json(body): Json<QueuePull>,
) -> Result<StatusCode, ApiError> {
    let id = user.workspace(id)?;
    let repo = repo_name(owner, name)?;
    let token = user.github_token(&app)?;
    let gh = installation(&app, id).await?;
    queue(&app, &gh, &token, &repo, number, body.priority).await?;
    tracing::info!(installation = id, %repo, number, priority = body.priority, "queued from the dashboard");
    Ok(StatusCode::NO_CONTENT)
}

/// Takes the PR out of the queue, and its stack if it is in one, as the user.
pub(crate) async fn dequeue_pull(
    State(app): State<AppState>,
    user: SignedIn,
    Path((id, owner, name, number)): Path<(u64, String, String, u64)>,
) -> Result<StatusCode, ApiError> {
    let id = user.workspace(id)?;
    let repo = repo_name(owner, name)?;
    let token = user.github_token(&app)?;
    let gh = installation(&app, id).await?;
    dequeue(&app, &gh, &token, &repo, number).await?;
    tracing::info!(installation = id, %repo, number, "dequeued from the dashboard");
    Ok(StatusCode::NO_CONTENT)
}

/// Puts the queue labels on the PR with `token`, and wakes the repo's queue.
pub(crate) async fn queue(
    app: &AppState,
    gh: &Installation,
    token: &str,
    repo: &RepoName,
    number: u64,
    priority: bool,
) -> Result<(), ApiError> {
    let (labels, _) = repo_labels(app, gh, repo).await?;
    let mut add = vec![labels.trigger.as_str()];
    let mut changed = vec![number];
    if priority {
        add.push(&labels.priority);
    } else {
        // A stack has priority while any of its layers has the label.
        for n in layers(gh, repo, number).await? {
            as_user(
                setup::remove_label_as(token, &repo.to_string(), n, &labels.priority).await,
                repo,
            )?;
            changed.push(n);
        }
    }
    as_user(
        setup::add_labels_as(token, &repo.to_string(), number, &add).await,
        repo,
    )?;
    relabeled(&*app.store, &*app.wake, gh.id, repo.clone(), &changed).await;
    Ok(())
}

/// Takes the queue labels off the PR and its stack with `token`, and wakes the repo's queue.
pub(crate) async fn dequeue(
    app: &AppState,
    gh: &Installation,
    token: &str,
    repo: &RepoName,
    number: u64,
) -> Result<(), ApiError> {
    let (labels, _) = repo_labels(app, gh, repo).await?;
    let layers = layers(gh, repo, number).await?;
    for &n in &layers {
        for label in [&labels.trigger, &labels.priority] {
            as_user(
                setup::remove_label_as(token, &repo.to_string(), n, label).await,
                repo,
            )?;
        }
    }
    relabeled(&*app.store, &*app.wake, gh.id, repo.clone(), &layers).await;
    Ok(())
}

/// These PRs' labels changed from the dashboard or MCP. The queue wakes to read them again now,
/// before the label webhooks come to mark its reads of them stale.
pub(crate) async fn relabeled(
    store: &dyn Store,
    wake: &dyn Wake,
    id: u64,
    repo: RepoName,
    prs: &[u64],
) {
    for &n in prs {
        if let Err(e) = store.github_changed(id, &repo, &Change::Pr(n)).await {
            tracing::warn!(installation = id, %repo, pr = n, error = %format!("{e:#}"), "marking a relabeled PR");
        }
    }
    wake.wake(id, repo);
}

/// Closes an open PR, as the user. The queue lists only the open PRs it does not have, so
/// none of its own.
pub(crate) async fn close_pull(
    State(app): State<AppState>,
    user: SignedIn,
    Path((id, owner, name, number)): Path<(u64, String, String, u64)>,
) -> Result<StatusCode, ApiError> {
    let id = user.workspace(id)?;
    let repo = repo_name(owner, name)?;
    let token = user.github_token(&app)?;
    as_user(
        setup::close_pull_as(&token, &repo.to_string(), number).await,
        &repo,
    )?;
    tracing::info!(installation = id, %repo, number, "closed from the dashboard");
    Ok(StatusCode::NO_CONTENT)
}

/// The user's inbox, read as them, with where the queue has each PR.
pub(crate) async fn you(
    State(app): State<AppState>,
    user: SignedIn,
) -> Result<Json<YouView>, ApiError> {
    let gh = AsUser::new(user.github_token(&app)?)?;
    let inbox = gh.inbox().await.map_err(|e| {
        if e.is::<setup::Revoked>() {
            ApiError::Status(StatusCode::UNAUTHORIZED, "Sign in again first.".into())
        } else {
            e.into()
        }
    })?;
    let ids = user.installations();
    let workspaces = app.store.workspaces(ids).await?;
    let on: HashSet<RepoName> = app
        .store
        .repos()
        .await?
        .into_iter()
        .filter(|r| ids.contains(&r.installation_id) && r.on())
        .map(|r| r.repo)
        .collect();
    let mut spots = HashMap::new();
    for &id in ids {
        spots.extend(queue_spots(app.store.lanes(id).await?));
    }
    let mut view = |p: InboxPull| InboxPullView {
        number: p.pr.number as u32,
        base: p.pr.merges_into().to_owned(),
        draft: p.pr.is_draft,
        updated_at: p.updated_at,
        workspace: workspaces
            .iter()
            .find(|w| w.account.eq_ignore_ascii_case(&p.repo.owner))
            .map(|w| w.installation_id as u32),
        queues: on.contains(&p.repo),
        queue: spots.remove(&(p.repo.clone(), p.pr.number)),
        pull: p.pr.summary().into(),
        repo: p.repo.to_string(),
    };
    let authored = inbox.authored.into_iter().map(&mut view).collect();
    let review_requested = inbox.review_requested.into_iter().map(&mut view).collect();
    Ok(Json(YouView {
        authored,
        review_requested,
    }))
}

/// Where the queue has each PR of these lanes. A stack's layers each take the stack's spot.
fn queue_spots(lanes: Vec<(LaneKey, Lane)>) -> HashMap<(RepoName, u64), QueueSpotView> {
    let mut spots = HashMap::new();
    for (key, lane) in lanes {
        let entries = in_merge_order(lane.entries);
        let of = entries.len() as u32;
        for (i, e) in entries.into_iter().enumerate() {
            let (state, detail) = match e.state {
                EntryState::Waiting => (EntryStateView::Waiting, None),
                EntryState::Held(reason) => (EntryStateView::Held, Some(reason)),
                EntryState::Active => (EntryStateView::Active, None),
            };
            let spot = QueueSpotView {
                state,
                place: i as u32 + 1,
                of,
                priority: e.priority,
                detail,
            };
            for n in std::iter::once(e.number).chain(e.layers) {
                spots.insert((key.repo.clone(), n), spot.clone());
            }
        }
    }
    spots
}

/// Reviews the PR as the user, then queues it if they asked. The queue reads the PR again now:
/// a review may be what held it.
pub(crate) async fn review_pull(
    State(app): State<AppState>,
    user: SignedIn,
    Path((id, owner, name, number)): Path<(u64, String, String, u64)>,
    Json(body): Json<ReviewPull>,
) -> Result<StatusCode, ApiError> {
    let id = user.workspace(id)?;
    let repo = repo_name(owner, name)?;
    let token = user.github_token(&app)?;
    let verdict = match body.verdict {
        VerdictView::Approve => setup::Verdict::Approve,
        VerdictView::RequestChanges => setup::Verdict::RequestChanges,
        VerdictView::Comment => setup::Verdict::Comment,
    };
    setup::review_as(&token, &repo.to_string(), number, verdict, body.body.trim())
        .await
        .map_err(|e| {
            if let Some(setup::Rejected(why)) = e.downcast_ref() {
                ApiError::Status(StatusCode::UNPROCESSABLE_ENTITY, why.clone())
            } else if e.is::<setup::Refused>() {
                ApiError::Status(
                    StatusCode::FORBIDDEN,
                    format!("GitHub won't let you review PRs in {repo}."),
                )
            } else if e.is::<setup::Revoked>() {
                ApiError::Status(StatusCode::UNAUTHORIZED, "Sign in again first.".into())
            } else {
                e.into()
            }
        })?;
    tracing::info!(installation = id, %repo, number, verdict = ?verdict, "reviewed from the dashboard");
    let gh = installation(&app, id).await?;
    if body.queue {
        queue(&app, &gh, &token, &repo, number, false).await?;
        tracing::info!(installation = id, %repo, number, "queued from the dashboard");
    } else {
        relabeled(&*app.store, &*app.wake, id, repo, &[number]).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// The repo's queue labels, and its default branch, as the config file there sets them.
async fn repo_labels(
    app: &AppState,
    gh: &Installation,
    repo: &RepoName,
) -> Result<(Labels, String), ApiError> {
    labels_of(app, gh.default_branch_file(repo, REPO_CONFIG_PATH).await?)
}

/// The labels the config file sets, and the branch it is on.
fn labels_of(app: &AppState, file: BranchFile) -> Result<(Labels, String), ApiError> {
    let settings = RepoSettings::from_file(file.text.as_deref(), &app.repo_defaults)
        .map_err(|e| ApiError::Status(StatusCode::CONFLICT, e))?;
    Ok((settings.labels(), file.branch))
}

/// The PRs whose labels queue `number`: its stack's layers, or the PR alone.
async fn layers(gh: &Installation, repo: &RepoName, number: u64) -> anyhow::Result<Vec<u64>> {
    Ok(match gh.pull_request(repo, number).await?.stack {
        Some(stack) => gh.stack_layers(repo, stack.number).await?,
        None => vec![number],
    })
}

/// A call made as the user: GitHub's refusal is theirs to read.
fn as_user<T>(result: anyhow::Result<T>, repo: &RepoName) -> Result<T, ApiError> {
    result.map_err(|e| {
        if e.is::<setup::Refused>() {
            ApiError::Status(
                StatusCode::FORBIDDEN,
                format!("You need triage access to {repo} for that."),
            )
        } else if e.is::<setup::Revoked>() {
            ApiError::Status(StatusCode::UNAUTHORIZED, "Sign in again first.".into())
        } else {
            e.into()
        }
    })
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
        providers: Provider::SUPPORTED.map(ProviderView::from).into(),
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
    let branches: Vec<String> = settings.branches.iter().map(|b| format!("`{b}`")).collect();
    let branches = match branches.is_empty() {
        true => "the default branch only".to_owned(),
        false => format!("the default branch, {}", branches.join(", ")),
    };
    let list = format!(
        "- Trigger label: `{}`\n- Batch size: {}\n- Resolver: {} `{}`, effort `{}`\n- Verify commands: {verify}\n- Fix failed checks: {fix_ci}\n- Queues: {branches}",
        settings.label,
        settings.batch_size,
        settings.resolver.provider.name(),
        settings.resolver.model,
        settings.resolver.effort
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
    let mut api_keys = Vec::new();
    for provider in Provider::SUPPORTED {
        let key = app
            .vault
            .status(Scope::Installation(id), api_key_secret(provider))
            .await?;
        api_keys.push(ApiKeyView {
            provider: provider.into(),
            key: key.map(|k| SecretView {
                hint: k.hint,
                updated_at: k.updated_at,
            }),
        });
    }
    let scope = Scope::Installation(id);
    let chatgpt = ChatGptView {
        login: app
            .vault
            .status(scope, CHATGPT_LOGIN)
            .await?
            .map(|s| SecretView {
                hint: s.hint,
                updated_at: s.updated_at,
            }),
        sign_in: device_login(app, id).await?.map(|d| ChatGptSignInView {
            user_code: d.user_code,
            verification_url: d.verification_url,
            interval_secs: d.interval_secs as u32,
        }),
    };
    Ok(Json(SettingsView { api_keys, chatgpt }))
}

async fn device_login(app: &AppState, id: u64) -> anyhow::Result<Option<DeviceLogin>> {
    let Some(json) = app
        .vault
        .get(Scope::Installation(id), CHATGPT_DEVICE_LOGIN)
        .await?
    else {
        return Ok(None);
    };
    Ok(Some(serde_json::from_str(json.expose_secret())?))
}

/// Starts a sign-in to the workspace's ChatGPT plan, in place of any unfinished one. Kept in the
/// vault, not in memory: the user's next poll can reach another instance.
pub(crate) async fn start_chatgpt_login(
    State(app): State<AppState>,
    user: SignedIn,
    Path(id): Path<u64>,
) -> Result<Json<SettingsView>, ApiError> {
    let id = user.workspace(id)?;
    let device = app
        .chatgpt
        .start()
        .await
        .map_err(|e| ApiError::Status(StatusCode::BAD_GATEWAY, format!("{e:#}")))?;
    let hint = device.user_code.clone();
    set_json(
        &app,
        Scope::Installation(id),
        CHATGPT_DEVICE_LOGIN,
        &device,
        hint,
    )
    .await?;
    settings_view(&app, id).await
}

/// Asks OpenAI once whether the user has entered the code, and keeps the login once they have.
pub(crate) async fn poll_chatgpt_login(
    State(app): State<AppState>,
    user: SignedIn,
    Path(id): Path<u64>,
) -> Result<Json<SettingsView>, ApiError> {
    let id = user.workspace(id)?;
    let scope = Scope::Installation(id);
    let Some(device) = device_login(&app, id).await? else {
        return Err(ApiError::Status(
            StatusCode::CONFLICT,
            "no ChatGPT sign-in to finish: start one".into(),
        ));
    };
    match app.chatgpt.poll(&device).await {
        Ok(None) => {}
        Ok(Some(login)) => {
            set_json(&app, scope, CHATGPT_LOGIN, &login, login.hint()).await?;
            app.vault.delete(scope, CHATGPT_DEVICE_LOGIN).await?;
            tracing::info!(installation = id, "a ChatGPT login was added");
        }
        Err(e) => {
            app.vault.delete(scope, CHATGPT_DEVICE_LOGIN).await?;
            return Err(ApiError::Status(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("{e:#}"),
            ));
        }
    }
    settings_view(&app, id).await
}

async fn set_json(
    app: &AppState,
    scope: Scope,
    name: &str,
    value: &impl Serialize,
    hint: String,
) -> anyhow::Result<()> {
    app.vault
        .set_with_hint(scope, name, &serde_json::to_string(value)?, hint)
        .await
}

pub(crate) async fn delete_chatgpt_login(
    State(app): State<AppState>,
    user: SignedIn,
    Path(id): Path<u64>,
) -> Result<Json<SettingsView>, ApiError> {
    let id = user.workspace(id)?;
    let scope = Scope::Installation(id);
    app.vault.delete(scope, CHATGPT_LOGIN).await?;
    app.vault.delete(scope, CHATGPT_DEVICE_LOGIN).await?;
    tracing::info!(installation = id, "a ChatGPT login was removed");
    settings_view(&app, id).await
}

fn provider(id: &str) -> Result<Provider, ApiError> {
    Provider::parse(id)
        .filter(|p| Provider::SUPPORTED.contains(p))
        .ok_or_else(|| ApiError::Status(StatusCode::NOT_FOUND, format!("no provider {id:?}")))
}

/// Checks the key with its provider before it stores it, so a typo fails here and not in a
/// rebase.
pub(crate) async fn set_api_key(
    State(app): State<AppState>,
    user: SignedIn,
    Path((id, provider_id)): Path<(u64, String)>,
    Json(body): Json<SetSecret>,
) -> Result<Json<SettingsView>, ApiError> {
    let id = user.workspace(id)?;
    let provider = provider(&provider_id)?;
    let key = body.value.expose_secret().trim();
    if key.is_empty() {
        return Err(ApiError::bad_request("the key is empty"));
    }
    match check_api_key(provider, key).await {
        Ok(KeyCheck::Valid) => {}
        Ok(KeyCheck::Invalid(why)) => {
            return Err(ApiError::Status(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("{} rejected the key: {why}", provider.name()),
            ));
        }
        Err(e) => return Err(ApiError::Status(StatusCode::BAD_GATEWAY, format!("{e:#}"))),
    }
    app.vault
        .set(Scope::Installation(id), api_key_secret(provider), key)
        .await?;
    tracing::info!(
        installation = id,
        provider = provider.as_str(),
        "an API key was changed"
    );
    settings_view(&app, id).await
}

pub(crate) async fn delete_api_key(
    State(app): State<AppState>,
    user: SignedIn,
    Path((id, provider_id)): Path<(u64, String)>,
) -> Result<Json<SettingsView>, ApiError> {
    let id = user.workspace(id)?;
    let provider = provider(&provider_id)?;
    app.vault
        .delete(Scope::Installation(id), api_key_secret(provider))
        .await?;
    tracing::info!(
        installation = id,
        provider = provider.as_str(),
        "an API key was removed"
    );
    settings_view(&app, id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use mergepatrol_core::PrSummary;

    /// This repo queues through Mergepatrol too: its agents read the skill every repo gets.
    #[test]
    fn this_repo_has_the_skill_every_repo_gets() {
        assert_eq!(
            include_str!("../../../.agents/skills/mergepatrol/SKILL.md"),
            SKILL
        );
        assert_eq!(
            include_str!("../../../.claude/skills/mergepatrol/SKILL.md"),
            SKILL
        );
    }

    #[test]
    fn llm_stats_group_by_model_and_effort_costliest_first() {
        let run = |kind: &str, message: &str| Spend::parse(kind, message).unwrap();
        let stats = llm_stats(vec![
            run("resolved", "a.rs in 3 turns, $0.40, by m at high effort"),
            run("fix", "2 turns, $0.10, by m at high effort"),
            run("unfixed", "6 turns, $0.20, by m at high effort"),
            run("fix", "5 turns, $1.00, by m at max effort"),
            run("fix", "4 turns"),
        ]);
        let keys: Vec<_> = stats
            .iter()
            .map(|l| (l.model.as_deref(), l.effort.as_deref()))
            .collect();
        assert_eq!(
            keys,
            [
                (Some("m"), Some("max")),
                (Some("m"), Some("high")),
                (None, None)
            ]
        );
        assert_eq!((stats[1].rebase.runs, stats[1].rebase.turns), (1, 3));
        let fix = &stats[1].fix;
        assert_eq!((fix.runs, fix.failed, fix.turns), (2, 1, 8));
        assert!((fix.cost_usd - 0.30).abs() < 1e-9);
        assert_eq!((stats[2].fix.runs, stats[2].fix.unpriced), (1, 1));
    }

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
    fn an_entry_has_its_pull_request_fields_at_the_top() {
        let view = EntryView::new(entry(1, true, EntryState::Waiting));
        let json = serde_json::to_value(view).unwrap();
        assert_eq!(json["number"], 1);
        assert_eq!(json["title"], "");
        assert!(json.get("pull").is_none());
    }

    #[test]
    fn each_queued_pr_and_stack_layer_has_its_spot_in_merge_order() {
        let repo = RepoName::parse("o/r").unwrap();
        let mut stack = entry(9, false, EntryState::Waiting);
        stack.layers = vec![7, 8, 9];
        let lane = Lane {
            entries: vec![
                entry(1, false, EntryState::Held("waiting for a required review".into())),
                stack,
                entry(3, true, EntryState::Waiting),
                entry(4, false, EntryState::Active),
            ],
            ..Lane::default()
        };
        let key = LaneKey {
            installation_id: 1,
            repo: repo.clone(),
            trunk: "main".into(),
        };
        let spots = queue_spots(vec![(key, lane)]);
        let spot = |n| &spots[&(repo.clone(), n)];
        let places: Vec<u32> = [4, 3, 9, 1].into_iter().map(|n| spot(n).place).collect();
        assert_eq!(places, [1, 2, 3, 4]);
        assert!(spot(3).priority);
        assert_eq!(spot(7).place, 3);
        assert_eq!(spot(8).state, EntryStateView::Waiting);
        assert_eq!(spot(1).state, EntryStateView::Held);
        assert_eq!(spot(1).detail.as_deref(), Some("waiting for a required review"));
        assert!(spots.values().all(|s| s.of == 4));
        assert!(!spots.contains_key(&(repo, 2)));
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
