//! What crosses the sandbox boundary. The orchestrator writes a `JobSpec` to the job's stdin.
//! The job writes `result.json`, and on success `result.bundle`, to its output directory.

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
    pub trunk_sha: String,
    pub pr_title: String,
    pub pr_body: String,
    /// The repo's verify commands, from its config file.
    pub verify: Vec<String>,
    pub resolver: ResolverSpec,
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
    /// The rebased branch is in `result.bundle`, as `refs/heads/result`.
    Rebased,
    Failed {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobEvent {
    pub kind: String,
    pub message: String,
}
