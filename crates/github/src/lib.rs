//! GitHub, as a GitHub App: auth, the queue snapshot, the Git Data API, checks, and merges.

mod app;
mod checks;
mod codeowners;
mod files;
mod repo;
pub mod setup;

pub use app::{App, Installation, InstallationInfo, RepoInfo, revoke_token};
pub use checks::{CheckState, Checks, RequiredChecks};
pub use codeowners::CodeOwners;
pub use files::{BranchFile, Proposal, PullRef};
pub use repo::{
    BranchMerge, FixPatch, HeadRepository, MergeOutcome, PrDetails, PullRequest, RepoOwner,
    RepoSnapshot, StackRef, TreeEntry,
};
pub use setup::{AppCredentials, GithubUser};
