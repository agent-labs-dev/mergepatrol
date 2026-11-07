use std::collections::{HashMap, HashSet};

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use mergepatrol_core::{
    AgentSettings, ConfigPath, ConfigText, EFFORTS, Entry, EntryState, FixCiSettings, Labels, Lane,
    ModelOverrides, Person, PrSummary, Provider, RepoName, RepoSettings, Spend, SpendTask, Stage,
    Step, TaskModel, WorkspaceDefaults, current_step,
};
use mergepatrol_github::{
    App, AsUser, BranchFile, CheckRunState, ConfigFile, DiffOf, GitObject, InboxPull, Installation,
    Proposal, PullDetail, PullRef, PullRequest, SYMLINK_MODE, TimelineEvent, setup,
};
use mergepatrol_resolver::{DeviceLogin, KeyCheck, check_api_key};
use mergepatrol_store::{
    CHATGPT_DEVICE_LOGIN, CHATGPT_LOGIN, Change, DeveloperStats, EventRecord, ExitQuery,
    ExitRecord, ExitStats, InboxRow, LaneKey, RepoState, ReviewRecord, Scope, Slice, Store,
    api_key_secret,
};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::auth::Caller;
use crate::v1::{RepoStateView, RepoStatusView};
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum StageView {
    Rebase,
    Checks,
    Review,
    Lane,
}

/// What the merge lane is doing with its active PR.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum ActiveStepView {
    Rebasing,
    /// Its checks, or its batch's, must pass.
    Checks,
    Fixing,
    Merging,
}

impl From<Stage> for StageView {
    fn from(stage: Stage) -> Self {
        match stage {
            Stage::Rebase => StageView::Rebase,
            Stage::Checks => StageView::Checks,
            Stage::Review => StageView::Review,
            Stage::Lane => StageView::Lane,
        }
    }
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct EntryView {
    pub number: u32,
    pub priority: bool,
    pub state: EntryStateView,
    /// Where it is on its way to the trunk. Null while on hold for what is no stage, as a draft.
    pub stage: Option<StageView>,
    /// What the queue is doing with it now. Null unless `state` is `active`, or until its job
    /// has begun.
    pub step: Option<ActiveStepView>,
    /// The fixes it has used, for its checks.
    pub fixes: u32,
    /// The fixes it gets.
    pub fix_budget: u32,
    /// Why the PR is held.
    pub detail: Option<String>,
    /// Why the PR is held, typed.
    pub hold: Option<crate::v1::HoldView>,
    #[serde(flatten)]
    pub pull: PullView,
    /// A stack's PRs, bottom to top: `number` is the stack's. Empty for a PR.
    pub layers: Vec<u32>,
    /// The agent's review of the PR, or of the stack's layer it is held for, else its top: the
    /// newest, of whatever head.
    pub review: Option<crate::review::ReviewBriefView>,
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
    fn new(
        e: Entry,
        step: Option<ActiveStepView>,
        review: Option<crate::review::ReviewBriefView>,
    ) -> Self {
        let Entry {
            number,
            priority,
            state,
            summary: pr,
            layers,
            heads: _,
            recheck: _,
        } = e;
        let stage = state.stage().map(StageView::from);
        let (fixes, fix_budget) = (pr.fixes, pr.fix_budget);
        let (state, detail, hold) = match state {
            EntryState::Waiting => (EntryStateView::Waiting, None, None),
            EntryState::Held(reason) => (
                EntryStateView::Held,
                Some(reason.to_string()),
                Some((&reason).into()),
            ),
            EntryState::Active => (EntryStateView::Active, None, None),
        };
        EntryView {
            number: number as u32,
            priority,
            state,
            stage,
            step,
            fixes,
            fix_budget,
            detail,
            hold,
            pull: pr.into(),
            layers: layers.into_iter().map(|n| n as u32).collect(),
            review,
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
    /// Theirs and open, updated last first.
    pub authored: Vec<InboxPullView>,
    /// Asked of them, or of a team of theirs, updated last first.
    pub review_requested: Vec<InboxPullView>,
    /// Theirs, merged in the last week, last first.
    pub merged: Vec<InboxPullView>,
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
    /// When it merged, for a merged one.
    pub merged_at: Option<String>,
    /// The workspace the PR is in, to act on it in.
    pub workspace: u32,
    /// Whether MergeLoop runs on the repo, so the PR can be queued.
    pub queues: bool,
    /// Where the queue has it. `None` when it is not queued.
    pub queue: Option<QueueSpotView>,
    /// Comments and reviews by others since the user last opened it here, or last said
    /// something on it.
    pub unread: u32,
    /// Until when the user snoozed it, RFC 3339. A snooze is over once someone else says
    /// something on the PR.
    pub snoozed_until: Option<String>,
    /// Its review threads no one resolved yet, of the first 100.
    pub unresolved_threads: u32,
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
    /// Why it is held, typed: whether it waits on its author or on others.
    pub hold: Option<crate::v1::HoldReasonView>,
}

/// Puts a PR out of the user's inbox for a while.
#[derive(Deserialize, TS)]
#[ts(export)]
pub struct SnoozePull {
    /// From 1 to a year's.
    pub minutes: u32,
}

/// Asks the reviewers who have not answered to review, in a comment that mentions them.
#[derive(Deserialize, TS)]
#[ts(export)]
pub struct NudgePull {
    pub reviewers: Vec<String>,
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
    Review,
}

impl From<SpendTask> for SpendTaskView {
    fn from(t: SpendTask) -> Self {
        match t {
            SpendTask::Rebase => SpendTaskView::Rebase,
            SpendTask::Fix => SpendTaskView::Fix,
            SpendTask::Review => SpendTaskView::Review,
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
    /// Reviewing PRs, and answering questions about the reviews.
    pub review: LlmRunsView,
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
                    review: LlmRunsView::default(),
                });
                by.len() - 1
            }
        };
        match s.task {
            SpendTask::Rebase => by[i].rebase.add(&s),
            SpendTask::Fix => by[i].fix.add(&s),
            SpendTask::Review => by[i].review.add(&s),
        }
    }
    let cost = |l: &LlmStatsView| l.rebase.cost_usd + l.fix.cost_usd + l.review.cost_usd;
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
    /// The default branch has the config file, as of the last look.
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
    /// A model for one task over the default.
    #[serde(default)]
    pub overrides: ModelOverridesView,
    pub verify: Vec<String>,
    /// The agent fixes failed required checks.
    pub fix_ci: bool,
    /// The most fixes a PR gets, before it is ejected.
    pub fix_ci_attempts: u32,
    /// The branches with a queue besides the default branch.
    pub branches: Vec<String>,
    /// The agent's review, over the workspace's settings.
    #[serde(default)]
    pub review: crate::review::RepoReviewView,
}

/// Each task's own model: none runs the default.
#[derive(Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelOverridesView {
    /// Resolving a rebase's conflicts.
    pub rebase: Option<TaskModelView>,
    /// Fixing failed checks.
    pub fix: Option<TaskModelView>,
    /// Reviewing a PR, and answering questions about the review.
    pub review: Option<TaskModelView>,
}

#[derive(Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TaskModelView {
    #[ts(type = "\"anthropic\" | \"openai\" | \"openrouter\"")]
    pub provider: Provider,
    pub model: String,
}

impl From<ModelOverrides> for ModelOverridesView {
    fn from(t: ModelOverrides) -> Self {
        let view = |t: Option<TaskModel>| {
            t.map(|t| TaskModelView {
                provider: t.provider,
                model: t.model,
            })
        };
        ModelOverridesView {
            rebase: view(t.rebase),
            fix: view(t.fix),
            review: view(t.review),
        }
    }
}

impl From<ModelOverridesView> for ModelOverrides {
    fn from(v: ModelOverridesView) -> Self {
        let model = |v: Option<TaskModelView>| {
            v.map(|v| TaskModel {
                provider: v.provider,
                model: v.model.trim().to_owned(),
            })
        };
        ModelOverrides {
            rebase: model(v.rebase),
            fix: model(v.fix),
            review: model(v.review),
        }
    }
}

impl From<RepoSettings> for RepoSettingsView {
    fn from(s: RepoSettings) -> Self {
        RepoSettingsView {
            label: s.label,
            batch_size: s.batch_size as u32,
            provider: s.agent.provider,
            model: s.agent.model,
            effort: s.agent.effort,
            overrides: s.agent.overrides.into(),
            verify: s.verify,
            fix_ci: s.fix_ci.enabled,
            fix_ci_attempts: s.fix_ci.attempts,
            branches: s.branches,
            review: s.review.into(),
        }
    }
}

impl From<RepoSettingsView> for RepoSettings {
    fn from(v: RepoSettingsView) -> Self {
        RepoSettings {
            label: v.label.trim().to_owned(),
            batch_size: v.batch_size as usize,
            agent: AgentSettings {
                provider: v.provider,
                model: v.model.trim().to_owned(),
                effort: v.effort,
                overrides: v.overrides.into(),
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
            review: v.review.into(),
        }
    }
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct RepoConfigView {
    /// What to edit: the open PR's file, else the default branch's, over the defaults. The
    /// defaults when the file does not parse.
    pub settings: RepoSettingsView,
    /// What the repo follows where its file says nothing: `defaults` of `get_repo_defaults`.
    pub defaults: RepoSettingsView,
    /// A setting equal to its default follows it. False while the file spells such a setting
    /// out: that one stays put when the default changes, until the file is trimmed.
    pub follows: bool,
    pub efforts: Vec<String>,
    /// The providers a repo can choose.
    pub providers: Vec<ProviderView>,
    pub path: String,
    pub default_branch: String,
    pub has_file: bool,
    /// Why the file on the default branch does not parse.
    pub error: Option<String>,
    /// MergeLoop's open PR that changes the file.
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
    /// The models to pick from, the one to start from first.
    pub models: Vec<ModelView>,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct ModelView {
    /// As the provider's API takes it, and the config file names it.
    pub id: String,
    pub name: String,
}

impl From<Provider> for ProviderView {
    fn from(p: Provider) -> Self {
        ProviderView {
            id: p.as_str().into(),
            name: p.name().into(),
            models: p
                .models()
                .iter()
                .map(|m| ModelView {
                    id: m.id.into(),
                    name: m.name.into(),
                })
                .collect(),
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

#[derive(Deserialize)]
pub(crate) struct LanesQuery {
    /// Every repo's without one.
    repo: Option<String>,
}

pub(crate) async fn lanes(
    State(app): State<AppState>,
    caller: Caller,
    Path(id): Path<u64>,
    Query(q): Query<LanesQuery>,
) -> Result<Json<Vec<LaneView>>, ApiError> {
    let repo = q
        .repo
        .map(|r| RepoName::parse(&r).ok_or(ApiError::bad_request("repo must be owner/name")))
        .transpose()?;
    let (id, readable) = match &repo {
        Some(repo) => (caller.repo(&app, id, repo).await?, None),
        None => {
            let (id, readable) = caller.workspace_repos(&app, id).await?;
            (id, Some(readable))
        }
    };
    let lanes: Vec<(LaneKey, Lane)> = app
        .store
        .lanes(id)
        .await?
        .into_iter()
        .filter(|(key, _)| repo.as_ref().is_none_or(|r| key.repo == *r))
        .filter(|(key, _)| readable.as_ref().is_none_or(|r| r.contains(&key.repo)))
        .collect();
    let (lanes, reviews) = reviewed(&app, id, lanes).await?;
    let mut views = Vec::with_capacity(lanes.len());
    for (key, lane) in lanes {
        let merging = lane.merging.as_ref().map(|m| m.number);
        let mut entries = Vec::with_capacity(lane.entries.len());
        for e in in_merge_order(lane.entries) {
            let step = match (e.state == EntryState::Active, merging == Some(e.number)) {
                (true, true) => Some(ActiveStepView::Merging),
                // A job records its events on each of a stack's layers, not the stack.
                (true, false) => {
                    let top = e.layers.last().copied().unwrap_or(e.number);
                    active_step(&app, id, &key.repo, top).await?
                }
                (false, _) => None,
            };
            let shown = review_layer(&e);
            let review = reviews.get(&(key.repo.clone(), shown)).map(Into::into);
            entries.push(EntryView::new(e, step, review));
        }
        views.push(LaneView {
            repo: key.repo.to_string(),
            trunk: key.trunk,
            batch: lane.batch_pr.map(|n| n as u32),
            entries,
        });
    }
    Ok(Json(views))
}

/// What the job of active PR `number` is at, from its newest events.
async fn active_step(
    app: &AppState,
    id: u64,
    repo: &RepoName,
    number: u64,
) -> anyhow::Result<Option<ActiveStepView>> {
    let events = app.store.events(id, repo, Some(number), 20).await?;
    Ok(match current_step(events.iter().map(|e| e.kind.as_str())) {
        Some(Step::Rebasing) => Some(ActiveStepView::Rebasing),
        Some(Step::Checks) => Some(ActiveStepView::Checks),
        Some(Step::Fixing) => Some(ActiveStepView::Fixing),
        Some(Step::Waiting | Step::Held) | None => None,
    })
}

/// The lanes with each held PR's review hold as its newest review has it now, which a lane only
/// saves at its next tick (`Entry::with_review`), and the newest review of each PR in them.
pub(crate) async fn reviewed(
    app: &AppState,
    id: u64,
    lanes: Vec<(LaneKey, Lane)>,
) -> anyhow::Result<(Vec<(LaneKey, Lane)>, HashMap<(RepoName, u64), ReviewRecord>)> {
    let mut queued: HashMap<&RepoName, Vec<u64>> = HashMap::new();
    for (key, lane) in &lanes {
        let prs = lane.entries.iter().flat_map(Entry::prs);
        queued.entry(&key.repo).or_default().extend(prs);
    }
    let mut reviews = HashMap::new();
    for (r, prs) in queued {
        for review in app.store.latest_reviews(id, r, &prs).await? {
            reviews.insert((r.clone(), review.number), review);
        }
    }
    let lanes = lanes
        .into_iter()
        .map(|(key, mut lane)| {
            lane.entries = lane
                .entries
                .into_iter()
                .map(|e| e.with_review(|n| reviews.get(&(key.repo.clone(), n)).map(|r| r.status)))
                .collect();
            (key, lane)
        })
        .collect();
    Ok((lanes, reviews))
}

/// The PR whose review an entry shows: the stack's layer its hold is on, else its top.
fn review_layer(e: &Entry) -> u64 {
    match &e.state {
        EntryState::Held(hold) => hold.layer(),
        _ => None,
    }
    .or(e.layers.last().copied())
    .unwrap_or(e.number)
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
    caller: Caller,
    Path(id): Path<u64>,
    Query(q): Query<EventsQuery>,
) -> Result<Json<Vec<EventView>>, ApiError> {
    let repo = RepoName::parse(&q.repo).ok_or(ApiError::bad_request("repo must be owner/name"))?;
    let id = caller.repo(&app, id, &repo).await?;
    let events = app.store.events(id, &repo, q.pr, 200).await?;
    Ok(Json(events.into_iter().map(EventView::from).collect()))
}

impl From<EventRecord> for EventView {
    fn from(e: EventRecord) -> Self {
        EventView {
            spend: Spend::parse(&e.kind, &e.message).map(SpendView::from),
            repo: e.repo,
            pr: e.pr as u32,
            at: e.at,
            kind: e.kind,
            message: e.message,
        }
    }
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
    caller: Caller,
    Path(id): Path<u64>,
    Query(q): Query<FixQuery>,
) -> Result<Json<FixView>, ApiError> {
    let repo = RepoName::parse(&q.repo).ok_or(ApiError::bad_request("repo must be owner/name"))?;
    let id = caller.repo(&app, id, &repo).await?;
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
    /// One PR of `repo`, from any time: `days` does not narrow it.
    pr: Option<u64>,
}

impl HistoryQuery {
    pub(crate) fn repo(&self) -> Result<Option<RepoName>, ApiError> {
        self.repo
            .as_deref()
            .map(|r| RepoName::parse(r).ok_or(ApiError::bad_request("repo must be owner/name")))
            .transpose()
    }

    /// Of `repo`, or every repo, within the repos `within`.
    fn slice<'a>(&'a self, repo: Option<&'a RepoName>, within: &'a [String]) -> Slice<'a> {
        Slice {
            repo,
            author: self.author.as_deref(),
            within: Some(within),
        }
    }

    pub(crate) fn days(&self) -> Result<u32, ApiError> {
        match self.days {
            1..=365 => Ok(self.days),
            _ => Err(ApiError::bad_request("days must be from 1 to 365")),
        }
    }
}

pub(crate) async fn history(
    State(app): State<AppState>,
    caller: Caller,
    Path(id): Path<u64>,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<ExitsPage>, ApiError> {
    let (id, readable) = caller.workspace_repos(&app, id).await?;
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
            slice: q.slice(repo.as_ref(), readable.names()),
            outcomes,
            days: match q.pr {
                Some(_) if repo.is_some() => None,
                _ => Some(q.days()?),
            },
            before: q.before,
            limit: HISTORY_PAGE,
            pr: q.pr.filter(|_| repo.is_some()),
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
    caller: Caller,
    Path(id): Path<u64>,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<HistoryStatsView>, ApiError> {
    let (id, readable) = caller.workspace_repos(&app, id).await?;
    let (repo, days) = (q.repo()?, q.days()?);
    let slice = q.slice(repo.as_ref(), readable.names());
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

/// The repos this workspace's installation can access that the user can read, and whether
/// MergeLoop runs on each.
pub(crate) async fn repos(
    State(app): State<AppState>,
    caller: Caller,
    Path(id): Path<u64>,
) -> Result<Json<Vec<RepoView>>, ApiError> {
    let (id, readable) = caller.workspace_repos(&app, id).await?;
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
        .filter(|r| readable.contains(&r.repo))
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

pub(crate) async fn installation(app: &AppState, id: u64) -> Result<Installation, ApiError> {
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
    caller: Caller,
    Path((id, owner, name)): Path<(u64, String, String)>,
    Json(body): Json<SetRepo>,
) -> Result<StatusCode, ApiError> {
    let repo = repo_name(owner, name)?;
    let id = caller.repo_admin(&app, id, &repo).await?;
    app.store.set_repo_enabled(id, &repo, body.enabled).await?;
    tracing::info!(installation = id, %repo, enabled = ?body.enabled, "repo setting changed");
    Ok(StatusCode::NO_CONTENT)
}

/// How many open PRs the queue page lists below the queue.
const OPEN_PULLS: u32 = 50;

/// The repo's open PRs into its default branch that are not in its queue, updated last first.
/// From the inbox, so a viewer's polling reads nothing of GitHub. Until the inbox has read all of
/// the repo, read as the user: each viewer's polling spends their own rate limit, not the
/// queue's. A revoked token means signing in again.
pub(crate) async fn open_pulls(
    State(app): State<AppState>,
    caller: Caller,
    Path((id, owner, name)): Path<(u64, String, String)>,
) -> Result<Json<Vec<OpenPullView>>, ApiError> {
    let repo = repo_name(owner, name)?;
    let id = caller.repo(&app, id, &repo).await?;
    // The config file as the queue last read it, `Some(None)` for none: its trigger label says
    // which PRs are queued. Until both are read, as the user.
    let kept = match app.store.inbox_open(id, &repo).await? {
        Some(nodes) => app.store.config_read(id, &repo).await?.map(|c| (nodes, c)),
        None => None,
    };
    let (pulls, labels) = match kept {
        Some((nodes, config)) => {
            let pulls = nodes
                .into_iter()
                .map(serde_json::from_value)
                .collect::<Result<Vec<PullRequest>, _>>()
                .map_err(anyhow::Error::from)?;
            (pulls, labels_in(&app, id, &config).await?)
        }
        None => {
            let gh = AsUser::new(caller.github_token(&app)?)?;
            let file = as_user(gh.config_file(&repo).await, &repo)?;
            let (labels, trunk) = labels_of(&app, id, file).await?;
            let pulls = as_user(
                gh.open_pull_requests(&repo, &trunk, OPEN_PULLS).await,
                &repo,
            )?;
            (pulls, labels)
        }
    };
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
    Ok(Json(
        pulls
            .into_iter()
            .filter(|pr| !pr.has_label(&labels.trigger) && !queued.contains(&pr.number))
            .take(OPEN_PULLS as usize)
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
    /// The head commit's SHA.
    pub head: String,
    /// The base branch's commit, by SHA. With `head`, what the PR's diff is of.
    pub base: String,
    /// The description as GitHub renders it, sanitized by GitHub. In a private repo, images of
    /// its own files carry short-lived signed URLs, so any page can load them.
    pub body_html: String,
    pub labels: Vec<LabelView>,
    /// The head commit's check runs and commit statuses.
    pub checks: Vec<CheckRunView>,
    /// Oldest first, up to the last 100 events.
    pub timeline: Vec<TimelineItemView>,
    /// Its review comments: how many threads are open, of how many.
    pub review_threads: ReviewThreadsView,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct ReviewThreadsView {
    /// Unresolved, of the threads read.
    pub open: u32,
    pub total: u32,
    /// The threads read: the first 100. Under `total`, more may be open than `open` says.
    pub read: u32,
    /// The unresolved ones, in the PR's order.
    pub open_threads: Vec<ReviewThreadView>,
}

#[derive(Serialize, TS)]
#[ts(export)]
pub struct ReviewThreadView {
    pub path: String,
    pub line: Option<u32>,
    pub author: Option<String>,
    /// The first line of its first comment.
    pub title: String,
    pub url: String,
    /// Its comments, replies too.
    pub comments: u32,
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
        let read = d.threads.len() as u32;
        let open_threads: Vec<ReviewThreadView> = d
            .threads
            .into_iter()
            .filter(|t| !t.resolved)
            .map(|t| ReviewThreadView {
                path: t.path,
                line: t.line,
                author: t.author,
                title: t.title,
                url: t.url,
                comments: t.comments,
            })
            .collect();
        PullDetailView {
            head: d.head,
            base: d.base,
            review_threads: ReviewThreadsView {
                open: open_threads.len() as u32,
                total: d.thread_count.max(read),
                read,
                open_threads,
            },
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
    caller: Caller,
    Path((id, owner, name, number)): Path<(u64, String, String, u64)>,
) -> Result<Json<PullDetailView>, ApiError> {
    let repo = repo_name(owner, name)?;
    caller.repo(&app, id, &repo).await?;
    let gh = AsUser::new(caller.github_token(&app)?)?;
    let detail = as_user(
        gh.pull_detail(&repo, number, &app.signed_images).await,
        &repo,
    )?;
    Ok(Json(detail.into()))
}

#[derive(Deserialize)]
pub(crate) struct PullDiffQuery {
    /// One of the PR's commits. The whole PR without one.
    commit: Option<String>,
    /// The whole PR's head and base commits, as `pull_detail` gives them: with both, its diff
    /// is kept until they change.
    head: Option<String>,
    base: Option<String>,
}

/// A PR's changes, or one commit's, as `git diff` prints them.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct PullDiffView {
    pub patch: String,
}

/// An open PR's diff, or one of its commits', read as the user. Each user's diffs are kept by
/// what they are of, so a read again costs GitHub nothing.
pub(crate) async fn pull_diff(
    State(app): State<AppState>,
    caller: Caller,
    Path((id, owner, name, number)): Path<(u64, String, String, u64)>,
    Query(q): Query<PullDiffQuery>,
) -> Result<Json<PullDiffView>, ApiError> {
    let repo = repo_name(owner, name)?;
    caller.repo(&app, id, &repo).await?;
    fn sha<'a>(name: &str, sha: &'a Option<String>) -> Result<Option<&'a str>, ApiError> {
        match sha {
            Some(s) if s.len() != 40 || !s.bytes().all(|b| b.is_ascii_hexdigit()) => Err(
                ApiError::bad_request(&format!("{name} must be a commit SHA")),
            ),
            _ => Ok(sha.as_deref()),
        }
    }
    let of = match (
        sha("commit", &q.commit)?,
        sha("head", &q.head)?,
        sha("base", &q.base)?,
    ) {
        (Some(commit), _, _) => DiffOf::Commit(commit),
        (None, Some(head), Some(base)) => DiffOf::Pull(Some((head, base))),
        (None, None, None) => DiffOf::Pull(None),
        _ => return Err(ApiError::bad_request("head and base go together")),
    };
    let gh = AsUser::new(caller.github_token(&app)?)?;
    let patch = as_user(
        gh.pull_diff(&repo, number, of, &app.pull_diffs).await,
        &repo,
    )?;
    let patch = patch.as_ref().clone().ok_or_else(|| {
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
    #[serde(default)]
    pub priority: bool,
}

/// Queues the PR, or changes the priority of a queued one. The labels do it, put on as the
/// user, so GitHub checks that they may.
pub(crate) async fn queue_pull(
    State(app): State<AppState>,
    caller: Caller,
    Path((id, owner, name, number)): Path<(u64, String, String, u64)>,
    Json(body): Json<QueuePull>,
) -> Result<Json<QueuedView>, ApiError> {
    let repo = repo_name(owner, name)?;
    let id = caller.repo(&app, id, &repo).await?;
    let token = caller.github_token(&app)?;
    let gh = installation(&app, id).await?;
    queue(&app, &gh, &token, &repo, number, body.priority).await?;
    tracing::info!(installation = id, %repo, number, priority = body.priority, "queued from the dashboard");
    let state = app.store.repo_state(id, &repo).await?;
    let status = RepoStatusView::read(&app, &repo, Some(id), state.as_ref()).await?;
    Ok(Json(QueuedView::new(status)))
}

/// A PR just queued: what the repo's status says will become of it.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct QueuedView {
    /// "Queued.", or that it goes on hold, or what the agent cannot do for it, and why.
    pub message: String,
    /// As `GET /api/v1` says it.
    pub mergeloop: RepoStatusView,
}

impl QueuedView {
    fn new(status: RepoStatusView) -> Self {
        let mut message = match (status.state, &status.hold) {
            (RepoStateView::Off, _) => {
                "Labeled, but MergeLoop is off on this repo: nothing merges it.".to_owned()
            }
            (RepoStateView::Paused, _) => format!(
                "Queued, but the queue is paused: {}.",
                status.config_error.as_deref().unwrap_or_default()
            ),
            (RepoStateView::On, Some(hold)) => {
                format!("Queued, but it goes on hold: {}.", hold.message)
            }
            (RepoStateView::On, None) => "Queued.".to_owned(),
        };
        if let Some(missing) = &status.missing_key {
            message.push_str(&format!(" Also, {}.", missing.message));
        }
        QueuedView {
            message,
            mergeloop: status,
        }
    }
}

/// Takes the PR out of the queue, and its stack if it is in one, as the user.
pub(crate) async fn dequeue_pull(
    State(app): State<AppState>,
    caller: Caller,
    Path((id, owner, name, number)): Path<(u64, String, String, u64)>,
) -> Result<StatusCode, ApiError> {
    let repo = repo_name(owner, name)?;
    let id = caller.repo(&app, id, &repo).await?;
    let token = caller.github_token(&app)?;
    let gh = installation(&app, id).await?;
    dequeue(&app, &gh, &token, &repo, number).await?;
    tracing::info!(installation = id, %repo, number, "dequeued from the dashboard");
    Ok(StatusCode::NO_CONTENT)
}

/// What `/api/v1` says of a PR, and its latest queue events.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct PullStatusView {
    #[serde(flatten)]
    pub state: crate::v1::PullStateView,
    /// The latest, newest first. One of kind `merged`, `ejected`, `dequeued` or `cancelled` is
    /// how it last left the queue.
    pub events: Vec<EventView>,
}

/// How many of a PR's events its status has.
const STATUS_EVENTS: u32 = 10;

pub(crate) async fn pull_status(
    State(app): State<AppState>,
    caller: Caller,
    Path((id, owner, name, number)): Path<(u64, String, String, u64)>,
) -> Result<Json<PullStatusView>, ApiError> {
    let repo = repo_name(owner, name)?;
    let id = caller.repo(&app, id, &repo).await?;
    let state = crate::v1::statuses(&app, &repo, Some(id), &[number])
        .await?
        .remove(0);
    let events = app
        .store
        .events(id, &repo, Some(number), STATUS_EVENTS)
        .await?;
    Ok(Json(PullStatusView {
        state,
        events: events.into_iter().map(EventView::from).collect(),
    }))
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

/// These PRs changed from the dashboard or MCP: their labels, or a draft made ready. The queue
/// wakes to read them again now, before the webhooks come to mark its reads of them stale.
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
    caller: Caller,
    Path((id, owner, name, number)): Path<(u64, String, String, u64)>,
) -> Result<StatusCode, ApiError> {
    let repo = repo_name(owner, name)?;
    let id = caller.repo(&app, id, &repo).await?;
    let token = caller.github_token(&app)?;
    as_user(
        setup::close_pull_as(&token, &repo.to_string(), number).await,
        &repo,
    )?;
    app.store.inbox_closed(id, &repo, number).await?;
    tracing::info!(installation = id, %repo, number, "closed from the dashboard");
    Ok(StatusCode::NO_CONTENT)
}

/// Marks a draft PR ready for review, as the user. The queue holds a draft, so it wakes to read
/// the PR again; the You tab moves it out of the drafts now.
pub(crate) async fn ready_pull(
    State(app): State<AppState>,
    caller: Caller,
    Path((id, owner, name, number)): Path<(u64, String, String, u64)>,
) -> Result<StatusCode, ApiError> {
    let repo = repo_name(owner, name)?;
    let id = caller.repo(&app, id, &repo).await?;
    let token = caller.github_token(&app)?;
    match setup::ready_for_review_as(&token, &repo.to_string(), number).await {
        Err(e) if e.is::<setup::Refused>() => {
            return Err(ApiError::Status(
                StatusCode::FORBIDDEN,
                format!("You need write access to {repo}, or to have opened the PR, for that."),
            ));
        }
        result => as_user(result, &repo)?,
    }
    published(&*app.store, &*app.wake, id, repo.clone(), number).await?;
    tracing::info!(installation = id, %repo, number, "marked ready from the dashboard");
    Ok(StatusCode::NO_CONTENT)
}

/// The draft PR was marked ready from the dashboard or MCP: out of the You tab's drafts now, and
/// the queue, which holds drafts, wakes to read it again.
pub(crate) async fn published(
    store: &dyn Store,
    wake: &dyn Wake,
    id: u64,
    repo: RepoName,
    number: u64,
) -> Result<(), ApiError> {
    store.inbox_ready(id, &repo, number).await?;
    relabeled(store, wake, id, repo, &[number]).await;
    Ok(())
}

/// How long the You tab lists a merged PR.
const MERGED_FOR: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);

/// The user's inbox, as webhooks keep it, with where the queue has each PR.
pub(crate) async fn you(
    State(app): State<AppState>,
    caller: Caller,
) -> Result<Json<YouView>, ApiError> {
    let user = caller.user()?;
    let ids = user.installations();
    let inbox = app.store.inbox(user.login(), ids, MERGED_FOR).await?;
    // Authored or asked to review is not enough: the user may since have lost the repo.
    let mut readable = HashMap::new();
    for &id in ids {
        readable.insert(id, user.readable(&app, id).await?);
    }
    let on: HashSet<(u64, RepoName)> = app
        .store
        .repos()
        .await?
        .into_iter()
        .filter(|r| ids.contains(&r.installation_id) && r.on())
        .map(|r| (r.installation_id, r.repo))
        .collect();
    let mut spots = HashMap::new();
    for &id in ids {
        let (lanes, _) = reviewed(&app, id, app.store.lanes(id).await?).await?;
        spots.extend(queue_spots(lanes));
    }
    let view = |rows: Vec<InboxRow>| -> Vec<InboxPullView> {
        rows.into_iter()
            .filter_map(|row| {
                let p = InboxPull::from_node(&row.node)
                    .inspect_err(|e| tracing::warn!(error = %format!("{e:#}"), "an inbox PR"))
                    .ok()?;
                if !readable.get(&row.installation_id)?.contains(&p.repo) {
                    return None;
                }
                Some(InboxPullView {
                    number: p.pr.number as u32,
                    base: p.pr.merges_into().to_owned(),
                    draft: p.pr.is_draft,
                    merged_at: (p.state == "merged").then(|| p.closed_at.clone()).flatten(),
                    workspace: row.installation_id as u32,
                    queues: on.contains(&(row.installation_id, p.repo.clone())),
                    queue: spots.get(&(p.repo.clone(), p.pr.number)).cloned(),
                    unread: unread(&p.activity, user.login(), row.seen_at.as_deref()),
                    snoozed_until: row
                        .snoozed
                        .filter(|(at, _)| unread(&p.activity, user.login(), Some(at)) == 0)
                        .map(|(_, until)| until),
                    unresolved_threads: p.pr.unresolved_threads,
                    pull: p.pr.summary().into(),
                    updated_at: p.updated_at,
                    repo: p.repo.to_string(),
                })
            })
            .collect()
    };
    Ok(Json(YouView {
        authored: view(inbox.authored),
        review_requested: view(inbox.requested),
        merged: view(inbox.merged),
    }))
}

/// What others said since `me` last opened the PR, or last said something on it: GitHub's
/// times compare as strings.
fn unread(activity: &[(String, String)], me: &str, seen: Option<&str>) -> u32 {
    let mine = activity
        .iter()
        .filter(|(who, _)| who.eq_ignore_ascii_case(me))
        .map(|(_, at)| at.as_str())
        .max();
    let since = seen.max(mine).unwrap_or("");
    activity
        .iter()
        .filter(|(who, at)| !who.eq_ignore_ascii_case(me) && at.as_str() > since)
        .count() as u32
}

/// The user opened the PR in the dashboard: what came before is read.
pub(crate) async fn seen_pull(
    State(app): State<AppState>,
    caller: Caller,
    Path((id, owner, name, number)): Path<(u64, String, String, u64)>,
) -> Result<StatusCode, ApiError> {
    let id = caller.workspace(id)?;
    let repo = repo_name(owner, name)?;
    app.store
        .inbox_seen(caller.user()?.login(), id, &repo, number)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Puts the PR out of the user's inbox for `minutes`.
pub(crate) async fn snooze_pull(
    State(app): State<AppState>,
    caller: Caller,
    Path((id, owner, name, number)): Path<(u64, String, String, u64)>,
    Json(body): Json<SnoozePull>,
) -> Result<StatusCode, ApiError> {
    let id = caller.workspace(id)?;
    let repo = repo_name(owner, name)?;
    if !(1..=366 * 24 * 60).contains(&body.minutes) {
        return Err(ApiError::bad_request("minutes must be from 1 to a year's"));
    }
    let snooze = std::time::Duration::from_secs(u64::from(body.minutes) * 60);
    app.store
        .inbox_snooze(caller.user()?.login(), id, &repo, number, Some(snooze))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Puts a snoozed PR back in the user's inbox.
pub(crate) async fn unsnooze_pull(
    State(app): State<AppState>,
    caller: Caller,
    Path((id, owner, name, number)): Path<(u64, String, String, u64)>,
) -> Result<StatusCode, ApiError> {
    let id = caller.workspace(id)?;
    let repo = repo_name(owner, name)?;
    app.store
        .inbox_snooze(caller.user()?.login(), id, &repo, number, None)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Mentions the reviewers in a comment as the user, so GitHub tells them.
pub(crate) async fn nudge_pull(
    State(app): State<AppState>,
    caller: Caller,
    Path((id, owner, name, number)): Path<(u64, String, String, u64)>,
    Json(body): Json<NudgePull>,
) -> Result<StatusCode, ApiError> {
    let repo = repo_name(owner, name)?;
    let id = caller.repo(&app, id, &repo).await?;
    let login = |l: &String| {
        !l.is_empty() && l.len() <= 39 && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    };
    if body.reviewers.is_empty() || !body.reviewers.iter().all(login) {
        return Err(ApiError::bad_request("reviewers must be GitHub logins"));
    }
    let mentions: Vec<String> = body.reviewers.iter().map(|r| format!("@{r}")).collect();
    let text = format!(
        "{}, could you review this when you have a moment?",
        mentions.join(" ")
    );
    let token = caller.github_token(&app)?;
    as_user(
        setup::comment_as(&token, &repo.to_string(), number, &text).await,
        &repo,
    )?;
    app.store.inbox_changed(id, &repo, number).await?;
    tracing::info!(installation = id, %repo, number, "nudged from the dashboard");
    Ok(StatusCode::NO_CONTENT)
}

/// Where the queue has each PR of these lanes. A stack's layers each take the stack's spot.
fn queue_spots(lanes: Vec<(LaneKey, Lane)>) -> HashMap<(RepoName, u64), QueueSpotView> {
    let mut spots = HashMap::new();
    for (key, lane) in lanes {
        let entries = in_merge_order(lane.entries);
        let of = entries.len() as u32;
        for (i, e) in entries.into_iter().enumerate() {
            let (state, detail, hold) = match e.state {
                EntryState::Waiting => (EntryStateView::Waiting, None, None),
                EntryState::Held(reason) => (
                    EntryStateView::Held,
                    Some(reason.to_string()),
                    Some(crate::v1::HoldView::from(&reason).reason),
                ),
                EntryState::Active => (EntryStateView::Active, None, None),
            };
            let spot = QueueSpotView {
                state,
                place: i as u32 + 1,
                of,
                priority: e.priority,
                detail,
                hold,
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
    caller: Caller,
    Path((id, owner, name, number)): Path<(u64, String, String, u64)>,
    Json(body): Json<ReviewPull>,
) -> Result<StatusCode, ApiError> {
    let repo = repo_name(owner, name)?;
    let id = caller.repo(&app, id, &repo).await?;
    let token = caller.github_token(&app)?;
    let gh = installation(&app, id).await?;
    let text = body.body.trim();
    let review = match body.verdict {
        VerdictView::Approve => {
            let login = match &caller {
                Caller::User(user) => Some(user.login().to_owned()),
                Caller::Token(_) => as_user(setup::login_of(&token).await, &repo)?,
            };
            match login {
                Some(login) => approval(&gh, &repo, number, &login).await?,
                // A workflow's token is no user's, so no PR's author.
                None => Some(setup::Verdict::Approve),
            }
        }
        VerdictView::RequestChanges => Some(setup::Verdict::RequestChanges),
        VerdictView::Comment => Some(setup::Verdict::Comment),
    };
    match review {
        Some(verdict) => post_review(&token, &repo, number, verdict, text).await?,
        // GitHub takes no empty comment, and the author's approval has nothing else to post.
        None if !text.is_empty() => {
            post_review(&token, &repo, number, setup::Verdict::Comment, text).await?;
        }
        None => {}
    }
    tracing::info!(installation = id, %repo, number, review = ?review, "reviewed from the dashboard");
    // The inbox shows the review now; GitHub's reads, a moment later, settle it.
    match &caller {
        Caller::User(user) if review != Some(setup::Verdict::Comment) => {
            app.store
                .inbox_reviewed(id, &repo, number, user.login())
                .await?;
        }
        _ => app.store.inbox_changed(id, &repo, number).await?,
    }
    if body.queue {
        queue(&app, &gh, &token, &repo, number, false).await?;
        tracing::info!(installation = id, %repo, number, "queued from the dashboard");
    } else {
        relabeled(&*app.store, &*app.wake, id, repo, &[number]).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// What `login`'s approval posts: `None` for a PR they opened, which GitHub lets no author
/// approve. The author's approval posts no review, and still takes the PR into the queue: where
/// GitHub asks for an approval before it merges, the queue holds it there for a reviewer's.
pub(crate) async fn approval(
    gh: &Installation,
    repo: &RepoName,
    number: u64,
    login: &str,
) -> Result<Option<setup::Verdict>, ApiError> {
    let pr = gh.pull_request(repo, number).await?;
    Ok((!pr.authored_by(login)).then_some(setup::Verdict::Approve))
}

/// Reviews the PR as the user, with GitHub's refusal theirs to read.
async fn post_review(
    token: &str,
    repo: &RepoName,
    number: u64,
    verdict: setup::Verdict,
    body: &str,
) -> Result<(), ApiError> {
    setup::review_as(token, &repo.to_string(), number, verdict, body)
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
        })
}

/// The repo's queue labels, and its default branch, as the config file there sets them.
async fn repo_labels(
    app: &AppState,
    gh: &Installation,
    repo: &RepoName,
) -> Result<(Labels, String), ApiError> {
    let file = gh.config_file(repo).await?;
    labels_of(app, gh.id, file).await
}

/// The labels the config file sets, and the branch it is on.
async fn labels_of(
    app: &AppState,
    id: u64,
    file: ConfigFile,
) -> Result<(Labels, String), ApiError> {
    let config = ConfigText {
        at: file.at,
        text: file.file.text,
    };
    Ok((labels_in(app, id, &config).await?, file.file.branch))
}

/// The labels a config file sets: the workspace's where it says nothing, or there is none.
async fn labels_in(app: &AppState, id: u64, config: &ConfigText) -> Result<Labels, ApiError> {
    let defaults = inherited(app, id).await?;
    let settings = config
        .settings(&defaults)
        .map_err(|e| ApiError::Status(StatusCode::CONFLICT, e))?;
    Ok(settings.labels())
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

/// What the workspace's repos follow where their config files say nothing.
async fn inherited(app: &AppState, id: u64) -> anyhow::Result<RepoSettings> {
    app.store.inherited(id, &app.repo_defaults).await
}

/// The repo of a URL's `{owner}/{name}`. Each is one path segment, but `%2F` decodes to a
/// slash: a repo stored with one would break every read of the repos.
pub(crate) fn repo_name(owner: String, name: String) -> Result<RepoName, ApiError> {
    RepoName::parse(&format!("{owner}/{name}")).ok_or_else(|| ApiError::bad_request("not a repo"))
}

/// MergeLoop's branch for the PR that changes a repo's config file.
const CONFIG_BRANCH: &str = "mergeloop/config";

/// The skill that tells a repo's agents how its queue works. The setup PR adds it.
const SKILL: &str = include_str!("skill.md");
/// The skill's dir in a skills dir.
const SKILL_NAME: &str = "mergeloop";
/// Its dir from before the rename, which an update of the skill moves to `SKILL_NAME`.
const LEGACY_SKILL_NAME: &str = "mergepatrol";
/// Where agents look for repo skills: `.agents` for most, `.claude` for Claude Code.
const SKILL_DIRS: [&str; 2] = [".agents/skills", ".claude/skills"];

/// The repo's settings as its default branch has them, and the PR that would change them.
pub(crate) async fn repo_config(
    State(app): State<AppState>,
    caller: Caller,
    Path((id, owner, name)): Path<(u64, String, String)>,
) -> Result<Json<RepoConfigView>, ApiError> {
    let repo = repo_name(owner, name)?;
    let id = caller.repo(&app, id, &repo).await?;
    let gh = installation(&app, id).await?;
    let ConfigFile { at, file } = gh.config_file(&repo).await?;
    let defaults = &at.inherit(&inherited(&app, id).await?);
    let error = RepoSettings::from_file_at(at, file.text.as_deref(), defaults).err();
    let pull_request = gh.open_pr_from(&repo, CONFIG_BRANCH).await?;
    // Edits start from the open PR, so updating it keeps the changes already in it.
    let text = match pull_request {
        Some(_) => gh.branch_file(&repo, CONFIG_BRANCH, at.path()).await?,
        None => file.text.clone(),
    };
    let settings = RepoSettings::from_file_at(at, text.as_deref(), defaults)
        .unwrap_or_else(|_| defaults.clone());
    let follows = text
        .as_deref()
        .is_none_or(|t| settings.written_in(t, defaults));
    Ok(Json(RepoConfigView {
        follows,
        settings: settings.into(),
        defaults: defaults.clone().into(),
        efforts: EFFORTS.map(str::to_owned).into(),
        providers: Provider::SUPPORTED.map(ProviderView::from).into(),
        path: at.path().into(),
        has_file: file.text.is_some(),
        default_branch: file.branch,
        error,
        pull_request: pull_request.map(PullRequestRef::from),
    }))
}

/// Opens a PR that writes these settings to the repo's config file, or updates the one open, and
/// queues it if the repo queues. The file says only what differs from the workspace.
pub(crate) async fn propose_repo_config(
    State(app): State<AppState>,
    caller: Caller,
    Path((id, owner, name)): Path<(u64, String, String)>,
    Json(body): Json<RepoSettingsView>,
) -> Result<Json<UpdatePullView>, ApiError> {
    caller.workspace(id)?;
    let repo = repo_name(owner, name)?;
    let id = caller.repo(&app, id, &repo).await?;
    let token = caller.github_token(&app)?;
    let settings = RepoSettings::from(body);
    settings
        .check()
        .map_err(|e| ApiError::Status(StatusCode::UNPROCESSABLE_ENTITY, e))?;
    let gh = installation(&app, id).await?;
    let ConfigFile { at, file: base } = gh.config_file(&repo).await?;
    let defaults = at.inherit(&inherited(&app, id).await?);
    let content = settings.to_yaml(&defaults);
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
    let open = gh.open_pr_from(&repo, CONFIG_BRANCH).await?;
    let on_branch = match open {
        Some(_) => gh.branch_file(&repo, CONFIG_BRANCH, at.path()).await?,
        None => None,
    };
    let loses_comments = [base.text.as_deref(), on_branch.as_deref()]
        .into_iter()
        .flatten()
        .any(|t| settings.loses_comments_of(t, &defaults));
    // A queued PR stays queued through a push: this one has to wait for a human.
    if let Some(pr) =
        open.filter(|_| dequeues_first(loses_comments, base.text.as_deref(), &defaults))
    {
        dequeue(&app, &gh, &token, &repo, pr.number).await?;
    }
    let (title, body) = config_pr(&settings, at, base.text.is_some(), &base.branch, &skills);
    let mut files = vec![(at.path(), content.as_str())];
    files.extend(skills.iter().map(|p| (p.as_str(), SKILL)));
    let pr = gh
        .propose_file(
            &repo,
            Proposal {
                base: &base,
                files: &files,
                removed: &[],
                branch: CONFIG_BRANCH,
                title,
                body: &body,
            },
        )
        .await?;
    tracing::info!(installation = id, %repo, pr = pr.number, "proposed repo settings");
    let pull_request = pr.into();
    let file = UpdatedFileView::Settings;
    Ok(Json(if loses_comments {
        UpdatePullView::held(file, pull_request, LOSES_COMMENTS)
    } else if repo_queues(&app, id, &repo).await? {
        queue_update(&app, &gh, &token, &repo, file, pull_request).await
    } else {
        UpdatePullView::held(file, pull_request, "This repo does not queue yet.")
    }))
}

/// Whether a rewrite of the open PR must take it out of the queue first. A default branch that
/// does not parse has no labels to read and no queue running: the PR may be its repair.
fn dequeues_first(loses_comments: bool, base: Option<&str>, defaults: &RepoSettings) -> bool {
    loses_comments && RepoSettings::from_file(base, defaults).is_ok()
}

async fn repo_queues(app: &AppState, id: u64, repo: &RepoName) -> anyhow::Result<bool> {
    Ok(app
        .store
        .repo_state(id, repo)
        .await?
        .is_some_and(|s| s.on()))
}

/// What a workspace sets for every repo that leaves it out of its config file.
#[derive(Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkspaceDefaultsView {
    pub label: String,
    pub batch_size: u32,
    #[ts(type = "\"anthropic\" | \"openai\" | \"openrouter\"")]
    pub provider: Provider,
    pub model: String,
    pub effort: String,
    /// A model for one task over the default.
    #[serde(default)]
    pub overrides: ModelOverridesView,
    /// The agent fixes failed required checks.
    pub fix_ci: bool,
    /// The most fixes a PR gets, before it is ejected.
    pub fix_ci_attempts: u32,
}

impl From<WorkspaceDefaults> for WorkspaceDefaultsView {
    fn from(d: WorkspaceDefaults) -> Self {
        WorkspaceDefaultsView {
            label: d.label,
            batch_size: d.batch_size as u32,
            provider: d.agent.provider,
            model: d.agent.model,
            effort: d.agent.effort,
            overrides: d.agent.overrides.into(),
            fix_ci: d.fix_ci.enabled,
            fix_ci_attempts: d.fix_ci.attempts,
        }
    }
}

impl From<WorkspaceDefaultsView> for WorkspaceDefaults {
    fn from(v: WorkspaceDefaultsView) -> Self {
        WorkspaceDefaults {
            label: v.label.trim().to_owned(),
            batch_size: v.batch_size as usize,
            agent: AgentSettings {
                provider: v.provider,
                model: v.model.trim().to_owned(),
                effort: v.effort,
                overrides: v.overrides.into(),
            },
            fix_ci: FixCiSettings {
                enabled: v.fix_ci,
                attempts: v.fix_ci_attempts,
            },
        }
    }
}

/// The workspace's defaults, and what to pick them from.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct RepoDefaultsView {
    pub defaults: WorkspaceDefaultsView,
    pub efforts: Vec<String>,
    /// The providers a repo can choose.
    pub providers: Vec<ProviderView>,
}

async fn defaults_view(app: &AppState, id: u64) -> Result<Json<RepoDefaultsView>, ApiError> {
    Ok(Json(RepoDefaultsView {
        defaults: WorkspaceDefaults::from(inherited(app, id).await?).into(),
        efforts: EFFORTS.map(str::to_owned).into(),
        providers: Provider::SUPPORTED.map(ProviderView::from).into(),
    }))
}

pub(crate) async fn repo_defaults(
    State(app): State<AppState>,
    caller: Caller,
    Path(id): Path<u64>,
) -> Result<Json<RepoDefaultsView>, ApiError> {
    let id = caller.workspace(id)?;
    defaults_view(&app, id).await
}

/// Sets what every repo follows where its config file says nothing, at once and with no PR.
pub(crate) async fn set_repo_defaults(
    State(app): State<AppState>,
    caller: Caller,
    Path(id): Path<u64>,
    Json(body): Json<WorkspaceDefaultsView>,
) -> Result<Json<RepoDefaultsView>, ApiError> {
    let id = caller.workspace(id)?;
    let defaults = WorkspaceDefaults::from(body);
    defaults
        .clone()
        .over(&app.repo_defaults)
        .check()
        .map_err(|e| ApiError::Status(StatusCode::UNPROCESSABLE_ENTITY, e))?;
    app.store.set_repo_defaults(id, &defaults).await?;
    tracing::info!(installation = id, "set the repo defaults");
    defaults_view(&app, id).await
}

/// MergeLoop's branch for the PR that updates a repo's skill.
const SKILL_BRANCH: &str = "mergeloop/skill";

/// Where a repo's copy of the skill is against the one MergeLoop ships.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum SkillStateView {
    /// Every copy is the current skill.
    Current,
    /// A copy is older, or edited, or one skills dir lacks it.
    Outdated,
    /// No copy at all.
    Missing,
}

/// A repo's `mergeloop` skill on its default branch.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct SkillView {
    pub state: SkillStateView,
    /// The copies an update writes.
    pub paths: Vec<String>,
    /// The copies under the skill's name from before the rename, which an update deletes.
    pub legacy: Vec<String>,
    pub default_branch: String,
    /// MergeLoop's open PR that updates it.
    pub pull_request: Option<PullRequestRef>,
    /// The open PR already writes the current skill to every copy an update writes: updating
    /// it changes nothing.
    pub pull_request_current: bool,
}

/// The state of the skill's copies, by path, with the `legacy` copies there are, and the paths
/// an update writes.
fn skill_plan(
    copies: &[(String, Option<String>)],
    legacy: &[String],
) -> (SkillStateView, Vec<String>) {
    let stale: Vec<String> = copies
        .iter()
        .filter(|(_, text)| text.as_deref() != Some(SKILL))
        .map(|(path, _)| path.clone())
        .collect();
    let state = if stale.is_empty() && legacy.is_empty() {
        SkillStateView::Current
    } else if copies.iter().all(|(_, text)| text.is_none()) && legacy.is_empty() {
        SkillStateView::Missing
    } else {
        SkillStateView::Outdated
    };
    (state, stale)
}

/// The skill's file in skills dir `dir`, under the skill's `name`.
fn skill_file(dir: &str, name: &str) -> String {
    format!("{dir}/{name}/SKILL.md")
}

/// What a lookup of the skill reads for each skills dir: each step of its path, which must be
/// a dir and not a link, the skill on the default branch, the skill on the update's branch, and
/// the copy under its old name on the default branch.
fn skill_lookups(dir: &str) -> [(String, String); SKILL_LOOKUPS] {
    let top = dir.split('/').next().unwrap_or(dir);
    let file = skill_file(dir, SKILL_NAME);
    [
        ("HEAD".into(), top.into()),
        ("HEAD".into(), dir.into()),
        ("HEAD".into(), format!("{dir}/{SKILL_NAME}")),
        ("HEAD".into(), file.clone()),
        (format!("refs/heads/{SKILL_BRANCH}"), file),
        ("HEAD".into(), skill_file(dir, LEGACY_SKILL_NAME)),
    ]
}

const SKILL_LOOKUPS: usize = 6;

/// One skills dir's copy, as `skill_copy` reads it.
#[derive(Debug, PartialEq, Eq)]
struct SkillCopy {
    /// On the default branch.
    text: Option<String>,
    /// On the update's branch.
    on_branch: Option<String>,
    /// Whether the default branch has the copy under the skill's old name.
    legacy: bool,
}

/// One skills dir's copy, from its `skill_lookups`: `None` when a step of its path is a link
/// to elsewhere, as `.claude/skills` to `.agents/skills`, since a commit cannot write through
/// it and the link shows the skill there already.
fn skill_copy(objects: &[Option<GitObject>]) -> Option<SkillCopy> {
    let [top, dir, skill, file, branch, legacy] = objects else {
        return None;
    };
    let link = |o: &Option<GitObject>| matches!(o, Some(GitObject::Blob(_)));
    let file_link = matches!(skill, Some(GitObject::Tree(entries))
        if entries.iter().any(|(name, mode)| name == "SKILL.md" && *mode == SYMLINK_MODE));
    if link(top) || link(dir) || link(skill) || file_link {
        return None;
    }
    let text = |o: &Option<GitObject>| match o {
        Some(GitObject::Blob(t)) => Some(t.clone()),
        _ => None,
    };
    Some(SkillCopy {
        text: text(file),
        on_branch: text(branch),
        legacy: matches!(legacy, Some(GitObject::Blob(_))),
    })
}

/// The skill as the repo has it, in one read: its state, the paths an update writes, whether
/// the open update PR has the current skill there already, and the default branch's head.
async fn repo_skill(
    gh: &Installation,
    repo: &RepoName,
) -> Result<(SkillView, BranchFile), ApiError> {
    let lookups: Vec<(String, String)> = SKILL_DIRS.iter().flat_map(|d| skill_lookups(d)).collect();
    let refs: Vec<(&str, &str)> = lookups
        .iter()
        .map(|(r, p)| (r.as_str(), p.as_str()))
        .collect();
    let found = gh.objects_at(repo, &refs, SKILL_BRANCH).await?;
    let mut copies = Vec::new();
    let mut on_branch = Vec::new();
    let mut legacy = Vec::new();
    for (dir, objects) in SKILL_DIRS.iter().zip(found.objects.chunks(SKILL_LOOKUPS)) {
        if let Some(copy) = skill_copy(objects) {
            let path = skill_file(dir, SKILL_NAME);
            on_branch.push((path.clone(), copy.on_branch));
            copies.push((path, copy.text));
            if copy.legacy {
                legacy.push(skill_file(dir, LEGACY_SKILL_NAME));
            }
        }
    }
    let (state, paths) = skill_plan(&copies, &legacy);
    let pull_request_current = found.pull_request.is_some()
        && on_branch
            .iter()
            .filter(|(p, _)| paths.contains(p))
            .all(|(_, text)| text.as_deref() == Some(SKILL));
    let view = SkillView {
        state,
        paths,
        legacy,
        default_branch: found.head.branch.clone(),
        pull_request: found.pull_request.map(PullRequestRef::from),
        pull_request_current,
    };
    Ok((view, found.head))
}

/// Whether the repo's copy of the `mergeloop` skill is the one MergeLoop ships now.
pub(crate) async fn repo_skill_view(
    State(app): State<AppState>,
    caller: Caller,
    Path((id, owner, name)): Path<(u64, String, String)>,
) -> Result<Json<SkillView>, ApiError> {
    let repo = repo_name(owner, name)?;
    let id = caller.repo(&app, id, &repo).await?;
    let gh = installation(&app, id).await?;
    Ok(Json(repo_skill(&gh, &repo).await?.0))
}

/// Opens a PR that writes the current skill over the repo's older copies, or updates the one
/// open.
pub(crate) async fn update_repo_skill(
    State(app): State<AppState>,
    caller: Caller,
    Path((id, owner, name)): Path<(u64, String, String)>,
) -> Result<Json<UpdatePullView>, ApiError> {
    caller.workspace(id)?;
    let repo = repo_name(owner, name)?;
    let id = caller.repo(&app, id, &repo).await?;
    let token = caller.github_token(&app)?;
    let gh = installation(&app, id).await?;
    let (skill, base) = repo_skill(&gh, &repo).await?;
    if skill.state == SkillStateView::Current {
        return Err(ApiError::Status(
            StatusCode::CONFLICT,
            format!("`{}` already has the current skill.", base.branch),
        ));
    }
    let pr = propose_skill(&gh, &repo, &skill, &base).await?;
    tracing::info!(installation = id, %repo, pr = pr.number, "proposed the current skill");
    let file = UpdatedFileView::Skill;
    Ok(Json(if repo_queues(&app, id, &repo).await? {
        queue_update(&app, &gh, &token, &repo, file, pr.into()).await
    } else {
        UpdatePullView::held(file, pr.into(), "This repo does not queue yet.")
    }))
}

/// Opens a PR that writes the current skill to the copies `skill` lists, and deletes its copies
/// under the old name, or updates the one open.
async fn propose_skill(
    gh: &Installation,
    repo: &RepoName,
    skill: &SkillView,
    base: &BranchFile,
) -> anyhow::Result<PullRef> {
    let files: Vec<(&str, &str)> = skill.paths.iter().map(|p| (p.as_str(), SKILL)).collect();
    let removed: Vec<&str> = skill.legacy.iter().map(String::as_str).collect();
    let listed = |paths: &[String]| {
        let list: Vec<String> = paths.iter().map(|p| format!("`{p}`")).collect();
        list.join(" and ")
    };
    let mut body = String::new();
    if !skill.paths.is_empty() {
        body.push_str(&format!(
            "This PR writes the current MergeLoop agent skill to {}, so coding agents in this repo follow how MergeLoop works now: they queue their PRs, then work every review comment to resolved. Once it merges into `{}`, agents read the new skill.\n\nIt replaces the copy that is there, so a change made to the skill in this repo is lost: carry it over in this PR if you want to keep it.",
            listed(&skill.paths),
            base.branch
        ));
    }
    if !skill.legacy.is_empty() {
        let it = match body.is_empty() {
            true => "This PR",
            false => "\n\nIt also",
        };
        body.push_str(&format!(
            "{it} deletes {}, the skill under its name from before Mergepatrol became MergeLoop, so agents read only the `mergeloop` skill.",
            listed(&skill.legacy)
        ));
    }
    gh.propose_file(
        repo,
        Proposal {
            base,
            files: &files,
            removed: &removed,
            branch: SKILL_BRANCH,
            title: "Update the MergeLoop skill",
            body: &body,
        },
    )
    .await
}

/// Which file of a repo an update's PR brings up to date.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum UpdatedFileView {
    Settings,
    Skill,
}

/// A PR that carries a repo's update.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct UpdatePullView {
    pub file: UpdatedFileView,
    pub pull_request: PullRequestRef,
    pub queued: bool,
    /// Why it is not queued.
    pub note: Option<String>,
}

impl UpdatePullView {
    /// Left out of the queue, and why.
    fn held(file: UpdatedFileView, pull_request: PullRequestRef, note: &str) -> Self {
        Self {
            file,
            pull_request,
            queued: false,
            note: Some(note.to_owned()),
        }
    }
}

const LOSES_COMMENTS: &str = "This drops the file's comments: carry them over, then queue this PR.";

/// What an update did to a repo. With no field set, the repo was up to date.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct RepoUpdateView {
    /// Why it left the repo alone.
    pub skipped: Option<String>,
    pub pulls: Vec<UpdatePullView>,
    /// Why the skill's half failed. `pulls` still has what the settings' half did.
    pub failed: Option<String>,
}

impl RepoUpdateView {
    /// The settings' PR, if any, with how the skill's half went.
    fn finished(
        mut pulls: Vec<UpdatePullView>,
        skill: Result<Option<UpdatePullView>, ApiError>,
    ) -> Self {
        let failed = match skill {
            Ok(pull) => {
                pulls.extend(pull);
                None
            }
            Err(e) => Some(reason(e, "MergeLoop could not update the skill.")),
        };
        Self {
            skipped: None,
            pulls,
            failed,
        }
    }
}

/// An error, for a user reading an outcome rather than a failed request.
fn reason(e: ApiError, fallback: &str) -> String {
    match e {
        ApiError::Status(_, why) | ApiError::GitHubLimited(_, why) => why,
        ApiError::Internal(e) => {
            tracing::error!(error = %format!("{e:#}"), "{fallback}");
            fallback.to_owned()
        }
    }
}

/// What an update does about a repo's config file.
#[derive(Debug, PartialEq, Eq)]
enum ConfigUpdate {
    /// The default branch has it already.
    Current,
    /// Open the PR, or bring the open one up to date.
    Propose,
    /// The open PR has it already: only queueing it is left.
    Queue,
    /// The open PR changes the settings too: someone's change, not the update's to touch.
    Leave,
    /// Rewriting the open PR would lose comments: a push cannot take it out of the queue, so
    /// the update does not push.
    KeepComments,
}

/// `text` is the file on the default branch, which sets `settings`, and `open` the file on the
/// open PR's branch, if a PR is open.
fn config_update(
    settings: &RepoSettings,
    text: &str,
    open: Option<Option<&str>>,
    defaults: &RepoSettings,
) -> ConfigUpdate {
    if settings.written_in(text, defaults) {
        return ConfigUpdate::Current;
    }
    let Some(open) = open else {
        return ConfigUpdate::Propose;
    };
    match RepoSettings::from_file(open, defaults) {
        Ok(theirs) if theirs != *settings => ConfigUpdate::Leave,
        Err(_) => ConfigUpdate::Leave,
        Ok(_) if open.is_some_and(|t| settings.written_in(t, defaults)) => ConfigUpdate::Queue,
        Ok(_)
            if settings.loses_comments_of(text, defaults)
                || open.is_some_and(|t| settings.loses_comments_of(t, defaults)) =>
        {
            ConfigUpdate::KeepComments
        }
        Ok(_) => ConfigUpdate::Propose,
    }
}

/// Brings a queuing repo's config file and skill up to date: opens the PRs it needs, or updates
/// the ones open, and queues them. A repo that is up to date gets none, and running it again
/// pushes nothing to a PR that has the update.
pub(crate) async fn update_repo(
    State(app): State<AppState>,
    caller: Caller,
    Path((id, owner, name)): Path<(u64, String, String)>,
) -> Result<Json<RepoUpdateView>, ApiError> {
    caller.workspace(id)?;
    let repo = repo_name(owner, name)?;
    let id = caller.repo(&app, id, &repo).await?;
    let token = caller.github_token(&app)?;
    let skip = |why: String| {
        Ok(Json(RepoUpdateView {
            skipped: Some(why),
            pulls: Vec::new(),
            failed: None,
        }))
    };
    let state = app.store.repo_state(id, &repo).await?;
    if !state.is_some_and(|s| s.on()) {
        return skip("MergeLoop is off on this repo.".into());
    }
    let gh = installation(&app, id).await?;
    let ConfigFile { at, file } = gh.config_file(&repo).await?;
    let Some(text) = file.text.as_deref() else {
        return skip(format!("{} has no config file.", file.branch));
    };
    let defaults = &at.inherit(&inherited(&app, id).await?);
    let settings = match RepoSettings::from_file_at(at, Some(text), defaults) {
        Ok(settings) => settings,
        Err(e) => return skip(e),
    };

    let mut pulls = Vec::new();
    let open = gh.open_pr_from(&repo, CONFIG_BRANCH).await?;
    let on_branch = match open {
        Some(_) => Some(gh.branch_file(&repo, CONFIG_BRANCH, at.path()).await?),
        None => None,
    };
    let update = config_update(
        &settings,
        text,
        on_branch.as_ref().map(Option::as_deref),
        defaults,
    );
    let pr = match update {
        ConfigUpdate::Current => None,
        ConfigUpdate::Queue | ConfigUpdate::Leave | ConfigUpdate::KeepComments => open,
        ConfigUpdate::Propose => Some(
            propose_rewrite(&gh, &repo, &settings, defaults, at, &file, text)
                .await
                .map_err(github_failed)?,
        ),
    };
    if let Some(pr) = pr {
        let note = match update {
            ConfigUpdate::Leave => {
                Some("This PR changes the settings, so the update left it as it is.")
            }
            ConfigUpdate::KeepComments => {
                Some("Updating this PR would drop comments, so the update left it as it is.")
            }
            _ if settings.loses_comments_of(text, defaults) => Some(LOSES_COMMENTS),
            _ => None,
        };
        pulls.push(match note {
            Some(note) => UpdatePullView::held(UpdatedFileView::Settings, pr.into(), note),
            None => {
                queue_update(
                    &app,
                    &gh,
                    &token,
                    &repo,
                    UpdatedFileView::Settings,
                    pr.into(),
                )
                .await
            }
        });
    }

    let skill = update_skill(&app, &gh, &token, &repo).await;
    tracing::info!(installation = id, %repo, pulls = pulls.len(), "updated from the dashboard");
    Ok(Json(RepoUpdateView::finished(pulls, skill)))
}

/// Opens and queues the PR that brings the repo's skill up to date, if it is behind.
async fn update_skill(
    app: &AppState,
    gh: &Installation,
    token: &str,
    repo: &RepoName,
) -> Result<Option<UpdatePullView>, ApiError> {
    let (skill, base) = repo_skill(gh, repo).await?;
    if skill.state == SkillStateView::Current {
        return Ok(None);
    }
    let pr = match skill.pull_request {
        Some(pr) if skill.pull_request_current => pr,
        _ => propose_skill(gh, repo, &skill, &base)
            .await
            .map_err(github_failed)?
            .into(),
    };
    let file = UpdatedFileView::Skill;
    Ok(Some(queue_update(app, gh, token, repo, file, pr).await))
}

/// GitHub's answer for the user: an update runs across repos, and each can fail its own way.
fn github_failed(e: anyhow::Error) -> ApiError {
    match ApiError::from(e) {
        ApiError::Internal(e) => ApiError::Status(StatusCode::BAD_GATEWAY, format!("{e:#}")),
        limited => limited,
    }
}

/// Opens a PR that rewrites the config file the way MergeLoop writes it now, or updates the one
/// open.
async fn propose_rewrite(
    gh: &Installation,
    repo: &RepoName,
    settings: &RepoSettings,
    defaults: &RepoSettings,
    at: ConfigPath,
    base: &BranchFile,
    text: &str,
) -> anyhow::Result<PullRef> {
    let path = at.path();
    let content = settings.to_yaml(defaults);
    let comments = if settings.loses_comments_of(text, defaults) {
        "\n\nIt replaces the file, so the file's comments are lost: carry them over in this PR if you want to keep them."
    } else {
        ""
    };
    let body = format!(
        "This PR writes `{path}` the way MergeLoop writes it now: it renames old settings and keeps only those that differ from the workspace, so the rest follow it from now on. The queue runs as it did, with these settings.\n\n{}{comments}",
        settings_list(settings)
    );
    gh.propose_file(
        repo,
        Proposal {
            base,
            files: &[(path, content.as_str())],
            removed: &[],
            branch: CONFIG_BRANCH,
            title: "Update the MergeLoop settings",
            body: &body,
        },
    )
    .await
}

/// Queues an update's PR as the caller. A PR GitHub won't let them queue is still open, so the
/// refusal goes with it rather than failing the update.
async fn queue_update(
    app: &AppState,
    gh: &Installation,
    token: &str,
    repo: &RepoName,
    file: UpdatedFileView,
    pull_request: PullRequestRef,
) -> UpdatePullView {
    let queued = queue(app, gh, token, repo, pull_request.number.into(), false).await;
    UpdatePullView {
        file,
        pull_request,
        queued: queued.is_ok(),
        note: queued
            .err()
            .map(|e| reason(e, "MergeLoop could not queue this PR.")),
    }
}

/// The skill's path in each skills dir, but one that links to the other: a commit cannot write
/// through a symlink, and the link already shows the skill there.
async fn skill_paths(gh: &Installation, repo: &RepoName) -> Result<Vec<String>, ApiError> {
    let mut paths = Vec::new();
    for dir in SKILL_DIRS {
        // A symlink is a blob, so it has text. A dir has none.
        if gh.default_branch_file(repo, dir).await?.text.is_none() {
            paths.push(skill_file(dir, SKILL_NAME));
        }
    }
    Ok(paths)
}

/// The settings as a PR's body lists them.
fn settings_list(settings: &RepoSettings) -> String {
    let verify = if settings.verify.is_empty() {
        "none: the agent uses the checks this repo's CI runs".to_owned()
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
    let r = &settings.agent;
    let overrides: String = [
        ("Rebase conflicts", r.overrides.rebase.as_ref()),
        ("Fixes", r.overrides.fix.as_ref()),
        ("Reviews", r.overrides.review.as_ref()),
    ]
    .into_iter()
    .filter_map(|(task, t)| {
        Some(format!(
            "\n  - {task}: {} `{}`",
            t?.provider.name(),
            t?.model
        ))
    })
    .collect();
    format!(
        "- Trigger label: `{}`\n- Batch size: {}\n- Default model: {} `{}`, effort `{}`{overrides}\n- Verify commands: {verify}\n- Fix failed checks: {fix_ci}\n- Queues: {branches}",
        settings.label,
        settings.batch_size,
        r.provider.name(),
        r.model,
        r.effort
    )
}

fn config_pr(
    settings: &RepoSettings,
    at: ConfigPath,
    update: bool,
    branch: &str,
    skills: &[String],
) -> (&'static str, String) {
    let path = at.path();
    let list = settings_list(settings);
    let skills = if skills.is_empty() {
        String::new()
    } else {
        let paths: Vec<String> = skills.iter().map(|p| format!("`{p}`")).collect();
        format!(
            "\n\nIt also adds the MergeLoop agent skill, at {}, so coding agents in this repo queue their PRs instead of merging them.",
            paths.join(" and ")
        )
    };
    if update {
        (
            "Update the MergeLoop settings",
            format!(
                "This PR changes `{path}`, as edited in the MergeLoop dashboard. The queue uses the new settings once this PR merges into `{branch}`.\n\n{list}"
            ),
        )
    } else {
        (
            "Set up MergeLoop",
            format!(
                "This PR adds `{path}` with the settings below. Once it merges into `{branch}`, MergeLoop runs a merge queue on this repo: add the `{}` label to a PR to queue it.\n\n{list}{skills}\n\nTo change the settings later, edit the file, or use the MergeLoop dashboard, which opens a PR like this one.",
                settings.label
            ),
        )
    }
}

pub(crate) async fn settings(
    State(app): State<AppState>,
    caller: Caller,
    Path(id): Path<u64>,
) -> Result<Json<SettingsView>, ApiError> {
    settings_view(&app, caller.workspace(id)?).await
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
    caller: Caller,
    Path(id): Path<u64>,
) -> Result<Json<SettingsView>, ApiError> {
    let id = caller.workspace(id)?;
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
    caller: Caller,
    Path(id): Path<u64>,
) -> Result<Json<SettingsView>, ApiError> {
    let id = caller.workspace(id)?;
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
    caller: Caller,
    Path(id): Path<u64>,
) -> Result<Json<SettingsView>, ApiError> {
    let id = caller.workspace(id)?;
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
    caller: Caller,
    Path((id, provider_id)): Path<(u64, String)>,
    Json(body): Json<SetSecret>,
) -> Result<Json<SettingsView>, ApiError> {
    let id = caller.workspace(id)?;
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
    caller: Caller,
    Path((id, provider_id)): Path<(u64, String)>,
) -> Result<Json<SettingsView>, ApiError> {
    let id = caller.workspace(id)?;
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
    use mergepatrol_core::{Hold, PrSummary};

    #[test]
    fn queueing_says_what_will_hold_or_stop_the_pr() {
        let status = |hold: Option<Hold>, missing: Option<&str>| RepoStatusView {
            state: RepoStateView::On,
            installed: true,
            has_config: true,
            forced: None,
            config_error: None,
            hold: hold.as_ref().map(crate::v1::HoldView::from),
            missing_key: missing.map(|m| crate::v1::MissingKeyView {
                tasks: Vec::new(),
                message: m.into(),
                settings_url: String::new(),
            }),
        };
        assert_eq!(QueuedView::new(status(None, None)).message, "Queued.");
        assert_eq!(
            QueuedView::new(status(Some(Hold::NoSubscription), None)).message,
            "Queued, but it goes on hold: private repos need a MergeLoop subscription."
        );
        assert_eq!(
            QueuedView::new(status(None, Some("the workspace has no key"))).message,
            "Queued. Also, the workspace has no key."
        );
    }

    /// What the settings form sends: each task's model reaches the file, and a caller from
    /// before overrides still writes a file whose tasks run the default model.
    #[test]
    fn the_form_s_task_models_reach_the_file() {
        let view = |json: &str| -> RepoSettings {
            serde_json::from_str::<RepoSettingsView>(json)
                .unwrap()
                .into()
        };
        let old = FORM.replace(FORM_OVERRIDES, "");
        assert!(!old.contains("overrides"));
        let defaults = view(&old);

        let read =
            RepoSettings::from_file(Some(&view(FORM).to_yaml(&defaults)), &defaults).unwrap();
        let r = &read.agent;
        assert_eq!(r.model(SpendTask::Rebase), (Provider::OpenAi, "gpt-6-luna"));
        assert_eq!(
            r.model(SpendTask::Fix),
            (Provider::Anthropic, "claude-sonnet-5-5")
        );
        assert_eq!(
            r.model(SpendTask::Review),
            (Provider::Anthropic, "claude-fable-5-1")
        );

        let yaml = view(&old).to_yaml(&defaults);
        assert!(!yaml.contains("overrides"), "{yaml}");
        let read = RepoSettings::from_file(Some(&yaml), &defaults).unwrap();
        assert_eq!(
            read.agent.model(SpendTask::Review),
            (Provider::Anthropic, "claude-sonnet-5-5")
        );
    }

    /// The settings form's PUT body, as the browser sent it.
    const FORM: &str = r#"{"label":"mergeloop","batch_size":8,"provider":"anthropic","model":"claude-sonnet-5-5","effort":"medium","overrides":{"rebase":{"provider":"openai","model":"gpt-6-luna"},"fix":null,"review":{"provider":"anthropic","model":"claude-fable-5-1"}},"verify":["cargo check"],"fix_ci":true,"fix_ci_attempts":2,"branches":["release"],"review":{"enabled":null,"bars":[]}}"#;
    const FORM_OVERRIDES: &str = r#""overrides":{"rebase":{"provider":"openai","model":"gpt-6-luna"},"fix":null,"review":{"provider":"anthropic","model":"claude-fable-5-1"}},"#;

    /// An open PR by `@ada` that GitHub won't merge before someone approves it.
    const ADAS_AWAITING_APPROVAL: &str = r#"{"data":{"repository":{
        "pullRequest":{"number":7,"state":"OPEN","title":"t","body":"","isDraft":false,"isCrossRepository":false,
            "reviewDecision":"REVIEW_REQUIRED","baseRefName":"main","headRefName":"t","headRefOid":"abc",
            "labels":{"nodes":[]},"files":{"nodes":[]},
            "url":"","createdAt":"","additions":1,"deletions":0,"changedFiles":1,
            "author":{"__typename":"User","login":"Ada","avatarUrl":"a"},
            "latestOpinionatedReviews":{"nodes":[]},"reviewRequests":{"nodes":[]},"commits":{"nodes":[]}},
        "defaultBranchRef":{"name":"main","rules":{"nodes":[]}},
        "codeowners0":null,"codeowners1":null,"codeowners2":null}}}"#;

    /// GitHub takes no approval from a PR's author, even where the repo needs one to merge.
    /// The author's approval posts none, and is no dead end: it still queues the PR, which
    /// waits there for a reviewer.
    #[tokio::test]
    async fn an_author_queues_their_own_pr_though_it_awaits_an_approval() {
        let repo = RepoName::parse("o/r").unwrap();
        for (login, verdict) in [("ada", None), ("bo", Some(setup::Verdict::Approve))] {
            let (gh, _) =
                mergepatrol_github::fake::serve(vec![(200, "", ADAS_AWAITING_APPROVAL)]).await;
            let Ok(got) = approval(&gh, &repo, 7, login).await else {
                panic!("{login}'s approval was refused");
            };
            assert_eq!(got, verdict, "{login}");
        }
    }

    fn settings() -> RepoSettings {
        RepoSettings::from(serde_json::from_str::<RepoSettingsView>(FORM).unwrap())
    }

    /// A file that says nothing over `s`, and one that spells out a setting it already follows.
    fn files(s: &RepoSettings) -> (String, String) {
        let current = s.to_yaml(s);
        let old = format!("{current}label: {}\n", s.label);
        (current, old)
    }

    #[test]
    fn an_update_opens_a_pr_only_for_a_file_out_of_date() {
        let s = settings();
        let (current, old) = files(&s);
        assert_eq!(config_update(&s, &current, None, &s), ConfigUpdate::Current);
        assert_eq!(config_update(&s, &old, None, &s), ConfigUpdate::Propose);
    }

    #[test]
    fn an_update_keeps_off_an_open_pr_that_changes_the_settings() {
        let s = settings();
        let mut theirs = s.clone();
        theirs.batch_size += 1;
        let theirs = theirs.to_yaml(&s);
        let (current, old) = files(&s);
        assert_eq!(
            config_update(&s, &old, Some(Some(&theirs)), &s),
            ConfigUpdate::Leave
        );
        assert_eq!(
            config_update(&s, &old, Some(Some("not: [settings")), &s),
            ConfigUpdate::Leave
        );
        // Nothing to update, so nothing to say about the PR either.
        assert_eq!(
            config_update(&s, &current, Some(Some(&theirs)), &s),
            ConfigUpdate::Current
        );
    }

    #[test]
    fn an_update_keeps_off_an_open_pr_it_would_lose_comments_of() {
        let s = settings();
        let (current, old) = files(&s);
        let on_the_pr = format!("# Why.\n{old}");
        assert_eq!(
            config_update(&s, &old, Some(Some(&on_the_pr)), &s),
            ConfigUpdate::KeepComments
        );
        let on_the_branch = format!("{old}# Why.\n");
        assert_eq!(
            config_update(&s, &on_the_branch, Some(Some(&old)), &s),
            ConfigUpdate::KeepComments
        );
        // Without a PR there is nothing queued to protect: the new one opens, unqueued.
        assert_eq!(
            config_update(&s, &on_the_branch, None, &s),
            ConfigUpdate::Propose
        );
        // A rewrite the open PR already has pushes nothing.
        assert_eq!(
            config_update(&s, &on_the_branch, Some(Some(&current)), &s),
            ConfigUpdate::Queue
        );
    }

    #[test]
    fn a_repair_pr_for_a_file_that_does_not_parse_can_be_updated_again() {
        let s = settings();
        let (current, _) = files(&s);
        assert!(dequeues_first(true, Some(&current), &s));
        assert!(!dequeues_first(false, Some(&current), &s));
        // Each submission reaches the push: nothing reads labels from the broken file.
        for _ in 0..2 {
            assert!(!dequeues_first(true, Some("# Why.\nnot: [settings"), &s));
        }
    }

    #[test]
    fn a_skill_failure_keeps_what_the_settings_pr_did() {
        let pull = UpdatePullView {
            file: UpdatedFileView::Settings,
            pull_request: PullRequestRef {
                number: 7,
                url: "u".into(),
            },
            queued: true,
            note: None,
        };
        let failed = Err(ApiError::Status(StatusCode::BAD_GATEWAY, "no".into()));
        let view = RepoUpdateView::finished(vec![pull], failed);
        assert_eq!(view.pulls.len(), 1);
        assert!(view.pulls[0].queued);
        assert_eq!(view.failed.as_deref(), Some("no"));
    }

    #[test]
    fn an_update_pushes_nothing_to_an_open_pr_that_has_it() {
        let s = settings();
        let (current, old) = files(&s);
        assert_eq!(
            config_update(&s, &old, Some(Some(&current)), &s),
            ConfigUpdate::Queue
        );
        // An older update's PR has the same settings in the old shape.
        assert_eq!(
            config_update(&s, &old, Some(Some(&old)), &s),
            ConfigUpdate::Propose
        );
    }

    #[test]
    fn a_skill_update_writes_each_copy_that_is_not_the_current_skill() {
        let (a, c) = (
            ".agents/skills/mergeloop/SKILL.md",
            ".claude/skills/mergeloop/SKILL.md",
        );
        let copy = |p: &str, t: Option<&str>| (p.to_owned(), t.map(str::to_owned));
        assert_eq!(
            skill_plan(&[copy(a, Some(SKILL)), copy(c, Some(SKILL))], &[]),
            (SkillStateView::Current, vec![])
        );
        assert_eq!(
            skill_plan(&[copy(a, Some("old")), copy(c, Some(SKILL))], &[]),
            (SkillStateView::Outdated, vec![a.to_owned()])
        );
        assert_eq!(
            skill_plan(&[copy(a, Some(SKILL)), copy(c, None)], &[]),
            (SkillStateView::Outdated, vec![c.to_owned()])
        );
        assert_eq!(
            skill_plan(&[copy(a, None), copy(c, None)], &[]),
            (SkillStateView::Missing, vec![a.to_owned(), c.to_owned()])
        );
        // Both dirs link elsewhere: nothing to write.
        assert_eq!(skill_plan(&[], &[]), (SkillStateView::Current, vec![]));

        // A copy under the old name is out of date, though it is the current skill.
        let legacy = [".agents/skills/mergepatrol/SKILL.md".to_owned()];
        assert_eq!(
            skill_plan(&[copy(a, None), copy(c, None)], &legacy),
            (SkillStateView::Outdated, vec![a.to_owned(), c.to_owned()])
        );
        assert_eq!(
            skill_plan(&[copy(a, Some(SKILL)), copy(c, Some(SKILL))], &legacy),
            (SkillStateView::Outdated, vec![])
        );
    }

    #[test]
    fn a_skills_dir_that_links_elsewhere_at_any_step_is_left_alone() {
        let dir = || Some(GitObject::Tree(Vec::new()));
        let link = || Some(GitObject::Blob("../.agents/skills".into()));
        let text = |t: &str| Some(GitObject::Blob(t.into()));
        let copy = |text: Option<&str>, on_branch: Option<&str>, legacy| {
            Some(SkillCopy {
                text: text.map(str::to_owned),
                on_branch: on_branch.map(str::to_owned),
                legacy,
            })
        };
        // A real copy, and the update's branch with the current skill.
        assert_eq!(
            skill_copy(&[dir(), dir(), dir(), text("old"), text(SKILL), None]),
            copy(Some("old"), Some(SKILL), false)
        );
        // No copy yet.
        assert_eq!(
            skill_copy(&[dir(), None, None, None, None, None]),
            copy(None, None, false)
        );
        // Only the copy under the old name.
        assert_eq!(
            skill_copy(&[dir(), dir(), None, None, None, text(SKILL)]),
            copy(None, None, true)
        );
        // A link at the top, at `skills`, at `mergeloop`, or the file itself.
        assert_eq!(skill_copy(&[link(), None, None, None, None, None]), None);
        assert_eq!(skill_copy(&[dir(), link(), None, None, None, None]), None);
        assert_eq!(skill_copy(&[dir(), dir(), link(), None, None, None]), None);
        let file_link = Some(GitObject::Tree(vec![("SKILL.md".into(), SYMLINK_MODE)]));
        assert_eq!(
            skill_copy(&[
                dir(),
                dir(),
                file_link,
                text("../../x/SKILL.md"),
                None,
                None
            ]),
            None
        );
        let lookups = skill_lookups(".claude/skills");
        assert_eq!(lookups[0].1, ".claude");
        assert_eq!(lookups[3].1, ".claude/skills/mergeloop/SKILL.md");
        assert_eq!(lookups[5].1, ".claude/skills/mergepatrol/SKILL.md");
    }

    #[test]
    fn the_open_comments_say_when_more_threads_exist_than_were_read() {
        use mergepatrol_github::ThreadSummary;
        let thread = |resolved| ThreadSummary {
            resolved,
            path: "a.rs".into(),
            line: Some(1),
            author: None,
            title: "t".into(),
            url: "u".into(),
            comments: 1,
        };
        let detail = |threads, thread_count| PullDetail {
            head: String::new(),
            base: String::new(),
            body_html: String::new(),
            labels: Vec::new(),
            checks: Vec::new(),
            timeline: Vec::new(),
            threads,
            thread_count,
        };
        let v = PullDetailView::from(detail(vec![thread(true), thread(false)], 120)).review_threads;
        assert_eq!((v.open, v.total, v.read), (1, 120, 2));
        // GitHub's count never says fewer than were read.
        let v = PullDetailView::from(detail(vec![thread(true)], 0)).review_threads;
        assert_eq!((v.open, v.total, v.read), (0, 1, 1));
    }

    /// This repo queues through MergeLoop too: its agents read the skill every repo gets.
    #[test]
    fn this_repo_has_the_skill_every_repo_gets() {
        assert_eq!(
            include_str!("../../../.agents/skills/mergeloop/SKILL.md"),
            SKILL
        );
        assert_eq!(
            include_str!("../../../.claude/skills/mergeloop/SKILL.md"),
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
            recheck: None,
        }
    }

    #[test]
    fn a_stack_shows_the_review_of_the_layer_it_waits_for() {
        let held = EntryState::Held(mergepatrol_core::Hold::Layer {
            number: 8,
            hold: Box::new(mergepatrol_core::Hold::ReviewAsks),
        });
        let stack = |state| Entry {
            layers: vec![7, 8, 9],
            ..entry(10, false, state)
        };
        assert_eq!(review_layer(&stack(held)), 8);
        assert_eq!(review_layer(&stack(EntryState::Waiting)), 9);
        assert_eq!(review_layer(&entry(5, false, EntryState::Waiting)), 5);
    }

    #[test]
    fn an_entry_has_its_pull_request_fields_at_the_top() {
        let view = EntryView::new(entry(1, true, EntryState::Waiting), None, None);
        let json = serde_json::to_value(view).unwrap();
        assert_eq!(json["number"], 1);
        assert_eq!(json["title"], "");
        assert!(json.get("pull").is_none());
    }

    #[test]
    fn unread_is_what_others_said_since_i_looked_or_last_spoke() {
        let a = |who: &str, at: &str| (who.to_owned(), at.to_owned());
        let activity = [a("ann", "1"), a("Me", "2"), a("bo", "3"), a("ann", "5")];
        assert_eq!(unread(&activity, "me", None), 2);
        assert_eq!(unread(&activity, "me", Some("4")), 1);
        assert_eq!(unread(&activity, "me", Some("1")), 2);
        assert_eq!(unread(&[a("ann", "1")], "me", None), 1);
        assert_eq!(unread(&[], "me", None), 0);
    }

    #[test]
    fn each_queued_pr_and_stack_layer_has_its_spot_in_merge_order() {
        let repo = RepoName::parse("o/r").unwrap();
        let mut stack = entry(9, false, EntryState::Waiting);
        stack.layers = vec![7, 8, 9];
        let lane = Lane {
            entries: vec![
                entry(
                    1,
                    false,
                    EntryState::Held(mergepatrol_core::Hold::ReviewRequired),
                ),
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
        assert_eq!(
            spot(1).detail.as_deref(),
            Some("waiting for a required review")
        );
        assert!(spots.values().all(|s| s.of == 4));
        assert!(!spots.contains_key(&(repo, 2)));
    }

    #[test]
    fn lanes_list_prs_in_the_order_they_merge() {
        let entries = vec![
            entry(1, false, EntryState::Held(mergepatrol_core::Hold::Draft)),
            entry(2, false, EntryState::Waiting),
            entry(3, true, EntryState::Waiting),
            entry(4, false, EntryState::Active),
            entry(5, false, EntryState::Waiting),
        ];
        let order: Vec<u64> = in_merge_order(entries).iter().map(|e| e.number).collect();
        assert_eq!(order, [4, 3, 2, 5, 1]);
    }
}
