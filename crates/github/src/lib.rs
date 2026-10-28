//! GitHub, as a GitHub App: auth, the queue snapshot, the Git Data API, checks, and merges.

mod app;
mod checks;
mod repo;
pub mod setup;

pub use app::{App, Installation, InstallationInfo, RepoInfo};
pub use checks::{CheckState, Checks};
pub use repo::{MergeOutcome, PullRequest, RepoSnapshot, TreeEntry};
pub use setup::{AppCredentials, GithubUser};
