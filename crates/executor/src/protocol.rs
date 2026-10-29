//! What crosses the sandbox boundary. The orchestrator writes a `JobSpec` to the job's stdin.
//! The job writes `result.json`, and on success `result.bundle`, to its output directory.

use mergepatrol_core::CheckFailure;
use serde::{Deserialize, Serialize};

pub const RESULT_FILE: &str = "result.json";
pub const BUNDLE_FILE: &str = "result.bundle";

/// No `Debug`: it holds a GitHub token and an API key.
#[derive(Clone, Serialize, Deserialize)]
pub struct JobSpec {
    /// `owner/name`.
    pub repo: String,
    pub clone_url: String,
    /// Read-only, and scoped to this repo. Empty fetches without auth.
    pub token: String,
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
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ResolverSpec {
    pub model: String,
    pub effort: String,
    pub timeout_secs: u64,
    pub max_turns: u32,
    /// The workspace's own key. Without it, a conflict fails the job.
    pub anthropic_api_key: Option<String>,
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
