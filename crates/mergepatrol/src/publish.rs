use std::collections::HashSet;
use std::path::Path;

use anyhow::{Result, bail};
use mergepatrol_core::RepoName;
use mergepatrol_git::Git;
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

/// Recreates the commits `base..local_head` on GitHub through the Git Data API, then moves
/// the branch. GitHub signs commits it creates for the App, so the branch passes
/// "require signed commits". Original authors stay as `Co-authored-by:` trailers.
pub async fn publish(gh: &Installation, git: &Git, wt: &Path, p: Publish<'_>) -> Result<String> {
    let commits = git.commits(wt, p.base, p.local_head).await?;
    if commits.is_empty() {
        bail!("the PR has no changes left after the rebase");
    }

    let mut parent = p.base.to_owned();
    let mut parent_tree = git.rev_parse(wt, &format!("{}^{{tree}}", p.base)).await?;
    let mut uploaded = HashSet::new();

    for commit in commits {
        let mut entries = Vec::with_capacity(commit.changes.len());
        for change in &commit.changes {
            let entry = match &change.sha {
                None => TreeEntry {
                    path: change.path.clone(),
                    mode: "100644".into(),
                    kind: "blob",
                    sha: None,
                },
                Some(sha) if change.is_submodule() => TreeEntry {
                    path: change.path.clone(),
                    mode: change.mode.clone(),
                    kind: "commit",
                    sha: Some(sha.clone()),
                },
                Some(sha) => {
                    if uploaded.insert(sha.clone()) {
                        let remote = gh
                            .create_blob(p.repo, &git.read_blob(wt, sha).await?)
                            .await?;
                        if &remote != sha {
                            bail!("blob {} uploaded as {remote}, expected {sha}", change.path);
                        }
                    }
                    TreeEntry {
                        path: change.path.clone(),
                        mode: change.mode.clone(),
                        kind: "blob",
                        sha: Some(sha.clone()),
                    }
                }
            };
            entries.push(entry);
        }

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

    gh.force_update_branch(p.repo_id, p.branch, p.expected_head, &parent)
        .await?;
    Ok(parent)
}
