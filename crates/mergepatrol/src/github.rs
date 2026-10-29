//! What the queue needs from GitHub, apart from the rebase job. `Installation` in production; a
//! fake in the queue's tests.

use anyhow::Result;
use async_trait::async_trait;
use mergepatrol_core::{PrNumber, RepoName};
use mergepatrol_github::{Checks, Installation, MergeOutcome, PullRequest, RepoSnapshot};

#[async_trait]
pub trait GitHub: Send + Sync {
    fn installation_id(&self) -> u64;
    async fn snapshot(&self, repo: &RepoName, label: &str) -> Result<RepoSnapshot>;
    async fn pull_request(&self, repo: &RepoName, number: PrNumber) -> Result<PullRequest>;
    async fn branch_sha(&self, repo: &RepoName, branch: &str) -> Result<String>;
    async fn required_checks(&self, repo: &RepoName, branch: &str) -> Result<Vec<String>>;
    async fn checks(&self, repo: &RepoName, sha: &str) -> Result<Checks>;
    async fn merge(
        &self,
        repo: &RepoName,
        number: PrNumber,
        sha: &str,
        method: &str,
    ) -> Result<MergeOutcome>;
    async fn add_label(&self, repo: &RepoName, number: PrNumber, label: &str) -> Result<()>;
    async fn remove_label(&self, repo: &RepoName, number: PrNumber, label: &str) -> Result<()>;
    async fn sticky_comment(&self, repo: &RepoName, number: PrNumber, body: &str) -> Result<()>;
}

#[async_trait]
impl GitHub for Installation {
    fn installation_id(&self) -> u64 {
        self.id
    }
    async fn snapshot(&self, repo: &RepoName, label: &str) -> Result<RepoSnapshot> {
        Installation::snapshot(self, repo, label).await
    }
    async fn pull_request(&self, repo: &RepoName, number: PrNumber) -> Result<PullRequest> {
        Installation::pull_request(self, repo, number).await
    }
    async fn branch_sha(&self, repo: &RepoName, branch: &str) -> Result<String> {
        Installation::branch_sha(self, repo, branch).await
    }
    async fn required_checks(&self, repo: &RepoName, branch: &str) -> Result<Vec<String>> {
        Installation::required_checks(self, repo, branch).await
    }
    async fn checks(&self, repo: &RepoName, sha: &str) -> Result<Checks> {
        Installation::checks(self, repo, sha).await
    }
    async fn merge(
        &self,
        repo: &RepoName,
        number: PrNumber,
        sha: &str,
        method: &str,
    ) -> Result<MergeOutcome> {
        Installation::merge(self, repo, number, sha, method).await
    }
    async fn add_label(&self, repo: &RepoName, number: PrNumber, label: &str) -> Result<()> {
        Installation::add_label(self, repo, number, label).await
    }
    async fn remove_label(&self, repo: &RepoName, number: PrNumber, label: &str) -> Result<()> {
        Installation::remove_label(self, repo, number, label).await
    }
    async fn sticky_comment(&self, repo: &RepoName, number: PrNumber, body: &str) -> Result<()> {
        Installation::sticky_comment(self, repo, number, body).await
    }
}
