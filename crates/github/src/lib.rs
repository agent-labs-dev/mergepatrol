//! GitHub, as a GitHub App: auth, the queue snapshot, the Git Data API, checks, and merges.

mod app;
mod checks;
mod codeowners;
mod detail;
mod files;
mod repo;
pub mod setup;

#[cfg(any(test, feature = "testing"))]
pub use app::fake;
pub use app::{
    App, AsUser, HookDelivery, Installation, InstallationInfo, RateLimited, RepoInfo, revoke_token,
};
pub use checks::{CheckState, Checks, RequiredChecks};
pub use codeowners::CodeOwners;
pub use detail::{
    CheckRun, CheckRunState, Label, PullDetail, ThreadSummary, TimelineEvent, TimelineItem,
};
pub use files::{BranchFile, GitObject, ObjectsAt, Proposal, PullRef, SYMLINK_MODE};
pub use repo::{
    BranchMerge, HeadRepository, InboxPull, MergeOutcome, PrDetails, PullRequest, RepoOwner,
    RepoSnapshot, ReviewThread, SnapshotParts, StackRef, ThreadComment, TreeEntry, queued,
};
pub use setup::{AppCredentials, GithubUser};
