//! Files on a repo's default branch: reading one, finding which repos have one, and proposing a
//! change to one as a PR.

use anyhow::{Context, Result};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

use mergepatrol_core::RepoName;

use crate::Installation;
use crate::app::AsUser;
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

const DEFAULT_BRANCH_FILE: &str = "query($owner: String!, $name: String!, $path: String!) {
    repository(owner: $owner, name: $name) {
      defaultBranchRef { name target { oid ... on Commit {
        file(path: $path) { object { ... on Blob { text } } }
      } } }
    }
  }";

/// What [`Installation::objects_at`] found.
#[derive(Debug, Clone)]
pub struct ObjectsAt {
    /// The default branch and its head. No text.
    pub head: BranchFile,
    pub pull_request: Option<PullRef>,
    /// One per lookup, in order.
    pub objects: Vec<Option<GitObject>>,
}

/// A git object: a blob's text (a symlink's is its target), or a tree's entries as
/// (name, mode).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitObject {
    Blob(String),
    Tree(Vec<(String, u32)>),
}

/// A tree entry's mode for a symlink, as GraphQL gives it: `0o120000`.
pub const SYMLINK_MODE: u32 = 0o120000;

/// The default branch's file, from a `DEFAULT_BRANCH_FILE` answer.
fn branch_file(data: &Value) -> Result<BranchFile> {
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

impl AsUser {
    /// As [`Installation::default_branch_file`], read as the user.
    pub async fn default_branch_file(&self, repo: &RepoName, path: &str) -> Result<BranchFile> {
        let data = self
            .graphql_missing_ok(
                "default branch file",
                DEFAULT_BRANCH_FILE,
                json!({ "owner": repo.owner, "name": repo.name, "path": path }),
            )
            .await?;
        branch_file(&data)
    }
}

impl Installation {
    pub async fn default_branch_file(&self, repo: &RepoName, path: &str) -> Result<BranchFile> {
        let data = self
            .graphql_missing_ok(
                "default branch file",
                DEFAULT_BRANCH_FILE,
                json!({ "owner": repo.owner, "name": repo.name, "path": path }),
            )
            .await?;
        branch_file(&data)
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
                "branch file",
                query,
                json!({ "owner": repo.owner, "name": repo.name, "expr": expr }),
            )
            .await?;
        Ok(data["repository"]["object"]["text"]
            .as_str()
            .map(str::to_owned))
    }

    /// The objects at `lookups`, as (git ref, path), in one query, with the default branch's
    /// head and the open PR from `pr_branch`. `HEAD` is the default branch. A missing ref or
    /// path is `None`.
    pub async fn objects_at(
        &self,
        repo: &RepoName,
        lookups: &[(&str, &str)],
        pr_branch: &str,
    ) -> Result<ObjectsAt> {
        let fields: Vec<String> = lookups
            .iter()
            .enumerate()
            .map(|(i, (rev, path))| {
                format!(
                    "o{i}: object(expression: {}) {{ __typename ... on Blob {{ text }} ... on Tree {{ entries {{ name mode }} }} }}",
                    json!(format!("{rev}:{path}"))
                )
            })
            .collect();
        let query = format!(
            "query($owner: String!, $name: String!, $head: String!) {{ repository(owner: $owner, name: $name) {{
              defaultBranchRef {{ name target {{ oid }} }}
              pullRequests(headRefName: $head, states: OPEN, first: 10) {{ nodes {{ number url isCrossRepository }} }}
              {} }} }}",
            fields.join("\n")
        );
        let data = self
            .graphql_missing_ok(
                "objects at",
                &query,
                json!({ "owner": repo.owner, "name": repo.name, "head": pr_branch }),
            )
            .await?;
        let r = &data["repository"];
        let head = BranchFile {
            branch: r["defaultBranchRef"]["name"]
                .as_str()
                .context("the repo has no default branch")?
                .to_owned(),
            sha: r["defaultBranchRef"]["target"]["oid"]
                .as_str()
                .context("trunk sha")?
                .to_owned(),
            text: None,
        };
        // A fork's PR may have a branch of the same name: only the repo's own is MergeLoop's,
        // as `open_pr_from` finds it.
        let pull_request = r["pullRequests"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|p| p["isCrossRepository"] == false)
            .and_then(|p| {
                Some(PullRef {
                    number: p["number"].as_u64()?,
                    url: p["url"].as_str().unwrap_or_default().to_owned(),
                })
            });
        let objects = (0..lookups.len())
            .map(|i| {
                let o = &r[format!("o{i}")];
                match o["__typename"].as_str() {
                    Some("Blob") => Some(GitObject::Blob(
                        o["text"].as_str().unwrap_or_default().to_owned(),
                    )),
                    Some("Tree") => Some(GitObject::Tree(
                        o["entries"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|e| {
                                Some((e["name"].as_str()?.to_owned(), e["mode"].as_u64()? as u32))
                            })
                            .collect(),
                    )),
                    _ => None,
                }
            })
            .collect();
        Ok(ObjectsAt {
            head,
            pull_request,
            objects,
        })
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
                .graphql_missing_ok("have file", &query, json!({ "path": path }))
                .await?;
            for i in 0..chunk.len() {
                let file = &data[format!("r{i}")]["defaultBranchRef"]["target"]["file"];
                found.push(file.is_object());
            }
        }
        Ok(found)
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
    /// updates the one open. The branch is MergeLoop's: whatever was on it is replaced.
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
