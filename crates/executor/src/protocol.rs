//! What crosses the sandbox boundary. The orchestrator writes a `JobSpec` to the job's stdin.
//! The job writes `result.json`, and on success `result.bundle` (a review, `review.json`; an
//! answer, `answer.md`), to its output directory.

use mergepatrol_core::{Answered, CheckFailure, Finding, Provider};
use serde::{Deserialize, Serialize};

pub const RESULT_FILE: &str = "result.json";
pub const BUNDLE_FILE: &str = "result.bundle";
/// What a review job found, as a `ReviewOutput`.
pub const REVIEW_FILE: &str = "review.json";
/// A follow-up's answer, as text.
pub const ANSWER_FILE: &str = "answer.md";
/// Every file a job may leave in its output directory: all of them cross the sandbox boundary.
pub const OUTPUT_FILES: [&str; 4] = [BUNDLE_FILE, RESULT_FILE, REVIEW_FILE, ANSWER_FILE];
/// The largest bundle a job may return.
pub const MAX_BUNDLE_BYTES: u64 = 200 * 1024 * 1024;

/// No `Debug`: it holds a GitHub token and a token for the Anthropic proxy.
#[derive(Clone, Serialize, Deserialize)]
pub struct JobSpec {
    /// `owner/name`.
    pub repo: String,
    pub clone_url: String,
    /// Read-only, and scoped to this repo. Empty fetches without auth.
    pub token: String,
    /// The PR head's full ref: `refs/heads/<branch>`, or a fork PR's `refs/pull/<n>/head`.
    pub head_ref: String,
    pub head_sha: String,
    pub trunk: String,
    /// What the rebase goes onto: the trunk, or for a layer of a stack, the rebased layer below.
    pub trunk_sha: String,
    /// A stack layer's branch below this one, which holds `trunk_sha`. `None` for the bottom.
    #[serde(default)]
    pub base_ref: Option<String>,
    /// Where a stack layer's own commits start: the layer below, as it was. The rebase replays
    /// `upstream..head_sha`. `None` replays what is not on the trunk.
    #[serde(default)]
    pub upstream: Option<String>,
    pub pr_title: String,
    pub pr_body: String,
    /// The repo's verify commands, from its config file.
    pub verify: Vec<String>,
    pub resolver: ResolverSpec,
    #[serde(default)]
    pub task: Task,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Task {
    /// Rebase `head_sha` onto `trunk_sha`, and resolve the conflicts.
    #[default]
    Rebase,
    /// Commit a fix for `failures` on top of `head_sha`, which already sits on `trunk_sha`.
    FixChecks { failures: Vec<CheckFailure> },
    /// Review what the PR changes since it left `trunk_sha`, and write `REVIEW_FILE`. Changes
    /// nothing. `answered` are what humans said about an earlier head; `sent_back` what the
    /// agent sent back to the author on the last one, to check the fix of.
    Review {
        #[serde(default)]
        answered: Vec<Answered>,
        #[serde(default)]
        sent_back: Vec<Finding>,
    },
    /// Answer a question about a finding of the review, and write `ANSWER_FILE`. `sent_back`:
    /// the finding went back to the author, and this is their reply, which it may withdraw on.
    Question {
        finding: Box<Finding>,
        question: String,
        #[serde(default)]
        sent_back: bool,
    },
}

impl Task {
    /// Whether the job returns code: a review or a question returns only what it wrote.
    pub fn returns_code(&self) -> bool {
        matches!(self, Task::Rebase | Task::FixChecks { .. })
    }
}

/// What a review job found: its summary, and each finding with its code.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewOutput {
    pub summary: String,
    pub findings: Vec<Finding>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ResolverSpec {
    #[serde(default)]
    pub provider: Provider,
    pub model: String,
    pub effort: String,
    pub timeout_secs: u64,
    pub max_turns: u32,
    /// The provider's API, through the orchestrator's proxy. `None` when the workspace has no
    /// key for it: then a conflict fails the job. The alias reads an orchestrator's spec from
    /// before other providers, while a deploy rolls out the job image first.
    #[serde(alias = "anthropic")]
    pub api: Option<ApiAccess>,
}

/// The job calls the provider's API at `base_url` with `token`, which the proxy swaps for the
/// workspace's key. The key itself never reaches the job.
#[derive(Clone, Serialize, Deserialize)]
pub struct ApiAccess {
    /// Where the agent's CLI sends its requests, as it expects its base URL.
    pub base_url: String,
    /// Good until the job ends.
    pub token: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobResult {
    pub outcome: JobOutcome,
    pub events: Vec<JobEvent>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JobOutcome {
    /// The rebased or fixed branch is in `result.bundle`, as `refs/heads/result`.
    #[serde(alias = "rebased")]
    Done,
    Failed {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobEvent {
    pub kind: String,
    pub message: String,
}
