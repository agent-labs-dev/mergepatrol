//! The rule of record: the queue acts on a PR only after the PR has been told to merge. Never on
//! the PR being opened, pushed to, labeled otherwise, reviewed or checked.
//!
//! Being told to merge is state, and it is three things at once: the PR is open, it or a layer of
//! its stack carries the repo's trigger label, and its lane holds a place for it. A path with no
//! merge request to act on changes nothing: no agent run, no review, no comment, no approval.

use anyhow::Result;
use mergepatrol_core::{PrNumber, RepoName};
use mergepatrol_github::{Installation, PullRequest};
use mergepatrol_store::Store;

/// Whether the PR has a merge request: open, labeled `trigger_label` itself or through its stack,
/// and holding a place in one of its repo's lanes. `pr` is read by the caller, so it is only as
/// current as that read.
pub async fn merge_requested(
    store: &dyn Store,
    gh: &Installation,
    repo: &RepoName,
    pr: &PullRequest,
    trigger_label: &str,
) -> Result<bool> {
    Ok(mergepatrol_core::merge_requested(
        pr.is_open(),
        gh.labeled_for_queue(repo, pr, trigger_label).await?,
        in_lane(store, gh.id, repo, pr.number).await?,
    ))
}

/// Whether a lane of the repo holds a place for the PR: alone, or as a layer of a stack.
async fn in_lane(
    store: &dyn Store,
    installation: u64,
    repo: &RepoName,
    number: PrNumber,
) -> Result<bool> {
    let lanes = store.repo_lanes(installation, repo).await?;
    Ok(lanes
        .iter()
        .any(|(_, lane)| lane.entries.iter().any(|e| e.prs().contains(&number))))
}
