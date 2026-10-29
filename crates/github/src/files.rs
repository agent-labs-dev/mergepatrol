//! Files on a repo's default branch: reading one, finding which repos have one, and proposing a
//! change to one as a PR.

use anyhow::{Context, Result};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

use mergepatrol_core::RepoName;

use crate::Installation;
use crate::repo::TreeEntry;

/// Repos per GraphQL query, when finding which have a file.
const PER_QUERY: usize = 50;

/// A file at the head of a repo's default branch.
#[derive(Debug, Clone)]
pub struct BranchFile {
    pub branch: String,
    pub sha: String,
    /// `None` when the branch has no such file.
    pub text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRef {
    pub number: u64,
    pub url: String,
}

/// A change to files, proposed on `branch`.
pub struct Proposal<'a> {
    pub base: &'a BranchFile,
    /// Paths and their text.
    pub files: &'a [(&'a str, &'a str)],
    pub branch: &'a str,
    pub title: &'a str,
    pub body: &'a str,
}

impl Installation {
    pub async fn default_branch_file(&self, repo: &RepoName, path: &str) -> Result<BranchFile> {
        let query = "query($owner: String!, $name: String!, $path: String!) {
            repository(owner: $owner, name: $name) {
              defaultBranchRef { name target { oid ... on Commit {
                file(path: $path) { object { ... on Blob { text } } }
              } } }
            }
          }";
        let data = self
            .graphql_missing_ok(
                query,
                json!({ "owner": repo.owner, "name": repo.name, "path": path }),
            )
            .await?;
        let r = &data["repository"]["defaultBranchRef"];
        Ok(BranchFile {
            branch: r["name"]
                .as_str()
                .context("the repo has no default branch")?
                .to_owned(),
            sha: r["target"]["oid"].as_str().context("trunk sha")?.to_owned(),
            text: r["target"]["file"]["object"]["text"]
                .as_str()
                .map(str::to_owned),
        })
    }

    /// `path` on `branch`, or `None` when either is missing.
    pub async fn branch_file(
        &self,
        repo: &RepoName,
        branch: &str,
        path: &str,
    ) -> Result<Option<String>> {
        let query = "query($owner: String!, $name: String!, $expr: String!) {
            repository(owner: $owner, name: $name) { object(expression: $expr) { ... on Blob { text } } }
          }";
        let expr = format!("refs/heads/{branch}:{path}");
        let data = self
            .graphql_missing_ok(
                query,
                json!({ "owner": repo.owner, "name": repo.name, "expr": expr }),
            )
            .await?;
        Ok(data["repository"]["object"]["text"]
            .as_str()
            .map(str::to_owned))
    }

    /// Which of `repos` have `path` on their default branch, in order. A repo GitHub cannot
    /// show, or an empty one, has not.
    pub async fn have_file(&self, repos: &[RepoName], path: &str) -> Result<Vec<bool>> {
        let mut found = Vec::with_capacity(repos.len());
        for chunk in repos.chunks(PER_QUERY) {
            let fields: Vec<String> = chunk
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    format!(
                        "r{i}: repository(owner: {}, name: {}) {{ defaultBranchRef {{ target {{ ... on Commit {{ file(path: $path) {{ oid }} }} }} }} }}",
                        json!(r.owner),
                        json!(r.name)
                    )
                })
                .collect();
            let query = format!("query($path: String!) {{ {} }}", fields.join("\n"));
            let data = self
                .graphql_missing_ok(&query, json!({ "path": path }))
                .await?;
            for i in 0..chunk.len() {
                let file = &data[format!("r{i}")]["defaultBranchRef"]["target"]["file"];
                found.push(file.is_object());
            }
        }
        Ok(found)
    }

    /// A GraphQL query whose answer may lack what does not exist: GitHub reports a missing file
    /// or repo as a `NOT_FOUND` error, beside the rest of the answer. Any other error fails it.
    async fn graphql_missing_ok(&self, query: &str, variables: Value) -> Result<Value> {
        let res: Value = self
            .rest(
                Method::POST,
                "/graphql",
                Some(json!({ "query": query, "variables": variables })),
            )
            .await?;
        let errors = res["errors"].as_array().cloned().unwrap_or_default();
        if res["data"].is_null() || errors.iter().any(|e| e["type"] != "NOT_FOUND") {
            anyhow::bail!("GraphQL: {}", res["errors"]);
        }
        Ok(res["data"].clone())
    }

    /// The open PR from `branch` in the same repo, if there is one.
    pub async fn open_pr_from(&self, repo: &RepoName, branch: &str) -> Result<Option<PullRef>> {
        let prs: Vec<Value> = self
            .rest(
                Method::GET,
                &format!(
                    "/repos/{repo}/pulls?state=open&head={}:{branch}",
                    repo.owner
                ),
                None,
            )
            .await?;
        Ok(prs.first().and_then(pull_ref))
    }

    /// Commits the files on `branch`, from the default branch's head, and opens a PR from it, or
    /// updates the one open. The branch is Mergepatrol's: whatever was on it is replaced.
    pub async fn propose_file(&self, repo: &RepoName, p: Proposal<'_>) -> Result<PullRef> {
        let mut entries = Vec::with_capacity(p.files.len());
        for (path, text) in p.files {
            entries.push(TreeEntry {
                path: (*path).to_owned(),
                mode: "100644".into(),
                kind: "blob",
                sha: Some(self.create_blob(repo, text.as_bytes()).await?),
            });
        }
        let base_tree = self.commit_tree(repo, &p.base.sha).await?;
        let tree = self.create_tree(repo, &base_tree, &entries).await?;
        let commit = self
            .create_commit(repo, p.title, &tree, &p.base.sha)
            .await?;

        let res = self
            .request(
                Method::PATCH,
                &format!("/repos/{repo}/git/refs/heads/{}", p.branch),
                Some(json!({ "sha": commit, "force": true })),
            )
            .await?;
        match res.status {
            s if s.is_success() => {}
            // The branch does not exist.
            StatusCode::UNPROCESSABLE_ENTITY | StatusCode::NOT_FOUND => {
                self.create_branch(repo, p.branch, &commit).await?
            }
            s => anyhow::bail!("update branch {}: {s} {}", p.branch, res.body),
        }

        let fields = json!({ "title": p.title, "body": p.body });
        let pr: Value = match self.open_pr_from(repo, p.branch).await? {
            Some(open) => {
                self.rest(
                    Method::PATCH,
                    &format!("/repos/{repo}/pulls/{}", open.number),
                    Some(fields),
                )
                .await?
            }
            None => {
                let mut fields = fields;
                fields["head"] = json!(p.branch);
                fields["base"] = json!(p.base.branch);
                self.rest(Method::POST, &format!("/repos/{repo}/pulls"), Some(fields))
                    .await?
            }
        };
        pull_ref(&pr).context("GitHub returned no PR")
    }
}

fn pull_ref(pr: &Value) -> Option<PullRef> {
    Some(PullRef {
        number: pr["number"].as_u64()?,
        url: pr["html_url"].as_str()?.to_owned(),
    })
}
