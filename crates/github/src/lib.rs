//! GitHub, as a GitHub App: auth, the queue snapshot, the Git Data API, checks, and merges.

mod app;
mod checks;
mod files;
mod repo;
pub mod setup;

pub use app::{App, Installation, InstallationInfo, RepoInfo, revoke_token};
pub use checks::{CheckState, Checks, RequiredChecks};
pub use files::{BranchFile, Proposal, PullRef};
pub use repo::{
    BranchMerge, MergeOutcome, PrDetails, PullRequest, RepoSnapshot, StackRef, TreeEntry,
};
pub use setup::{AppCredentials, GithubUser};
