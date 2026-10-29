//! The queue: lanes, entries, the state machine of one job, and a repo's settings. Pure logic,
//! no I/O.

mod job;
mod lane;
mod repo_config;

pub use job::{EjectReason, Event, Job, Phase, Transition};
pub use lane::{Bisect, Candidate, Entry, EntryState, Lane, LaneChange, Person, PrSummary, Review};
pub use repo_config::{
    EFFORTS, LabelSpec, Labels, REPO_CONFIG_PATH, RepoSettings, ResolverSettings,
};

use serde::{Deserialize, Serialize};

pub type PrNumber = u64;

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
