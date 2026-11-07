//! What the queue needs from GitHub, apart from the rebase job. `Installation` in production; a
//! fake in the queue's tests.

use anyhow::Result;
use async_trait::async_trait;
use mergepatrol_core::{CheckFailure, ConfigText, LabelSpec, Labels, PrNumber, RepoName};
use mergepatrol_github::{
    BranchMerge, BranchRules, Checks, Installation, MergeOutcome, PullRequest, RepoSnapshot,
};

#[async_trait]
pub trait GitHub: Send + Sync {
    fn installation_id(&self) -> u64;
    /// The repo's config file on its default branch.
    async fn config_file(&self, repo: &RepoName) -> Result<ConfigText>;
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
    async fn branch_rules(&self, repo: &RepoName, branch: &str) -> Result<BranchRules>;
    async fn checks(&self, repo: &RepoName, sha: &str) -> Result<Checks>;
    /// Whether a workflow run on `sha` has yet to finish: queued, waiting for an approval, or
    /// running.
    async fn workflows_running(&self, repo: &RepoName, sha: &str) -> Result<bool>;
    /// How many of its fix budget the PR has used.
    async fn fixes_used(&self, repo: &RepoName, number: PrNumber) -> Result<u32>;
    /// What GitHub shows of why the checks `names` failed on `sha`.
    async fn check_failures(
        &self,
        repo: &RepoName,
        sha: &str,
        names: &[String],
    ) -> Result<Vec<CheckFailure>>;
    /// Runs the failed jobs of the workflow run `run_id`, one of `sha`'s, again.
    async fn rerun_failed_jobs(&self, repo: &RepoName, sha: &str, run_id: u64) -> Result<()>;
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
    /// The messages of every commit on PR `number`, oldest first.
    async fn commit_messages(&self, repo: &RepoName, number: PrNumber) -> Result<Vec<String>>;
    /// The PR's description as GitHub has it now, never from a cache.
    async fn pr_body(&self, repo: &RepoName, number: PrNumber) -> Result<String>;
    async fn set_body(&self, repo: &RepoName, number: PrNumber, body: &str) -> Result<()>;
    /// Dismisses each request for changes on the PR whose body holds one of `marks`.
    async fn dismiss_reviews_marked(
        &self,
        repo: &RepoName,
        number: PrNumber,
        marks: &[&str],
        message: &str,
    ) -> Result<usize>;
}

#[async_trait]
impl GitHub for Installation {
    fn installation_id(&self) -> u64 {
        self.id
    }
    async fn config_file(&self, repo: &RepoName) -> Result<ConfigText> {
        let found = Installation::config_file(self, repo).await?;
        Ok(ConfigText {
            at: found.at,
            text: found.file.text,
        })
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
    async fn branch_rules(&self, repo: &RepoName, branch: &str) -> Result<BranchRules> {
        Installation::branch_rules(self, repo, branch).await
    }
    async fn checks(&self, repo: &RepoName, sha: &str) -> Result<Checks> {
        Installation::checks(self, repo, sha).await
    }
    async fn workflows_running(&self, repo: &RepoName, sha: &str) -> Result<bool> {
        Installation::workflows_running(self, repo, sha).await
    }
    async fn fixes_used(&self, repo: &RepoName, number: PrNumber) -> Result<u32> {
        let messages = Installation::commit_messages(self, repo, number).await?;
        Ok(mergepatrol_core::fixes_used(
            messages.iter().rev().map(String::as_str),
        ))
    }
    async fn check_failures(
        &self,
        repo: &RepoName,
        sha: &str,
        names: &[String],
    ) -> Result<Vec<CheckFailure>> {
        Installation::check_failures(self, repo, sha, names).await
    }
    async fn rerun_failed_jobs(&self, repo: &RepoName, _: &str, run_id: u64) -> Result<()> {
        Installation::rerun_failed_jobs(self, repo, run_id).await
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
    async fn commit_messages(&self, repo: &RepoName, number: PrNumber) -> Result<Vec<String>> {
        Installation::commit_messages(self, repo, number).await
    }
    async fn pr_body(&self, repo: &RepoName, number: PrNumber) -> Result<String> {
        Installation::pr_body(self, repo, number).await
    }
    async fn dismiss_reviews_marked(
        &self,
        repo: &RepoName,
        number: PrNumber,
        marks: &[&str],
        message: &str,
    ) -> Result<usize> {
        Installation::dismiss_reviews_marked(self, repo, number, marks, message).await
    }
    async fn set_body(&self, repo: &RepoName, number: PrNumber, body: &str) -> Result<()> {
        Installation::set_body(self, repo, number, body).await
    }
}
