//! The queue: lanes, entries, the state machine of one job, and a repo's settings. Pure logic,
//! no I/O.

mod hold;
mod job;
mod lane;
mod repo_config;
mod review;
mod spend;
mod steps;

pub use hold::Hold;
pub use job::{CHECKS_TIMEOUT, EjectReason, Event, Job, Phase, Transition};
pub use lane::{
    BatchStep, Bisect, BuiltBatch, Candidate, Entry, EntryState, Lane, LaneChange, LayerHead,
    PendingMerge, Person, PrSummary, Review,
};
pub use repo_config::{
    EFFORTS, FixCiSettings, LabelSpec, Labels, REPO_CONFIG_PATH, RepoSettings, ResolverSettings,
};
pub use review::{
    Answer, Answered, Bar, CodeLine, Finding, Judged, Kind, Lean, LineKind, Question, Related,
    ReviewOverrides, ReviewSettings, ReviewStatus, review_url,
};
pub use spend::{Spend, SpendTask};
pub use steps::{Step, StepTimes, step_times};

use serde::{Deserialize, Serialize};

pub type PrNumber = u64;

/// The trailer on every commit the fix agent makes, before the agent's name.
pub const FIXED_BY: &str = "Fixed-by: mergepatrol";

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
