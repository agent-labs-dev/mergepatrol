use std::collections::HashSet;
use std::path::Path;

use anyhow::{Result, bail};
use futures_util::{StreamExt, TryStreamExt, stream};
use mergepatrol_core::RepoName;
use mergepatrol_git::{Git, TreeChange};
use mergepatrol_github::{Installation, TreeEntry};

pub struct Publish<'a> {
    pub repo: &'a RepoName,
    pub repo_id: &'a str,
    pub branch: &'a str,
    /// The branch must still be here, or the update fails.
    pub expected_head: &'a str,
    pub base: &'a str,
    pub local_head: &'a str,
}

/// Blob uploads in flight at once.
const UPLOADS: usize = 8;

/// Recreates the commits `base..local_head` on GitHub, then moves the branch.
pub async fn publish(gh: &Installation, git: &Git, wt: &Path, p: Publish<'_>) -> Result<String> {
    let head = create_commits(gh, git, wt, &p).await?;
    gh.force_update_branch(p.repo_id, p.branch, p.expected_head, &head)
        .await?;
    Ok(head)
}

/// Recreates the commits `base..local_head` on GitHub through the Git Data API, and returns the
/// last. No branch moves. GitHub signs commits it creates for the App, so the branch passes
/// "require signed commits". Original authors stay as `Co-authored-by:` trailers.
pub async fn create_commits(
    gh: &Installation,
    git: &Git,
    wt: &Path,
    p: &Publish<'_>,
) -> Result<String> {
    let commits = git.commits(wt, p.base, p.local_head).await?;
    if commits.is_empty() {
        bail!("the PR has no changes left after the rebase");
    }

    // A clean rebase reuses the PR's own blobs, which GitHub has: only resolutions and files
    // that both sides changed are new.
    let on_github: HashSet<String> = git
        .objects(wt, p.base, p.expected_head)
        .await?
        .into_iter()
        .collect();
    let changes = commits.iter().flat_map(|c| &c.changes);
    upload_blobs(gh, git, wt, p.repo, changes, |sha| !on_github.contains(sha)).await?;

    let mut parent = p.base.to_owned();
    let mut parent_tree = git.rev_parse(wt, &format!("{}^{{tree}}", p.base)).await?;

    for commit in commits {
        let entries: Vec<TreeEntry> = commit.changes.iter().map(tree_entry).collect();
        let tree = gh.create_tree(p.repo, &parent_tree, &entries).await?;
        if tree != commit.tree {
            bail!(
                "tree of {} came out as {tree} on GitHub, expected {}",
                commit.sha,
                commit.tree
            );
        }
        let message = git
            .add_trailer(
                wt,
                &commit.message,
                &format!("Co-authored-by: {}", commit.author),
            )
            .await?;
        parent = gh.create_commit(p.repo, &message, &tree, &parent).await?;
        parent_tree = tree;
    }

    Ok(parent)
}

/// Recreates `local_head`, a merge of `base` into `expected_head`, on GitHub: one commit with
/// both as its parents, and the merge's message. No branch moves. Only what the merge made
/// itself is new to GitHub: the files that both sides changed, and the agent's resolution.
pub async fn create_merge(
    gh: &Installation,
    git: &Git,
    wt: &Path,
    p: &Publish<'_>,
) -> Result<String> {
    let changes = git.tree_changes(wt, p.expected_head, p.local_head).await?;
    let made: HashSet<String> = git
        .objects_beyond(wt, p.local_head, &[p.expected_head, p.base])
        .await?
        .into_iter()
        .collect();
    upload_blobs(gh, git, wt, p.repo, &changes, |sha| made.contains(sha)).await?;

    let head_tree = git
        .rev_parse(wt, &format!("{}^{{tree}}", p.expected_head))
        .await?;
    let entries: Vec<TreeEntry> = changes.iter().map(tree_entry).collect();
    let tree = gh.create_tree(p.repo, &head_tree, &entries).await?;
    let local_tree = git
        .rev_parse(wt, &format!("{}^{{tree}}", p.local_head))
        .await?;
    if tree != local_tree {
        bail!(
            "tree of the merge {} came out as {tree} on GitHub, expected {local_tree}",
            p.local_head
        );
    }
    let message = git.commit_message(wt, p.local_head).await?;
    gh.create_commit_of(p.repo, &message, &tree, &[p.expected_head, p.base])
        .await
}

/// Uploads the blobs of `changes` that are `new` to GitHub.
async fn upload_blobs<'c>(
    gh: &Installation,
    git: &Git,
    wt: &Path,
    repo: &RepoName,
    changes: impl IntoIterator<Item = &'c TreeChange>,
    new: impl Fn(&str) -> bool,
) -> Result<()> {
    let blobs: HashSet<String> = changes
        .into_iter()
        .filter(|c| !c.is_submodule())
        .filter_map(|c| c.sha.clone())
        .filter(|sha| new(sha))
        .collect();
    stream::iter(blobs)
        .map(|sha| async move {
            let remote = gh
                .create_blob(repo, &git.read_blob(wt, &sha).await?)
                .await?;
            if remote != sha {
                bail!("blob {sha} uploaded as {remote}");
            }
            Ok(())
        })
        .buffer_unordered(UPLOADS)
        .try_collect::<Vec<()>>()
        .await?;
    Ok(())
}

/// The entry of GitHub's tree that makes `change`.
fn tree_entry(change: &TreeChange) -> TreeEntry {
    let (mode, kind) = match &change.sha {
        None => ("100644".to_owned(), "blob"),
        Some(_) if change.is_submodule() => (change.mode.clone(), "commit"),
        Some(_) => (change.mode.clone(), "blob"),
    };
    TreeEntry {
        path: change.path.clone(),
        mode,
        kind,
        sha: change.sha.clone(),
    }
}
