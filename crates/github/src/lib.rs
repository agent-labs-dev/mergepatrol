//! GitHub, as a GitHub App: auth, the queue snapshot, the Git Data API, checks, and merges.

mod app;
mod checks;
mod codeowners;
mod files;
mod repo;
pub mod setup;

#[cfg(any(test, feature = "testing"))]
pub use app::fake;
pub use app::{App, AsUser, Installation, InstallationInfo, RateLimited, RepoInfo, revoke_token};
pub use checks::{CheckState, Checks, RequiredChecks};
pub use codeowners::CodeOwners;
pub use files::{BranchFile, Proposal, PullRef};
pub use repo::{
    BranchMerge, HeadRepository, MergeOutcome, PrDetails, PullRequest, RepoOwner, RepoSnapshot,
    SnapshotParts, StackRef, TreeEntry, queued,
};
pub use setup::{AppCredentials, GithubUser};
