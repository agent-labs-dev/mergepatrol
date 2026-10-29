//! What the queue needs from GitHub, apart from the rebase job. `Installation` in production; a
//! fake in the queue's tests.

use anyhow::Result;
use async_trait::async_trait;
use mergepatrol_core::{CheckFailure, LabelSpec, Labels, PrNumber, REPO_CONFIG_PATH, RepoName};
use mergepatrol_github::{
    BranchMerge, Checks, Installation, MergeOutcome, PullRequest, RepoSnapshot, RequiredChecks,
};

#[async_trait]
pub trait GitHub: Send + Sync {
    fn installation_id(&self) -> u64;
    /// The repo's config file on its default branch.
    async fn config_file(&self, repo: &RepoName) -> Result<Option<String>>;
    async fn snapshot(&self, repo: &RepoName, labels: &Labels) -> Result<RepoSnapshot>;
    async fn pull_request(&self, repo: &RepoName, number: PrNumber) -> Result<PullRequest>;
    async fn branch_sha(&self, repo: &RepoName, branch: &str) -> Result<String>;
    async fn required_checks(&self, repo: &RepoName, branch: &str) -> Result<RequiredChecks>;
    async fn checks(&self, repo: &RepoName, sha: &str) -> Result<Checks>;
    /// What GitHub shows of why the checks `names` failed on `sha`.
    async fn check_failures(
        &self,
        repo: &RepoName,
        sha: &str,
        names: &[String],
    ) -> Result<Vec<CheckFailure>>;
    async fn merge(
        &self,
        repo: &RepoName,
        number: PrNumber,
        sha: &str,
        method: &str,
    ) -> Result<MergeOutcome>;
    async fn create_branch(&self, repo: &RepoName, branch: &str, sha: &str) -> Result<()>;
    async fn delete_branch(&self, repo: &RepoName, branch: &str) -> Result<()>;
    async fn merge_into_branch(
        &self,
        repo: &RepoName,
        branch: &str,
        head: &str,
        message: &str,
    ) -> Result<Option<BranchMerge>>;
    async fn open_draft_pr(
        &self,
        repo: &RepoName,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<PrNumber>;
    async fn commit_tree(&self, repo: &RepoName, sha: &str) -> Result<String>;
    async fn create_label(&self, repo: &RepoName, label: &LabelSpec<'_>) -> Result<()>;
    async fn add_label(&self, repo: &RepoName, number: PrNumber, label: &str) -> Result<()>;
    async fn remove_label(&self, repo: &RepoName, number: PrNumber, label: &str) -> Result<()>;
    async fn sticky_comment(&self, repo: &RepoName, number: PrNumber, body: &str) -> Result<()>;
}

#[async_trait]
impl GitHub for Installation {
    fn installation_id(&self) -> u64 {
        self.id
    }
    async fn config_file(&self, repo: &RepoName) -> Result<Option<String>> {
        Ok(self.default_branch_file(repo, REPO_CONFIG_PATH).await?.text)
    }
    async fn snapshot(&self, repo: &RepoName, labels: &Labels) -> Result<RepoSnapshot> {
        Installation::snapshot(self, repo, labels).await
    }
    async fn pull_request(&self, repo: &RepoName, number: PrNumber) -> Result<PullRequest> {
        Installation::pull_request(self, repo, number).await
    }
    async fn branch_sha(&self, repo: &RepoName, branch: &str) -> Result<String> {
        Installation::branch_sha(self, repo, branch).await
    }
    async fn required_checks(&self, repo: &RepoName, branch: &str) -> Result<RequiredChecks> {
        Installation::required_checks(self, repo, branch).await
    }
    async fn checks(&self, repo: &RepoName, sha: &str) -> Result<Checks> {
        Installation::checks(self, repo, sha).await
    }
    async fn check_failures(
        &self,
        repo: &RepoName,
        sha: &str,
        names: &[String],
    ) -> Result<Vec<CheckFailure>> {
        Installation::check_failures(self, repo, sha, names).await
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
    async fn create_branch(&self, repo: &RepoName, branch: &str, sha: &str) -> Result<()> {
        Installation::create_branch(self, repo, branch, sha).await
    }
    async fn delete_branch(&self, repo: &RepoName, branch: &str) -> Result<()> {
        Installation::delete_branch(self, repo, branch).await
    }
    async fn merge_into_branch(
        &self,
        repo: &RepoName,
        branch: &str,
        head: &str,
        message: &str,
    ) -> Result<Option<BranchMerge>> {
        Installation::merge_into_branch(self, repo, branch, head, message).await
    }
    async fn open_draft_pr(
        &self,
        repo: &RepoName,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<PrNumber> {
        Installation::open_draft_pr(self, repo, head, base, title, body).await
    }
    async fn commit_tree(&self, repo: &RepoName, sha: &str) -> Result<String> {
        Installation::commit_tree(self, repo, sha).await
    }
    async fn create_label(&self, repo: &RepoName, label: &LabelSpec<'_>) -> Result<()> {
        Installation::create_label(self, repo, label).await
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
