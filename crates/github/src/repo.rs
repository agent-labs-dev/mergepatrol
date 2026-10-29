use anyhow::{Context, Result};
use base64::Engine;
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use mergepatrol_core::{PrNumber, RepoName};

use crate::Installation;

/// Marks the one comment per PR that Mergepatrol keeps up to date.
const STICKY_MARKER: &str = "<!-- mergepatrol:status -->";

#[derive(Debug, Clone)]
pub struct RepoSnapshot {
    /// GraphQL node ID, for `updateRefs`.
    pub id: String,
    pub default_branch: String,
    pub trunk_sha: String,
    /// `squash`, `merge` or `rebase`: the repo's default for this App.
    pub merge_method: String,
    /// Open PRs with the trigger label, oldest first.
    pub labeled: Vec<PullRequest>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequest {
    pub number: PrNumber,
    pub title: String,
    pub body: String,
    pub is_draft: bool,
    pub is_cross_repository: bool,
    /// `APPROVED`, `REVIEW_REQUIRED`, `CHANGES_REQUESTED`, or `None` when no review is required.
    pub review_decision: Option<String>,
    pub base_ref_name: String,
    pub head_ref_name: String,
    pub head_ref_oid: String,
    #[serde(deserialize_with = "label_names")]
    pub labels: Vec<String>,
}

impl PullRequest {
    pub fn has_label(&self, label: &str) -> bool {
        self.labels.iter().any(|l| l == label)
    }

    /// Why GitHub would not merge this PR yet, if it would not.
    pub fn hold_reason(&self) -> Option<String> {
        if self.is_draft {
            return Some("the PR is a draft".into());
        }
        match self.review_decision.as_deref() {
            Some("REVIEW_REQUIRED") => Some("waiting for an approval".into()),
            Some("CHANGES_REQUESTED") => Some("a reviewer requested changes".into()),
            _ => None,
        }
    }
}

fn label_names<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    struct Conn {
        nodes: Vec<Label>,
    }
    #[derive(Deserialize)]
    struct Label {
        name: String,
    }
    Ok(Conn::deserialize(d)?
        .nodes
        .into_iter()
        .map(|l| l.name)
        .collect())
}

#[derive(Debug, Clone, Serialize)]
pub struct TreeEntry {
    pub path: String,
    pub mode: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    /// `None` deletes the path.
    pub sha: Option<String>,
}

/// A merge commit that GitHub made on a branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchMerge {
    pub sha: String,
    pub tree: String,
}

#[derive(Debug)]
pub enum MergeOutcome {
    Merged { sha: String },
    Rejected(String),
}

const PR_FIELDS: &str = "number title body isDraft isCrossRepository reviewDecision baseRefName headRefName headRefOid labels(first: 50) { nodes { name } }";

impl Installation {
    pub async fn snapshot(&self, repo: &RepoName, label: &str) -> Result<RepoSnapshot> {
        let query = format!(
            "query($owner: String!, $name: String!, $label: String!) {{
              repository(owner: $owner, name: $name) {{
                id
                viewerDefaultMergeMethod
                defaultBranchRef {{ name target {{ oid }} }}
                pullRequests(states: OPEN, labels: [$label], first: 100, orderBy: {{ field: CREATED_AT, direction: ASC }}) {{
                  nodes {{ {PR_FIELDS} }}
                }}
              }}
            }}"
        );
        let data: Value = self
            .graphql(
                &query,
                json!({ "owner": repo.owner, "name": repo.name, "label": label }),
            )
            .await?;
        let r = &data["repository"];
        Ok(RepoSnapshot {
            id: r["id"].as_str().context("repository.id")?.to_owned(),
            default_branch: r["defaultBranchRef"]["name"]
                .as_str()
                .context("default branch")?
                .to_owned(),
            trunk_sha: r["defaultBranchRef"]["target"]["oid"]
                .as_str()
                .context("trunk sha")?
                .to_owned(),
            merge_method: r["viewerDefaultMergeMethod"]
                .as_str()
                .unwrap_or("MERGE")
                .to_lowercase(),
            labeled: serde_json::from_value(r["pullRequests"]["nodes"].clone())?,
        })
    }

    pub async fn pull_request(&self, repo: &RepoName, number: PrNumber) -> Result<PullRequest> {
        let query = format!(
            "query($owner: String!, $name: String!, $number: Int!) {{
              repository(owner: $owner, name: $name) {{ pullRequest(number: $number) {{ {PR_FIELDS} }} }}
            }}"
        );
        let data: Value = self
            .graphql(
                &query,
                json!({ "owner": repo.owner, "name": repo.name, "number": number }),
            )
            .await?;
        Ok(serde_json::from_value(
            data["repository"]["pullRequest"].clone(),
        )?)
    }

    pub async fn branch_sha(&self, repo: &RepoName, branch: &str) -> Result<String> {
        let r: Value = self
            .rest(
                Method::GET,
                &format!("/repos/{repo}/git/ref/heads/{branch}"),
                None,
            )
            .await?;
        Ok(r["object"]["sha"].as_str().context("ref sha")?.to_owned())
    }

    pub async fn create_blob(&self, repo: &RepoName, content: &[u8]) -> Result<String> {
        let content = base64::engine::general_purpose::STANDARD.encode(content);
        let r: Value = self
            .rest(
                Method::POST,
                &format!("/repos/{repo}/git/blobs"),
                Some(json!({ "content": content, "encoding": "base64" })),
            )
            .await?;
        Ok(r["sha"].as_str().context("blob sha")?.to_owned())
    }

    pub async fn create_tree(
        &self,
        repo: &RepoName,
        base_tree: &str,
        entries: &[TreeEntry],
    ) -> Result<String> {
        let r: Value = self
            .rest(
                Method::POST,
                &format!("/repos/{repo}/git/trees"),
                Some(json!({ "base_tree": base_tree, "tree": entries })),
            )
            .await?;
        Ok(r["sha"].as_str().context("tree sha")?.to_owned())
    }

    /// Creates a commit with no author or committer, so GitHub signs it as the App's bot.
    pub async fn create_commit(
        &self,
        repo: &RepoName,
        message: &str,
        tree: &str,
        parent: &str,
    ) -> Result<String> {
        let r: Value = self
            .rest(
                Method::POST,
                &format!("/repos/{repo}/git/commits"),
                Some(json!({ "message": message, "tree": tree, "parents": [parent] })),
            )
            .await?;
        Ok(r["sha"].as_str().context("commit sha")?.to_owned())
    }

    /// Moves `branch` from `before` to `after`. It fails if the branch is not at `before`.
    /// `updateRefs` is atomic, so a stack can later move all its branches in one call.
    pub async fn force_update_branch(
        &self,
        repo_id: &str,
        branch: &str,
        before: &str,
        after: &str,
    ) -> Result<()> {
        let query = "mutation($repo: ID!, $updates: [RefUpdate!]!) {
            updateRefs(input: { repositoryId: $repo, refUpdates: $updates }) { clientMutationId }
        }";
        let updates = json!([{ "name": format!("refs/heads/{branch}"), "beforeOid": before, "afterOid": after, "force": true }]);
        let _: Value = self
            .graphql(query, json!({ "repo": repo_id, "updates": updates }))
            .await?;
        Ok(())
    }

    pub async fn create_branch(&self, repo: &RepoName, branch: &str, sha: &str) -> Result<()> {
        let _: Value = self
            .rest(
                Method::POST,
                &format!("/repos/{repo}/git/refs"),
                Some(json!({ "ref": format!("refs/heads/{branch}"), "sha": sha })),
            )
            .await?;
        Ok(())
    }

    /// Deletes `branch`, if it is there. GitHub closes the PRs it heads.
    pub async fn delete_branch(&self, repo: &RepoName, branch: &str) -> Result<()> {
        let res = self
            .request(
                Method::DELETE,
                &format!("/repos/{repo}/git/refs/heads/{branch}"),
                None,
            )
            .await?;
        // 422: the ref does not exist.
        if !res.status.is_success()
            && res.status != StatusCode::NOT_FOUND
            && res.status != StatusCode::UNPROCESSABLE_ENTITY
        {
            anyhow::bail!("delete branch {branch}: {} {}", res.status, res.body);
        }
        Ok(())
    }

    /// Merges `head` into `branch` on GitHub, with a merge commit. `None` when they conflict,
    /// or when `branch` already has `head`.
    pub async fn merge_into_branch(
        &self,
        repo: &RepoName,
        branch: &str,
        head: &str,
        message: &str,
    ) -> Result<Option<BranchMerge>> {
        let res = self
            .request(
                Method::POST,
                &format!("/repos/{repo}/merges"),
                Some(json!({ "base": branch, "head": head, "commit_message": message })),
            )
            .await?;
        match res.status {
            StatusCode::CREATED => Ok(Some(BranchMerge {
                sha: res.body["sha"].as_str().context("merge sha")?.to_owned(),
                tree: res.body["commit"]["tree"]["sha"]
                    .as_str()
                    .context("merge tree")?
                    .to_owned(),
            })),
            StatusCode::CONFLICT | StatusCode::NO_CONTENT => Ok(None),
            s => anyhow::bail!("merge {head} into {branch}: {s} {}", res.body),
        }
    }

    pub async fn open_draft_pr(
        &self,
        repo: &RepoName,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<PrNumber> {
        let r: Value = self
            .rest(
                Method::POST,
                &format!("/repos/{repo}/pulls"),
                Some(json!({ "head": head, "base": base, "title": title, "body": body, "draft": true })),
            )
            .await?;
        r["number"].as_u64().context("PR number")
    }

    pub async fn commit_tree(&self, repo: &RepoName, sha: &str) -> Result<String> {
        let r: Value = self
            .rest(
                Method::GET,
                &format!("/repos/{repo}/git/commits/{sha}"),
                None,
            )
            .await?;
        Ok(r["tree"]["sha"].as_str().context("commit tree")?.to_owned())
    }

    pub async fn merge(
        &self,
        repo: &RepoName,
        number: PrNumber,
        sha: &str,
        method: &str,
    ) -> Result<MergeOutcome> {
        let res = self
            .request(
                Method::PUT,
                &format!("/repos/{repo}/pulls/{number}/merge"),
                Some(json!({ "sha": sha, "merge_method": method })),
            )
            .await?;
        match res.status {
            s if s.is_success() => Ok(MergeOutcome::Merged {
                sha: res.body["sha"].as_str().unwrap_or_default().to_owned(),
            }),
            StatusCode::METHOD_NOT_ALLOWED
            | StatusCode::CONFLICT
            | StatusCode::UNPROCESSABLE_ENTITY => Ok(MergeOutcome::Rejected(
                res.body["message"]
                    .as_str()
                    .unwrap_or("not mergeable")
                    .to_owned(),
            )),
            s => anyhow::bail!("merge #{number}: {s} {}", res.body),
        }
    }

    pub async fn add_label(&self, repo: &RepoName, number: PrNumber, label: &str) -> Result<()> {
        let _: Value = self
            .rest(
                Method::POST,
                &format!("/repos/{repo}/issues/{number}/labels"),
                Some(json!({ "labels": [label] })),
            )
            .await?;
        Ok(())
    }

    pub async fn remove_label(&self, repo: &RepoName, number: PrNumber, label: &str) -> Result<()> {
        let res = self
            .request(
                Method::DELETE,
                &format!("/repos/{repo}/issues/{number}/labels/{label}"),
                None,
            )
            .await?;
        if !res.status.is_success() && res.status != StatusCode::NOT_FOUND {
            anyhow::bail!(
                "remove label {label} from #{number}: {} {}",
                res.status,
                res.body
            );
        }
        Ok(())
    }

    /// Writes the status comment: edits the one Mergepatrol posted before, or posts it.
    pub async fn sticky_comment(
        &self,
        repo: &RepoName,
        number: PrNumber,
        body: &str,
    ) -> Result<()> {
        let body = format!("{STICKY_MARKER}\n{body}");
        let comments: Vec<Value> = self
            .rest(
                Method::GET,
                &format!("/repos/{repo}/issues/{number}/comments?per_page=100"),
                None,
            )
            .await?;
        let mine = comments.iter().find(|c| {
            c["user"]["type"] == "Bot"
                && c["body"]
                    .as_str()
                    .is_some_and(|b| b.starts_with(STICKY_MARKER))
        });
        match mine.and_then(|c| c["id"].as_u64()) {
            Some(id) => {
                let _: Value = self
                    .rest(
                        Method::PATCH,
                        &format!("/repos/{repo}/issues/comments/{id}"),
                        Some(json!({ "body": body })),
                    )
                    .await?;
            }
            None => {
                let _: Value = self
                    .rest(
                        Method::POST,
                        &format!("/repos/{repo}/issues/{number}/comments"),
                        Some(json!({ "body": body })),
                    )
                    .await?;
            }
        }
        Ok(())
    }
}
