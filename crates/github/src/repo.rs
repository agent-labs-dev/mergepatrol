use anyhow::{Context, Result};
use base64::Engine;
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use mergepatrol_core::{
    FIXED_BY, LabelSpec, Labels, Person, PrNumber, PrSummary, RepoName, Review,
};

use crate::Installation;
use crate::app::AsUser;
use crate::codeowners::{self, CodeOwners};

/// Marks the one comment per PR that Mergepatrol keeps up to date.
const STICKY_MARKER: &str = "<!-- mergepatrol:status -->";

#[derive(Debug, Clone)]
pub struct RepoSnapshot {
    /// GraphQL node ID, for `updateRefs`.
    pub id: String,
    pub default_branch: String,
    pub trunk_sha: String,
    /// Private, or internal: not open to everyone.
    pub private: bool,
    /// `squash`, `merge` or `rebase`: the repo's default for this App.
    pub merge_method: String,
    /// Open PRs with the trigger label, oldest first.
    pub labeled: Vec<PullRequest>,
    /// Queue labels the repo does not have yet.
    pub missing_labels: Vec<String>,
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
    /// The changed files, up to 100: what the trunk's code owners are checked against. Empty
    /// when the query did not ask.
    #[serde(default, deserialize_with = "file_paths")]
    pub files: Vec<String>,
    /// Whether a code owner must still approve, which `reviewDecision` can leave out.
    #[serde(skip)]
    pub awaits_code_owner: bool,
    /// The stack the PR is a layer of. `None` outside a stack.
    #[serde(default)]
    pub stack: Option<StackRef>,
    /// Whether the author lets maintainers, and so the App, push to a fork's branch.
    #[serde(default)]
    pub maintainer_can_modify: bool,
    /// The repo the head branch is in: a fork's, for a PR from one. `None` once it is deleted.
    #[serde(default)]
    pub head_repository: Option<HeadRepository>,
    #[serde(flatten)]
    pub details: PrDetails,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeadRepository {
    /// `owner/name`.
    pub name_with_owner: String,
    pub owner: RepoOwner,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RepoOwner {
    /// `User` or `Organization`.
    #[serde(rename = "__typename")]
    pub kind: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackRef {
    pub number: PrNumber,
    /// The branch the whole stack merges into. A layer's own base is the layer below it.
    pub base_ref_name: String,
}

/// What the dashboard shows of a PR, as GraphQL returns it. The queue reads only its approvals,
/// for the code owners.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrDetails {
    url: String,
    created_at: String,
    additions: u64,
    deletions: u64,
    changed_files: u64,
    author: Option<Actor>,
    #[serde(deserialize_with = "nodes")]
    latest_opinionated_reviews: Vec<ReviewNode>,
    /// A team asked to review comes back as `{}`: the App cannot read teams.
    #[serde(deserialize_with = "nodes")]
    review_requests: Vec<Value>,
    /// The last commit's `statusCheckRollup`.
    commits: Value,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Actor {
    login: String,
    avatar_url: String,
    /// `User` or `Bot`. A requested reviewer comes without it, and is always a user.
    #[serde(rename = "__typename", default)]
    typename: String,
}

impl From<Actor> for Person {
    fn from(a: Actor) -> Self {
        Person {
            login: a.login,
            avatar_url: a.avatar_url,
            bot: a.typename == "Bot",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct ReviewNode {
    state: String,
    author: Option<Actor>,
}

impl PullRequest {
    pub fn has_label(&self, label: &str) -> bool {
        self.labels.iter().any(|l| l == label)
    }

    /// Notes whether the trunk's code owners, if its rules require their review, must still
    /// approve.
    fn check_code_owners(&mut self, owners: Option<&CodeOwners>) {
        let approvers: Vec<&str> = self
            .details
            .latest_opinionated_reviews
            .iter()
            .filter(|r| r.state == "APPROVED")
            .filter_map(|r| Some(r.author.as_ref()?.login.as_str()))
            .collect();
        self.awaits_code_owner = owners.is_some_and(|o| o.awaits_review(&self.files, &approvers));
    }

    /// `reviewDecision`, with the code owners' review it leaves out.
    fn review_decision(&self) -> Option<&str> {
        match self.review_decision.as_deref() {
            None if self.awaits_code_owner => Some("REVIEW_REQUIRED"),
            decision => decision,
        }
    }

    /// Why GitHub would not merge this PR yet, if it would not.
    pub fn hold_reason(&self) -> Option<String> {
        if self.is_draft {
            return Some("the PR is a draft".into());
        }
        match self.review_decision() {
            Some("REVIEW_REQUIRED") => Some("waiting for a required review".into()),
            Some("CHANGES_REQUESTED") => {
                Some("a reviewer requested changes, and their approval is required".into())
            }
            _ => None,
        }
    }

    pub fn summary(&self) -> PrSummary {
        let d = self.details.clone();
        PrSummary {
            title: self.title.clone(),
            url: d.url,
            author: d.author.map(Person::from),
            created_at: d.created_at,
            head_ref: self.head_ref_name.clone(),
            fork: self.is_cross_repository,
            additions: d.additions,
            deletions: d.deletions,
            changed_files: d.changed_files,
            review_decision: self.review_decision().map(str::to_owned),
            reviews: d
                .latest_opinionated_reviews
                .into_iter()
                .filter_map(|r| {
                    Some(Review {
                        reviewer: r.author?.into(),
                        state: r.state,
                    })
                })
                .collect(),
            requested: d
                .review_requests
                .into_iter()
                .filter_map(|r| {
                    serde_json::from_value::<Actor>(r["requestedReviewer"].clone()).ok()
                })
                .map(Person::from)
                .collect(),
            checks: d.commits["nodes"][0]["commit"]["statusCheckRollup"]["state"]
                .as_str()
                .map(str::to_owned),
        }
    }
}

fn nodes<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Vec<T>, D::Error> {
    #[derive(Deserialize)]
    struct Conn<T> {
        nodes: Vec<T>,
    }
    Ok(Conn::deserialize(d)?.nodes)
}

fn file_paths<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    struct File {
        path: String,
    }
    Ok(nodes::<D, File>(d)?.into_iter().map(|f| f.path).collect())
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

/// How often to ask for the result of an asynchronous merge. A stack of three took 10 seconds.
const MERGE_POLL: std::time::Duration = std::time::Duration::from_secs(2);

/// GitHub's reason for not merging, from an error body or a failed result.
fn rejection(body: &Value) -> String {
    body["details"]["message"]
        .as_str()
        .or(body["message"].as_str())
        .unwrap_or("not mergeable")
        .to_owned()
}

/// What a PR shows on the dashboard. A macro, so `concat!` can build on it.
macro_rules! summary_fields {
    () => {
        "number title body isDraft isCrossRepository reviewDecision baseRefName headRefName headRefOid labels(first: 100) { nodes { name } } stack { number baseRefName }
  maintainerCanModify headRepository { nameWithOwner owner { __typename } }
  url createdAt additions deletions changedFiles author { __typename login avatarUrl }
  latestOpinionatedReviews(first: 20) { nodes { state author { login avatarUrl } } }
  reviewRequests(first: 20) { nodes { requestedReviewer { ... on User { login avatarUrl } } } }
  commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }"
    };
}

/// A PR as the queue reads it: its files too, for the code owners.
const PR_FIELDS: &str = concat!(summary_fields!(), " files(first: 100) { nodes { path } }");

/// The labeled PRs a snapshot reads at a time. GraphQL charges a point per 100 requests its lists
/// could take, one per list on each PR: a page of 25 PRs costs 1 point, of 100 5, on every tick.
const SNAPSHOT_PAGE: u32 = 25;

impl AsUser {
    /// The open PRs into `base`, up to `limit`, updated last first. Their code owners are not
    /// checked, nor their files read: `review_decision` leaves out a code owner's review it
    /// requires.
    pub async fn open_pull_requests(
        &self,
        repo: &RepoName,
        base: &str,
        limit: u32,
    ) -> Result<Vec<PullRequest>> {
        let query = format!(
            "query($owner: String!, $name: String!, $base: String!, $limit: Int!) {{
              repository(owner: $owner, name: $name) {{
                pullRequests(states: OPEN, baseRefName: $base, first: $limit, orderBy: {{ field: UPDATED_AT, direction: DESC }}) {{
                  nodes {{ {} }}
                }}
              }}
            }}",
            summary_fields!()
        );
        let data: Value = self
            .graphql(
                "open pull requests",
                &query,
                json!({ "owner": repo.owner, "name": repo.name, "base": base, "limit": limit }),
            )
            .await?;
        Ok(serde_json::from_value(
            data["repository"]["pullRequests"]["nodes"].clone(),
        )?)
    }
}

impl Installation {
    pub async fn snapshot(&self, repo: &RepoName, labels: &Labels) -> Result<RepoSnapshot> {
        let query = format!(
            "query($owner: String!, $name: String!, $label: String!, $priority: String!, $failed: String!, $page: Int!, $after: String) {{
              repository(owner: $owner, name: $name) {{
                id
                isPrivate
                trigger: label(name: $label) {{ name }}
                priority: label(name: $priority) {{ name }}
                failed: label(name: $failed) {{ name }}
                viewerDefaultMergeMethod
                defaultBranchRef {{ name target {{ oid }} }}
                {code_owners}
                pullRequests(states: OPEN, labels: [$label], first: $page, after: $after, orderBy: {{ field: CREATED_AT, direction: ASC }}) {{
                  nodes {{ {PR_FIELDS} }}
                  pageInfo {{ hasNextPage endCursor }}
                }}
              }}
            }}",
            code_owners = codeowners::FIELDS,
        );
        // The labeled PRs come a page at a time. The rest of the repo is the same on every page: the
        // first page's counts.
        let mut first: Option<Value> = None;
        let mut labeled: Vec<PullRequest> = Vec::new();
        let mut after = Value::Null;
        loop {
            let data: Value = self
                .graphql(
                    "snapshot",
                    &query,
                    json!({
                        "owner": repo.owner,
                        "name": repo.name,
                        "label": labels.trigger,
                        "priority": labels.priority,
                        "failed": labels.failed,
                        "page": SNAPSHOT_PAGE,
                        "after": after,
                    }),
                )
                .await?;
            let prs = &data["repository"]["pullRequests"];
            let page: Vec<PullRequest> = serde_json::from_value(prs["nodes"].clone())?;
            labeled.extend(page);
            let next = &prs["pageInfo"];
            let more = next["hasNextPage"] == true;
            after = next["endCursor"].clone();
            first.get_or_insert(data);
            if !more || after.is_null() {
                break;
            }
        }
        let data = first.expect("one page at least");
        let r = &data["repository"];
        let missing_labels = [
            ("trigger", &labels.trigger),
            ("priority", &labels.priority),
            ("failed", &labels.failed),
        ]
        .into_iter()
        .filter(|(alias, _)| r[alias].is_null())
        .map(|(_, name)| name.clone())
        .collect();
        let owners = CodeOwners::from_repository(r);
        for pr in &mut labeled {
            pr.check_code_owners(owners.as_ref());
        }
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
            private: r["isPrivate"].as_bool().unwrap_or(true),
            merge_method: r["viewerDefaultMergeMethod"]
                .as_str()
                .unwrap_or("MERGE")
                .to_lowercase(),
            labeled,
            missing_labels,
        })
    }

    pub async fn pull_request(&self, repo: &RepoName, number: PrNumber) -> Result<PullRequest> {
        let query = format!(
            "query($owner: String!, $name: String!, $number: Int!) {{
              repository(owner: $owner, name: $name) {{
                pullRequest(number: $number) {{ {PR_FIELDS} }}
                {code_owners}
              }}
            }}",
            code_owners = codeowners::FIELDS,
        );
        let data: Value = self
            .graphql(
                "pull request",
                &query,
                json!({ "owner": repo.owner, "name": repo.name, "number": number }),
            )
            .await?;
        let r = &data["repository"];
        let mut pr: PullRequest = serde_json::from_value(r["pullRequest"].clone())?;
        pr.check_code_owners(CodeOwners::from_repository(r).as_ref());
        Ok(pr)
    }

    /// The open PRs of the stack, bottom to top. Its merged layers are left out.
    pub async fn stack_layers(&self, repo: &RepoName, stack: PrNumber) -> Result<Vec<PrNumber>> {
        let r: Value = self
            .rest(Method::GET, &format!("/repos/{repo}/stacks/{stack}"), None)
            .await?;
        Ok(r["pull_requests"]
            .as_array()
            .context("stack pull_requests")?
            .iter()
            .filter(|p| p["state"] == "open")
            .filter_map(|p| p["number"].as_u64())
            .collect())
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

    /// The head of PR `number`, from its `refs/pull/<n>/head`: where a fork PR's head shows in
    /// the base repo.
    pub async fn pull_head(&self, repo: &RepoName, number: PrNumber) -> Result<String> {
        let r: Value = self
            .rest(
                Method::GET,
                &format!("/repos/{repo}/git/ref/pull/{number}/head"),
                None,
            )
            .await?;
        Ok(r["object"]["sha"].as_str().context("ref sha")?.to_owned())
    }

    /// Whether `head` lacks commits of `base`: it needs a rebase onto it.
    pub async fn behind(&self, repo: &RepoName, base: &str, head: &str) -> Result<bool> {
        let r: Value = self
            .rest(
                Method::GET,
                &format!("/repos/{repo}/compare/{base}...{head}"),
                None,
            )
            .await?;
        Ok(r["behind_by"].as_u64().context("behind_by")? > 0)
    }

    /// Whether workflow runs on `sha` wait for a maintainer to approve them, as GitHub has a
    /// fork PR's from an outside contributor.
    pub async fn awaiting_approval(&self, repo: &RepoName, sha: &str) -> Result<bool> {
        let r: Value = self
            .rest(
                Method::GET,
                &format!(
                    "/repos/{repo}/actions/runs?head_sha={sha}&status=action_required&per_page=1"
                ),
                None,
            )
            .await?;
        Ok(r["total_count"].as_u64().unwrap_or(0) > 0)
    }

    /// Whether PR `number` changes a file under `dir`.
    pub async fn changes_under(
        &self,
        repo: &RepoName,
        number: PrNumber,
        dir: &str,
    ) -> Result<bool> {
        // GitHub lists at most 3000 files, 100 a page.
        for page in 1..=30 {
            let files: Value = self
                .rest(
                    Method::GET,
                    &format!("/repos/{repo}/pulls/{number}/files?per_page=100&page={page}"),
                    None,
                )
                .await?;
            let files = files.as_array().context("PR files")?;
            let under = |f: &Value| {
                [&f["filename"], &f["previous_filename"]]
                    .iter()
                    .any(|n| n.as_str().is_some_and(|n| n.starts_with(dir)))
            };
            if files.iter().any(under) {
                return Ok(true);
            }
            if files.len() < 100 {
                break;
            }
        }
        Ok(false)
    }

    /// Creates `name`, a full ref outside `refs/heads/`, so that no workflow runs on it.
    pub async fn create_ref(&self, repo: &RepoName, name: &str, sha: &str) -> Result<()> {
        let _: Value = self
            .rest(
                Method::POST,
                &format!("/repos/{repo}/git/refs"),
                Some(json!({ "ref": name, "sha": sha })),
            )
            .await?;
        Ok(())
    }

    /// Deletes the full ref `name`, if it is there.
    pub async fn delete_ref(&self, repo: &RepoName, name: &str) -> Result<()> {
        let name = name.strip_prefix("refs/").context("a full ref")?;
        let res = self
            .request(
                Method::DELETE,
                &format!("/repos/{repo}/git/refs/{name}"),
                None,
            )
            .await?;
        if !res.status.is_success()
            && res.status != StatusCode::NOT_FOUND
            && res.status != StatusCode::UNPROCESSABLE_ENTITY
        {
            anyhow::bail!("delete ref {name}: {} {}", res.status, res.body);
        }
        Ok(())
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
    pub async fn force_update_branch(
        &self,
        repo_id: &str,
        branch: &str,
        before: &str,
        after: &str,
    ) -> Result<()> {
        self.force_update_branches(repo_id, &[(branch, before, after)])
            .await
    }

    /// Moves each `(branch, before, after)`, all or none: `updateRefs` is atomic. It fails if a
    /// branch is not at its `before`, also one whose `after` is the same.
    pub async fn force_update_branches(
        &self,
        repo_id: &str,
        moves: &[(&str, &str, &str)],
    ) -> Result<()> {
        let query = "mutation($repo: ID!, $updates: [RefUpdate!]!) {
            updateRefs(input: { repositoryId: $repo, refUpdates: $updates }) { clientMutationId }
        }";
        let updates: Vec<Value> = moves
            .iter()
            .map(|(branch, before, after)| {
                json!({ "name": format!("refs/heads/{branch}"), "beforeOid": before, "afterOid": after, "force": true })
            })
            .collect();
        let _: Value = self
            .graphql(
                "update refs",
                query,
                json!({ "repo": repo_id, "updates": updates }),
            )
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

    /// The fix agent's commit `sha` as `git diff` prints it. `None` for any other commit, one
    /// without the `FIXED_BY` trailer, or for no commit at all.
    pub async fn fix_patch(&self, repo: &RepoName, sha: &str) -> Result<Option<String>> {
        let res = self
            .request(
                Method::GET,
                &format!("/repos/{repo}/git/commits/{sha}"),
                None,
            )
            .await?;
        if matches!(
            res.status,
            StatusCode::NOT_FOUND | StatusCode::UNPROCESSABLE_ENTITY
        ) {
            return Ok(None);
        }
        if !res.status.is_success() {
            anyhow::bail!("get commit {sha}: {} {}", res.status, res.body);
        }
        let message = res.body["message"].as_str().context("commit message")?;
        if !message.lines().any(|l| l.starts_with(FIXED_BY)) {
            return Ok(None);
        }
        let path = format!("/repos/{repo}/commits/{sha}");
        let (status, diff) = self.get_text(&path, "application/vnd.github.diff").await?;
        if !status.is_success() {
            anyhow::bail!("get the diff of {sha}: {status} {diff}");
        }
        Ok(Some(diff))
    }

    /// Merges the PR through the asynchronous merge API, the only one GitHub offers for a stacked
    /// PR: for one, it merges every layer up to `number`, or none. Waits for the result. A merge
    /// already pending for the PR, from before a restart, is waited for rather than requested
    /// again.
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
                &format!("/repos/{repo}/pulls/{number}/merge-async"),
                Some(json!({ "sha": sha, "merge_method": method, "merge_action": "direct_merge" })),
            )
            .await;
        // A merge can go through though its reply says otherwise: lost in a 502, or a retry that
        // found it merged already.
        let answered = matches!(&res, Ok(r) if matches!(r.status, StatusCode::OK | StatusCode::ACCEPTED | StatusCode::CONFLICT));
        if !answered && let Some(sha) = self.merged_as(repo, number, sha).await? {
            return Ok(MergeOutcome::Merged { sha });
        }
        let res = res?;
        let mut result = match res.status {
            StatusCode::OK | StatusCode::ACCEPTED | StatusCode::CONFLICT => res.body,
            StatusCode::BAD_REQUEST
            | StatusCode::METHOD_NOT_ALLOWED
            | StatusCode::UNPROCESSABLE_ENTITY => {
                return Ok(MergeOutcome::Rejected(rejection(&res.body)));
            }
            s => anyhow::bail!("merge #{number}: {s} {}", res.body),
        };
        loop {
            match result["status"].as_str() {
                Some("merged") => {
                    let sha = match result["details"]["sha"].as_str() {
                        Some(sha) => sha.to_owned(),
                        // Merged before this request: the PR has its merge commit.
                        None => self.merge_commit(repo, number).await?,
                    };
                    return Ok(MergeOutcome::Merged { sha });
                }
                Some("failed") => return Ok(MergeOutcome::Rejected(rejection(&result))),
                // `direct_merge` never queues, so this is not a merge Mergepatrol asked for.
                Some("enqueued") => {
                    return Ok(MergeOutcome::Rejected(
                        "the PR is in GitHub's merge queue".into(),
                    ));
                }
                Some("pending") => {}
                _ => anyhow::bail!("merge #{number}: unexpected result {result}"),
            }
            let uuid = result["details"]["uuid"]
                .as_str()
                .context("pending merge uuid")?
                .to_owned();
            tokio::time::sleep(MERGE_POLL).await;
            result = self
                .rest(
                    Method::GET,
                    &format!("/repos/{repo}/pulls/{number}/merge-async/{uuid}"),
                    None,
                )
                .await?;
        }
    }

    async fn merge_commit(&self, repo: &RepoName, number: PrNumber) -> Result<String> {
        let pr: Value = self
            .rest(Method::GET, &format!("/repos/{repo}/pulls/{number}"), None)
            .await?;
        Ok(pr["merge_commit_sha"]
            .as_str()
            .context("merge commit")?
            .to_owned())
    }

    /// The merge commit of the PR, if it merged at `head`.
    pub async fn merged_as(
        &self,
        repo: &RepoName,
        number: PrNumber,
        head: &str,
    ) -> Result<Option<String>> {
        let pr: Value = self
            .rest(Method::GET, &format!("/repos/{repo}/pulls/{number}"), None)
            .await?;
        Ok((pr["merged"] == true && pr["head"]["sha"] == head)
            .then(|| pr["merge_commit_sha"].as_str().map(str::to_owned))
            .flatten())
    }

    /// Creates the label, unless the repo already has it.
    pub async fn create_label(&self, repo: &RepoName, label: &LabelSpec<'_>) -> Result<()> {
        let res = self
            .request(
                Method::POST,
                &format!("/repos/{repo}/labels"),
                Some(json!({
                    "name": label.name,
                    "color": label.color,
                    "description": label.description,
                })),
            )
            .await?;
        let exists = res.status == StatusCode::UNPROCESSABLE_ENTITY
            && res.body["errors"][0]["code"] == "already_exists";
        if !res.status.is_success() && !exists {
            anyhow::bail!("create label {}: {} {}", label.name, res.status, res.body);
        }
        Ok(())
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
        // Page by page, oldest first, until ours: a busy PR has more than a page.
        let mut mine = None;
        for page in 1.. {
            let comments: Vec<Value> = self
                .rest(
                    Method::GET,
                    &format!("/repos/{repo}/issues/{number}/comments?per_page=100&page={page}"),
                    None,
                )
                .await?;
            mine = comments
                .iter()
                .find(|c| {
                    c["user"]["type"] == "Bot"
                        && c["body"]
                            .as_str()
                            .is_some_and(|b| b.starts_with(STICKY_MARKER))
                })
                .and_then(|c| c["id"].as_u64());
            if mine.is_some() || comments.len() < 100 {
                break;
            }
        }
        match mine {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::fake;

    const NOT_MERGEABLE: &str = r#"{"message":"Pull Request is not mergeable"}"#;

    fn leak(s: String) -> &'static str {
        Box::leak(s.into_boxed_str())
    }

    /// A labeled PR as the snapshot query returns it.
    fn pr_node(number: u64) -> String {
        format!(
            r#"{{"number":{number},"title":"t","body":"","isDraft":false,"isCrossRepository":false,"reviewDecision":null,"baseRefName":"main","headRefName":"b{number}","headRefOid":"h{number}","labels":{{"nodes":[{{"name":"mp"}}]}},"files":{{"nodes":[]}},"stack":null,"url":"u","createdAt":"2026-10-01T00:00:00Z","additions":0,"deletions":0,"changedFiles":0,"author":null,"latestOpinionatedReviews":{{"nodes":[]}},"reviewRequests":{{"nodes":[]}},"commits":{{"nodes":[]}}}}"#
        )
    }

    fn snapshot_page(numbers: std::ops::Range<u64>, next: Option<&str>) -> &'static str {
        let nodes: Vec<String> = numbers.map(pr_node).collect();
        let page_info = match next {
            Some(cursor) => format!(r#"{{"hasNextPage":true,"endCursor":"{cursor}"}}"#),
            None => r#"{"hasNextPage":false,"endCursor":null}"#.into(),
        };
        leak(format!(
            r#"{{"data":{{"repository":{{"id":"R","trigger":{{"name":"mp"}},"priority":null,"failed":{{"name":"mp:failed"}},"viewerDefaultMergeMethod":"SQUASH","defaultBranchRef":{{"name":"main","target":{{"oid":"t"}}}},"pullRequests":{{"nodes":[{}],"pageInfo":{page_info}}}}}}}}}"#,
            nodes.join(",")
        ))
    }

    #[tokio::test]
    async fn the_snapshot_reads_every_page_of_labeled_prs() {
        let (gh, requests) = fake::serve(vec![
            (200, "", snapshot_page(1..101, Some("c1"))),
            (200, "", snapshot_page(101..103, None)),
        ])
        .await;
        let repo = RepoName::parse("o/r").unwrap();
        let labels = Labels {
            trigger: "mp".into(),
            priority: "mp:priority".into(),
            failed: "mp:failed".into(),
        };
        let snap = gh.snapshot(&repo, &labels).await.unwrap();
        let numbers: Vec<u64> = snap.labeled.iter().map(|p| p.number).collect();
        assert_eq!(numbers, (1..103).collect::<Vec<_>>());
        assert_eq!(snap.missing_labels, ["mp:priority"]);
        assert_eq!(requests.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn the_sticky_comment_is_found_past_the_first_page() {
        let others: Vec<String> = (0..100)
            .map(|i| format!(r#"{{"id":{i},"user":{{"type":"User"}},"body":"lgtm"}}"#))
            .collect();
        let mine =
            format!(r#"[{{"id":4242,"user":{{"type":"Bot"}},"body":"{STICKY_MARKER}\nold"}}]"#);
        let (gh, requests) = fake::serve(vec![
            (200, "", leak(format!("[{}]", others.join(",")))),
            (200, "", leak(mine)),
            (200, "", "{}"),
        ])
        .await;
        let repo = RepoName::parse("o/r").unwrap();
        gh.sticky_comment(&repo, 7, "new").await.unwrap();
        assert_eq!(
            *requests.lock().unwrap(),
            [
                "GET /repos/o/r/issues/7/comments?per_page=100&page=1 HTTP/1.1",
                "GET /repos/o/r/issues/7/comments?per_page=100&page=2 HTTP/1.1",
                "PATCH /repos/o/r/issues/comments/4242 HTTP/1.1",
            ]
        );
    }

    async fn merge_answered(pr: &'static str) -> (MergeOutcome, Vec<String>) {
        let (gh, requests) = fake::serve(vec![(405, "", NOT_MERGEABLE), (200, "", pr)]).await;
        let repo = RepoName::parse("o/r").unwrap();
        let outcome = gh.merge(&repo, 7, "abc", "squash").await.unwrap();
        let requests = requests.lock().unwrap().clone();
        (outcome, requests)
    }

    #[tokio::test]
    async fn a_merge_that_went_through_counts_though_its_reply_failed() {
        let pr = r#"{"merged":true,"head":{"sha":"abc"},"merge_commit_sha":"m1"}"#;
        let (outcome, requests) = merge_answered(pr).await;
        assert!(
            matches!(&outcome, MergeOutcome::Merged { sha } if sha == "m1"),
            "{outcome:?}"
        );
        assert_eq!(
            requests,
            [
                "PUT /repos/o/r/pulls/7/merge-async HTTP/1.1",
                "GET /repos/o/r/pulls/7 HTTP/1.1"
            ]
        );
    }

    #[tokio::test]
    async fn a_rejected_merge_stays_rejected() {
        let pr = r#"{"merged":false,"head":{"sha":"abc"},"merge_commit_sha":null}"#;
        let (outcome, _) = merge_answered(pr).await;
        assert!(
            matches!(&outcome, MergeOutcome::Rejected(m) if m.contains("not mergeable")),
            "{outcome:?}"
        );
    }

    /// nebula#11186 before its approval: `reviewDecision` is empty, though `main` requires a code
    /// owner's review and the PR changes an owned file.
    const UNREVIEWED: &str = r#"{"data":{"repository":{
        "pullRequest":{"number":11186,"title":"ci","body":"","isDraft":false,"isCrossRepository":false,
            "reviewDecision":null,"baseRefName":"main","headRefName":"ci","headRefOid":"abc",
            "labels":{"nodes":[]},"files":{"nodes":[{"path":".github/dependabot.yml"}]},
            "url":"","createdAt":"","additions":1,"deletions":0,"changedFiles":1,"author":null,
            "latestOpinionatedReviews":{"nodes":[]},"reviewRequests":{"nodes":[]},"commits":{"nodes":[]}},
        "defaultBranchRef":{"rules":{"nodes":[{"parameters":{"requireCodeOwnerReview":true}},{"parameters":null}]}},
        "codeowners0":{"text":"/.github/ @joaquim-verges @jnsdls\n"},"codeowners1":null,"codeowners2":null}}}"#;

    #[tokio::test]
    async fn a_pr_that_awaits_a_code_owner_is_held_though_review_decision_is_empty() {
        let (gh, _) = fake::serve(vec![(200, "", UNREVIEWED)]).await;
        let repo = RepoName::parse("o/r").unwrap();
        let pr = gh.pull_request(&repo, 11186).await.unwrap();
        assert_eq!(
            pr.hold_reason().as_deref(),
            Some("waiting for a required review")
        );
        assert_eq!(
            pr.summary().review_decision.as_deref(),
            Some("REVIEW_REQUIRED")
        );
    }

    #[test]
    fn summary_reads_graphql() {
        let pr: PullRequest = serde_json::from_value(json!({
            "number": 7, "title": "Fix it", "body": "", "isDraft": false,
            "isCrossRepository": false, "reviewDecision": "APPROVED", "baseRefName": "main",
            "headRefName": "fix", "headRefOid": "abc", "labels": { "nodes": [{ "name": "mergepatrol" }] },
            "files": { "nodes": [{ "path": "src/a.rs" }] },
            "url": "https://github.com/o/r/pull/7", "createdAt": "2026-10-01T00:00:00Z",
            "additions": 3, "deletions": 1, "changedFiles": 2,
            "author": { "__typename": "Bot", "login": "merge-patrol", "avatarUrl": "m" },
            "latestOpinionatedReviews": { "nodes": [
                { "state": "APPROVED", "author": { "login": "ann", "avatarUrl": "a" } },
                { "state": "CHANGES_REQUESTED", "author": null },
            ] },
            "reviewRequests": { "nodes": [
                { "requestedReviewer": { "login": "bob", "avatarUrl": "b" } },
                { "requestedReviewer": {} },
            ] },
            "commits": { "nodes": [{ "commit": { "statusCheckRollup": { "state": "PENDING" } } }] },
        }))
        .unwrap();
        let s = pr.summary();
        assert!(s.author.unwrap().bot);
        assert_eq!(s.head_ref, "fix");
        assert_eq!(
            s.reviews,
            [Review {
                reviewer: Person {
                    login: "ann".into(),
                    avatar_url: "a".into(),
                    bot: false,
                },
                state: "APPROVED".into(),
            }]
        );
        assert_eq!(
            s.requested,
            [Person {
                login: "bob".into(),
                avatar_url: "b".into(),
                bot: false,
            }]
        );
        assert_eq!(s.checks.as_deref(), Some("PENDING"));
    }

    const FIX_COMMIT: &str = r#"{"message":"Fix test\n\nFixed-by: mergepatrol, claude (claude-opus-5-5)\nCo-authored-by: mergepatrol <m@x>"}"#;
    const FIX_DIFF: &str = "diff --git a/src/a.rs b/src/a.rs\nindex 1..2 100644\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-a\n+b\n";

    #[tokio::test]
    async fn a_fix_commit_comes_back_as_its_diff() {
        let (gh, requests) = fake::serve(vec![(200, "", FIX_COMMIT), (200, "", FIX_DIFF)]).await;
        let repo = RepoName::parse("o/r").unwrap();
        let patch = gh.fix_patch(&repo, "abc").await.unwrap().unwrap();
        assert_eq!(patch, FIX_DIFF);
        assert_eq!(
            *requests.lock().unwrap(),
            [
                "GET /repos/o/r/git/commits/abc HTTP/1.1",
                "GET /repos/o/r/commits/abc HTTP/1.1",
            ]
        );
    }

    #[tokio::test]
    async fn only_the_fix_agents_commits_have_a_fix_patch() {
        let theirs = r#"{"message":"feat: x"}"#;
        let missing = r#"{"message":"No commit found for SHA: abc"}"#;
        let (gh, _) = fake::serve(vec![(200, "", theirs), (422, "", missing)]).await;
        let repo = RepoName::parse("o/r").unwrap();
        assert!(gh.fix_patch(&repo, "abc").await.unwrap().is_none());
        assert!(gh.fix_patch(&repo, "abc").await.unwrap().is_none());
    }
}
