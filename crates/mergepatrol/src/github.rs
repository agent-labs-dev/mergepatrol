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
    /// The open PRs of a stack, bottom to top.
    async fn stack_layers(&self, repo: &RepoName, stack: PrNumber) -> Result<Vec<PrNumber>>;
    async fn branch_sha(&self, repo: &RepoName, branch: &str) -> Result<String>;
    /// A PR's head, from its pull ref: where a fork PR's head shows in the repo.
    async fn pull_head(&self, repo: &RepoName, number: PrNumber) -> Result<String>;
    /// Whether `head` needs a rebase onto `base`.
    async fn behind(&self, repo: &RepoName, base: &str, head: &str) -> Result<bool>;
    /// Whether workflow runs on `sha` wait for a maintainer's approval.
    async fn awaiting_approval(&self, repo: &RepoName, sha: &str) -> Result<bool>;
    /// Whether PR `number` changes a file under `dir`.
    async fn changes_under(&self, repo: &RepoName, number: PrNumber, dir: &str) -> Result<bool>;
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
    /// The merge commit of the PR, if it merged at `head`.
    async fn merged_as(
        &self,
        repo: &RepoName,
        number: PrNumber,
        head: &str,
    ) -> Result<Option<String>>;
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
    async fn stack_layers(&self, repo: &RepoName, stack: PrNumber) -> Result<Vec<PrNumber>> {
        Installation::stack_layers(self, repo, stack).await
    }
    async fn branch_sha(&self, repo: &RepoName, branch: &str) -> Result<String> {
        Installation::branch_sha(self, repo, branch).await
    }
    async fn pull_head(&self, repo: &RepoName, number: PrNumber) -> Result<String> {
        Installation::pull_head(self, repo, number).await
    }
    async fn behind(&self, repo: &RepoName, base: &str, head: &str) -> Result<bool> {
        Installation::behind(self, repo, base, head).await
    }
    async fn awaiting_approval(&self, repo: &RepoName, sha: &str) -> Result<bool> {
        Installation::awaiting_approval(self, repo, sha).await
    }
    async fn changes_under(&self, repo: &RepoName, number: PrNumber, dir: &str) -> Result<bool> {
        Installation::changes_under(self, repo, number, dir).await
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
    async fn merged_as(
        &self,
        repo: &RepoName,
        number: PrNumber,
        head: &str,
    ) -> Result<Option<String>> {
        Installation::merged_as(self, repo, number, head).await
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
