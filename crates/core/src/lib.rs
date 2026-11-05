//! The queue: lanes, entries, the state machine of one job, and a repo's settings. Pure logic,
//! no I/O.

mod ci;
mod closing;
mod hold;
mod job;
mod lane;
mod repo_config;
mod review;
mod spend;
mod steps;

pub use ci::verify_from_ci;
pub use closing::body_closing_commit_issues;
pub use hold::Hold;
pub use job::{CHECKS_MISSING_FOR, CHECKS_TIMEOUT, EjectReason, Event, Job, Phase, Transition};
pub use lane::{
    BatchStep, Bisect, BuiltBatch, Candidate, Entry, EntryState, Lane, LaneChange, LayerHead,
    PendingMerge, Person, PrSummary, Review, Stage,
};
pub use repo_config::{
    AgentSettings, EFFORTS, FixCiSettings, LabelSpec, Labels, ModelOverrides, REPO_CONFIG_PATH,
    RepoSettings, Runs, TaskModel, WorkspaceDefaults,
};
pub use review::{
    Answer, Answered, Bar, CodeLine, Evidence, Finding, Judged, Kind, Lean, LineKind, Question,
    Related, ReviewOverrides, ReviewSettings, ReviewStatus, Route, SentBack, review_status,
    review_url,
};
pub use spend::{Spend, SpendTask};
pub use steps::{Step, StepTimes, current_step, step_times};

use serde::{Deserialize, Serialize};

pub type PrNumber = u64;

/// The trailer on every commit the fix agent makes, before the agent's name.
pub const FIXED_BY: &str = "Fixed-by: mergepatrol";
/// The trailer of a commit whose conflict the agent resolved in a rebase.
pub const RESOLVED_BY: &str = "Resolved-by: mergepatrol";
/// The empty commit that runs a PR's checks again.
pub const RECHECK_MESSAGE: &str =
    "Run the checks again\n\nThe agent found nothing to fix in the code.";

/// How many of a PR's fix budget its commits have used, from `messages`, newest first: the
/// agent's fixes on top of the PR. The author's next commit ends the count, so new code earns new
/// fixes, and the commits themselves survive a restart or a rebase.
pub fn fixes_used<'a>(messages: impl IntoIterator<Item = &'a str>) -> u32 {
    let mut used = 0;
    for message in messages {
        if message.lines().any(|l| l.starts_with(FIXED_BY)) {
            used += 1;
        // A rebase's rewrite adds trailers after it.
        } else if !message.starts_with(RECHECK_MESSAGE) {
            break;
        }
    }
    used
}

/// Who runs the agent's model, with the workspace's own key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[default]
    Anthropic,
    OpenAi,
    /// Any model OpenRouter serves, through its Anthropic or its OpenAI API.
    OpenRouter,
}

impl Provider {
    /// The providers a repo can choose.
    pub const SUPPORTED: [Provider; 2] = [Provider::Anthropic, Provider::OpenAi];

    pub fn parse(s: &str) -> Option<Provider> {
        [Provider::Anthropic, Provider::OpenAi, Provider::OpenRouter]
            .into_iter()
            .find(|p| p.as_str() == s)
    }

    /// As in the config file.
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Anthropic => "anthropic",
            Provider::OpenAi => "openai",
            Provider::OpenRouter => "openrouter",
        }
    }

    /// As people write it.
    pub fn name(self) -> &'static str {
        match self {
            Provider::Anthropic => "Anthropic",
            Provider::OpenAi => "OpenAI",
            Provider::OpenRouter => "OpenRouter",
        }
    }

    /// What the agent runs the provider's models with, which a workspace adds in its settings.
    pub fn credential(self) -> &'static str {
        match self {
            Provider::Anthropic => "Anthropic API key",
            Provider::OpenAi => "OpenAI API key or ChatGPT login",
            Provider::OpenRouter => "OpenRouter API key",
        }
    }

    /// The models the settings offer, the one to start from first. The file may name any other.
    pub fn models(self) -> &'static [Model] {
        const ANTHROPIC: &[Model] = &[
            Model::new("claude-opus-5-5", "Claude Opus 5.5"),
            Model::new("claude-fable-5-1", "Claude Fable 5.1"),
            Model::new("claude-sonnet-5-5", "Claude Sonnet 5.5"),
            Model::new("claude-haiku-5-5", "Claude Haiku 5.5"),
        ];
        const OPENAI: &[Model] = &[
            Model::new("gpt-6.1-sol", "GPT-6.1 Sol"),
            Model::new("gpt-6-astra", "GPT-6 Astra"),
            Model::new("gpt-6.1-terra", "GPT-6.1 Terra"),
            Model::new("gpt-6-luna", "GPT-6 Luna"),
        ];
        match self {
            Provider::Anthropic => ANTHROPIC,
            Provider::OpenAi => OPENAI,
            // It serves too many to list: its file names one.
            Provider::OpenRouter => &[],
        }
    }
}

/// A model to pick: its id, as the provider's API takes it, and its name, as people write it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Model {
    pub id: &'static str,
    pub name: &'static str,
}

impl Model {
    const fn new(id: &'static str, name: &'static str) -> Self {
        Self { id, name }
    }
}

/// A required check that failed, with what GitHub shows of why: the check's output, its
/// annotations, and the end of its log. The fix agent reads it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckFailure {
    pub name: String,
    pub details: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RepoName {
    pub owner: String,
    pub name: String,
}

impl RepoName {
    /// Parses `owner/name`.
    pub fn parse(s: &str) -> Option<Self> {
        let (owner, name) = s.split_once('/')?;
        if owner.is_empty() || name.is_empty() || name.contains('/') {
            return None;
        }
        Some(Self {
            owner: owner.into(),
            name: name.into(),
        })
    }
}

impl std::fmt::Display for RepoName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

/// Whether a PR has been told to merge, the only state in which the queue acts on it: it is
/// open, it or a layer of its stack carries the repo's trigger label, and its lane holds a place
/// for it. Autonomous work never starts on a PR being opened, pushed to, labeled otherwise,
/// reviewed or checked.
pub fn merge_requested(open: bool, labeled: bool, in_lane: bool) -> bool {
    open && labeled && in_lane
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_budget_counts_the_agents_fixes_on_top_of_the_authors_last_commit() {
        let fix = "Fix ci\n\nFixed-by: mergepatrol, claude";
        assert_eq!(fixes_used([fix, RECHECK_MESSAGE, fix, "the author's"]), 2);
        assert_eq!(fixes_used(["the author's", fix, fix]), 0);
        let rewritten = format!("{RECHECK_MESSAGE}\n\nCo-authored-by: a <a@example.com>");
        assert_eq!(fixes_used([rewritten.as_str(), fix]), 1);
        assert_eq!(fixes_used([]), 0);
    }

    #[test]
    fn a_merge_request_is_state_three_things_all_holding() {
        assert!(merge_requested(true, true, true));
        // Not told to merge: no label, or no place in the lane.
        assert!(!merge_requested(true, false, true));
        assert!(!merge_requested(true, true, false));
        // Closed, or merged: never.
        assert!(!merge_requested(false, true, true));
        assert!(!merge_requested(false, false, false));
    }
}
