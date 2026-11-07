//! What the dashboard shows of one open PR beyond its summary: its description, labels, check
//! runs, timeline and diff. Read as the user, on their own rate limit.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use futures_util::future::join_all;
use reqwest::{StatusCode, Url};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use mergepatrol_core::{Person, PrNumber, RepoName};

use crate::app::AsUser;

#[derive(Debug, Clone, PartialEq)]
pub struct PullDetail {
    /// The head commit's SHA.
    pub head: String,
    /// The base branch's commit, by SHA.
    pub base: String,
    /// The description as GitHub renders it, sanitized by GitHub, its images signed as
    /// [`SignedImages`] says.
    pub body_html: String,
    pub labels: Vec<Label>,
    /// The head commit's check runs and commit statuses.
    pub checks: Vec<CheckRun>,
    /// Oldest first, up to the last 100 events.
    pub timeline: Vec<TimelineItem>,
    /// Its review threads, up to the first 100, in order.
    pub threads: Vec<ThreadSummary>,
    /// All its review threads, past the 100 read.
    pub thread_count: u32,
}

/// A review thread, as the PR's comment on a line starts it.
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadSummary {
    pub resolved: bool,
    pub path: String,
    /// `None` on a whole file, or a line the PR has since changed.
    pub line: Option<u32>,
    /// Who started it. `None` for a deleted account.
    pub author: Option<String>,
    /// The first line of its first comment, as text.
    pub title: String,
    /// The first comment on GitHub.
    pub url: String,
    /// Its comments, the first too.
    pub comments: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Label {
    pub name: String,
    /// Hex, without `#`.
    pub color: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CheckRun {
    /// `workflow / job` for an Actions job, else the run's or status's own name.
    pub name: String,
    pub state: CheckRunState,
    pub url: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckRunState {
    Passed,
    Failed,
    Pending,
    Skipped,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TimelineItem {
    pub at: String,
    /// `None` for a deleted account.
    pub actor: Option<Person>,
    pub event: TimelineEvent,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TimelineEvent {
    Commit {
        sha: String,
        headline: String,
    },
    /// `state` is `APPROVED`, `CHANGES_REQUESTED`, `COMMENTED` or `DISMISSED`.
    Review {
        state: String,
        body_html: String,
    },
    Comment {
        body_html: String,
    },
    /// A user's login, or a team's name.
    ReviewRequested {
        reviewer: String,
    },
    Labeled {
        label: Label,
        added: bool,
    },
    ForcePushed {
        before: String,
        after: String,
    },
    ReadyForReview,
    ConvertedToDraft,
}

/// The PR, its head commit's checks, and its last 100 timeline events.
const DETAIL: &str = "query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    isPrivate
    pullRequest(number: $number) {
      headRefOid baseRefOid
      bodyHTML
      labels(first: 20) { nodes { name color } }
      reviewThreads(first: 100) { totalCount nodes { isResolved path line
        comments(first: 1) { totalCount nodes { author { login } bodyText url } } } }
      commits(last: 1) { nodes { commit { statusCheckRollup { contexts(first: 100) { nodes {
        __typename
        ... on CheckRun { name status conclusion detailsUrl startedAt completedAt checkSuite { workflowRun { workflow { name } } } }
        ... on StatusContext { context state targetUrl createdAt }
      } } } } } }
      timelineItems(last: 100, itemTypes: [PULL_REQUEST_COMMIT, PULL_REQUEST_REVIEW, ISSUE_COMMENT, REVIEW_REQUESTED_EVENT, LABELED_EVENT, UNLABELED_EVENT, HEAD_REF_FORCE_PUSHED_EVENT, READY_FOR_REVIEW_EVENT, CONVERT_TO_DRAFT_EVENT]) {
        nodes {
          __typename
          ... on PullRequestCommit { commit { oid messageHeadline committedDate author { name avatarUrl user { login avatarUrl } } } }
          ... on PullRequestReview { author { __typename login avatarUrl } state createdAt bodyHTML }
          ... on IssueComment { author { __typename login avatarUrl } createdAt bodyHTML }
          ... on ReviewRequestedEvent { actor { __typename login avatarUrl } createdAt requestedReviewer { __typename ... on User { login } ... on Bot { login } ... on Team { name } } }
          ... on LabeledEvent { actor { __typename login avatarUrl } createdAt label { name color } }
          ... on UnlabeledEvent { actor { __typename login avatarUrl } createdAt label { name color } }
          ... on HeadRefForcePushedEvent { actor { __typename login avatarUrl } createdAt beforeCommit { oid } afterCommit { oid } }
          ... on ReadyForReviewEvent { actor { __typename login avatarUrl } createdAt }
          ... on ConvertToDraftEvent { actor { __typename login avatarUrl } createdAt }
        }
      }
    }
  }
}";

/// How long a signed URL, or that an image could not be signed, is kept.
const SIGNED_FOR: Duration = Duration::from_secs(5 * 60);

/// The hash of a viewer's token, and an image's `src`.
type Image = (String, String);

/// The signed URLs of a private repo's images, by viewer, so each image costs a viewer one
/// signing per [`SIGNED_FOR`], not one per read of the PR. Their `github.com` URLs need the
/// session cookie, which an `<img>` on another site does not send, so GitHub answers 404 there,
/// though the same link opens.
pub struct SignedImages {
    /// The longest a read of the PR waits on signing. Past it, its images stay as they were.
    within: Duration,
    /// `None`: it could not be signed.
    urls: Mutex<HashMap<Image, (Instant, Option<String>)>>,
}

impl Default for SignedImages {
    fn default() -> Self {
        Self::within(Duration::from_secs(5))
    }
}

impl SignedImages {
    pub fn within(within: Duration) -> Self {
        Self {
            within,
            urls: Mutex::default(),
        }
    }

    /// `Some(None)` for an image that could not be signed lately.
    fn get(&self, key: &Image) -> Option<Option<String>> {
        let urls = self.urls.lock().unwrap();
        let (at, url) = urls.get(key)?;
        (at.elapsed() < SIGNED_FOR).then(|| url.clone())
    }

    fn put(&self, keys: impl IntoIterator<Item = (Image, Option<String>)>) {
        let mut urls = self.urls.lock().unwrap();
        urls.retain(|_, (at, _)| at.elapsed() < SIGNED_FOR);
        urls.extend(keys.into_iter().map(|(k, url)| (k, (Instant::now(), url))));
    }
}

impl AsUser {
    pub async fn pull_detail(
        &self,
        repo: &RepoName,
        number: PrNumber,
        signed: &SignedImages,
    ) -> Result<PullDetail> {
        let data: Value = self
            .graphql(
                "pull detail",
                DETAIL,
                json!({ "owner": repo.owner, "name": repo.name, "number": number }),
            )
            .await?;
        let mut detail = pull_detail(&data["repository"]["pullRequest"])?;
        if data["repository"]["isPrivate"].as_bool() == Some(true) {
            self.sign_images(repo, &mut detail, signed).await;
        }
        Ok(detail)
    }

    /// Points the images of a private repo's own files at GitHub's signed URLs for them. An image
    /// that cannot be signed in time stays as it was.
    async fn sign_images(&self, repo: &RepoName, detail: &mut PullDetail, signed: &SignedImages) {
        let comments = detail
            .timeline
            .iter_mut()
            .filter_map(|t| match &mut t.event {
                TimelineEvent::Review { body_html, .. } | TimelineEvent::Comment { body_html } => {
                    Some(body_html)
                }
                _ => None,
            });
        let mut bodies: Vec<&mut String> = std::iter::once(&mut detail.body_html)
            .chain(comments)
            .collect();
        let mut images: Vec<String> = bodies.iter().flat_map(|b| repo_images(b, repo)).collect();
        images.sort();
        images.dedup();
        let viewer: String = Sha256::digest(self.token.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let keys: Vec<_> = images
            .into_iter()
            .map(|src| (viewer.clone(), src))
            .collect();
        let kept: Vec<_> = keys.iter().map(|k| signed.get(k)).collect();
        let unsigned: Vec<_> = keys
            .iter()
            .zip(&kept)
            .filter(|(_, u)| u.is_none())
            .collect();
        let reads = join_all(
            unsigned
                .iter()
                .map(|((_, src), _)| self.signed_url(repo, src)),
        );
        // Out of time, as when the rate limit is used up: unsigned until it is read again.
        let read = tokio::time::timeout(signed.within, reads)
            .await
            .unwrap_or_else(|_| vec![None; unsigned.len()]);
        let read: Vec<_> = unsigned
            .iter()
            .map(|(k, _)| (*k).clone())
            .zip(read)
            .collect();
        signed.put(read.clone());
        let read: HashMap<_, _> = read.into_iter().collect();
        for (key, kept) in keys.iter().zip(kept) {
            let Some(url) = kept.or_else(|| read.get(key).cloned()).flatten() else {
                continue;
            };
            let src = &key.1;
            for body in &mut bodies {
                **body = body.replace(&format!(" src=\"{src}\""), &format!(" src=\"{url}\""));
            }
        }
    }

    /// The signed URL of the file `src` shows, which the contents API gives for a private repo.
    async fn signed_url(&self, repo: &RepoName, src: &str) -> Option<String> {
        for (git_ref, path) in ref_and_path(repo, src) {
            let api = format!("/repos/{repo}/contents/{path}?ref={git_ref}");
            let (status, body) = match self.get_text(&api, "application/vnd.github+json").await {
                Ok(res) => res,
                Err(e) => {
                    tracing::warn!("GET {api}: {e:#}");
                    return None;
                }
            };
            match status {
                s if s.is_success() => return download_url(repo, &body),
                StatusCode::NOT_FOUND => {}
                s => {
                    tracing::warn!("GET {api}: {s}");
                    return None;
                }
            }
        }
        None
    }

    /// A PR's diff, or one commit's, as [`DiffOf`] says, from `kept` when it has it. `None` when
    /// GitHub will not make one this large: over 300 files or 20,000 lines.
    pub async fn pull_diff(
        &self,
        repo: &RepoName,
        number: PrNumber,
        of: DiffOf<'_>,
        kept: &PullDiffs,
    ) -> Result<Arc<Option<String>>> {
        let key = of.key(number).map(|of| Diff {
            viewer: Sha256::digest(self.token.as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
            repo: repo.clone(),
            of,
        });
        if let Some(diff) = key.as_ref().and_then(|k| kept.get(k)) {
            return Ok(diff);
        }
        let path = match of {
            DiffOf::Commit(sha) => format!("/repos/{repo}/commits/{sha}"),
            DiffOf::Pull(_) => format!("/repos/{repo}/pulls/{number}"),
        };
        let (status, diff) = self.get_text(&path, "application/vnd.github.diff").await?;
        let diff = Arc::new(match status {
            s if s.is_success() => Some(diff),
            StatusCode::NOT_ACCEPTABLE => None,
            s => bail!("GET {path} as a diff: {s} {diff}"),
        });
        if let Some(key) = key {
            kept.put(key, diff.clone());
        }
        Ok(diff)
    }
}

/// Which diff of a PR.
#[derive(Debug, Clone, Copy)]
pub enum DiffOf<'a> {
    /// The whole PR. With its head and base commits as [`PullDetail`] has them, it is kept: a
    /// push or a moved base makes another. Without, it is read every time.
    Pull(Option<(&'a str, &'a str)>),
    /// One of its commits, by SHA, which never changes.
    Commit(&'a str),
}

impl DiffOf<'_> {
    fn key(self, number: PrNumber) -> Option<Of> {
        match self {
            Self::Pull(Some((head, base))) => Some(Of::Pull {
                number,
                head: head.into(),
                base: base.into(),
            }),
            Self::Pull(None) => None,
            Self::Commit(sha) => Some(Of::Commit(sha.into())),
        }
    }
}

/// How long a diff is kept. What it is of never changes, so this bounds only how long a viewer
/// who loses access to the repo still reads what they read before.
const DIFF_KEPT_FOR: Duration = Duration::from_secs(15 * 60);
/// The most patch text kept, all viewers together. The oldest go first.
const DIFFS_MAX_BYTES: usize = 64 << 20;
/// A diff larger than this is not kept, so one cannot push out all the others.
const DIFF_MAX_BYTES: usize = DIFFS_MAX_BYTES / 8;

/// A diff, by viewer: the hash of their token. Each viewer reads only what GitHub gave them.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Diff {
    viewer: String,
    repo: RepoName,
    of: Of,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Of {
    Pull {
        number: PrNumber,
        head: String,
        base: String,
    },
    Commit(String),
}

/// The diffs viewers read lately, so a PR's Code tab opened again, or another of its commits
/// looked at and back, reads GitHub once.
#[derive(Default)]
pub struct PullDiffs(Mutex<Kept>);

#[derive(Default)]
struct Kept {
    diffs: HashMap<Diff, (Instant, Arc<Option<String>>)>,
    /// Of the patches in `diffs`.
    bytes: usize,
}

fn size(diff: &Option<String>) -> usize {
    diff.as_ref().map_or(0, String::len)
}

impl PullDiffs {
    fn get(&self, key: &Diff) -> Option<Arc<Option<String>>> {
        let kept = self.0.lock().unwrap();
        let (at, diff) = kept.diffs.get(key)?;
        (at.elapsed() < DIFF_KEPT_FOR).then(|| diff.clone())
    }

    fn put(&self, key: Diff, diff: Arc<Option<String>>) {
        let bytes = size(&diff);
        if bytes > DIFF_MAX_BYTES {
            return;
        }
        let mut kept = self.0.lock().unwrap();
        kept.diffs.retain(|_, (at, _)| at.elapsed() < DIFF_KEPT_FOR);
        kept.bytes = kept.diffs.values().map(|(_, d)| size(d)).sum();
        while kept.bytes + bytes > DIFFS_MAX_BYTES {
            let Some(oldest) = kept
                .diffs
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            if let Some((_, d)) = kept.diffs.remove(&oldest) {
                kept.bytes -= size(&d);
            }
        }
        kept.bytes += bytes;
        if let Some((_, d)) = kept.diffs.insert(key, (Instant::now(), diff)) {
            kept.bytes -= size(&d);
        }
    }
}

/// The `src` of each image in `html` at `repo` on `github.com`.
fn repo_images(html: &str, repo: &RepoName) -> Vec<String> {
    let start = format!(" src=\"https://github.com/{repo}/");
    html.match_indices(&start)
        .filter_map(|(i, _)| {
            let src = &html[i + " src=\"".len()..];
            Some(src[..src.find('"')?].to_owned())
        })
        .collect()
}

/// The ways to read the `blob` or `raw` URL `src` as a ref and a path, shortest ref first: a URL
/// does not say where a ref with slashes ends, and refs rarely run past three parts.
fn ref_and_path(repo: &RepoName, src: &str) -> Vec<(String, String)> {
    let Ok(url) = Url::parse(src) else {
        return Vec::new();
    };
    let file = url.path().strip_prefix(&format!("/{repo}/"));
    let file = file.and_then(|f| f.strip_prefix("blob/").or_else(|| f.strip_prefix("raw/")));
    let Some(file) = file else {
        return Vec::new();
    };
    file.match_indices('/')
        .take(3)
        .map(|(i, _)| (file[..i].to_owned(), file[i + 1..].to_owned()))
        .collect()
}

/// The `download_url` of a contents API response for a file in `repo`.
fn download_url(repo: &RepoName, body: &str) -> Option<String> {
    let file: Value = serde_json::from_str(body).ok()?;
    let url = file["download_url"].as_str()?;
    let in_repo = url.starts_with(&format!("https://raw.githubusercontent.com/{repo}/"));
    (in_repo && !url.contains('"')).then(|| url.to_owned())
}

/// A `pullRequest` node of the [`DETAIL`] query.
fn pull_detail(pr: &Value) -> Result<PullDetail> {
    if pr.is_null() {
        bail!("no such pull request");
    }
    let nodes = |v: &Value| v["nodes"].as_array().cloned().unwrap_or_default();
    let contexts = nodes(&pr["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"]);
    Ok(PullDetail {
        head: str_of(&pr["headRefOid"]),
        base: str_of(&pr["baseRefOid"]),
        body_html: str_of(&pr["bodyHTML"]),
        labels: nodes(&pr["labels"]).iter().map(label).collect(),
        checks: contexts.iter().filter_map(check_run).collect(),
        timeline: nodes(&pr["timelineItems"])
            .iter()
            .map(timeline_item)
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect(),
        threads: nodes(&pr["reviewThreads"])
            .iter()
            .map(review_thread)
            .collect(),
        thread_count: pr["reviewThreads"]["totalCount"].as_u64().unwrap_or(0) as u32,
    })
}

fn review_thread(t: &Value) -> ThreadSummary {
    let first = &t["comments"]["nodes"][0];
    ThreadSummary {
        resolved: t["isResolved"].as_bool().unwrap_or(false),
        path: str_of(&t["path"]),
        line: t["line"].as_u64().map(|l| l as u32),
        author: opt_str(&first["author"]["login"]),
        title: thread_title(&str_of(&first["bodyText"])),
        url: str_of(&first["url"]),
        comments: t["comments"]["totalCount"].as_u64().unwrap_or(0) as u32,
    }
}

/// A comment's first line that says something, without Markdown's bold, cut to 140 chars. A
/// MergeLoop finding's first line is its label, so its title is the next one.
fn thread_title(text: &str) -> String {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let mut line = lines.next().unwrap_or_default();
    if line.starts_with("Sent back to the author") || line.starts_with("For a human") {
        line = lines.next().unwrap_or(line);
    }
    let line = line.trim_matches('*');
    match line.char_indices().nth(140) {
        Some((i, _)) => format!("{}…", &line[..i]),
        None => line.to_owned(),
    }
}

fn str_of(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_owned()
}

fn opt_str(v: &Value) -> Option<String> {
    v.as_str().map(str::to_owned)
}

fn label(v: &Value) -> Label {
    Label {
        name: str_of(&v["name"]),
        color: str_of(&v["color"]),
    }
}

fn person(v: &Value) -> Option<Person> {
    Some(Person {
        login: v["login"].as_str()?.to_owned(),
        avatar_url: str_of(&v["avatarUrl"]),
        bot: v["__typename"] == "Bot",
    })
}

/// A check run or a commit status. Neither kind of node, as from a newer API, is left out.
fn check_run(v: &Value) -> Option<CheckRun> {
    match v["__typename"].as_str()? {
        "CheckRun" => {
            let job = str_of(&v["name"]);
            let name = match v["checkSuite"]["workflowRun"]["workflow"]["name"].as_str() {
                Some(workflow) => format!("{workflow} / {job}"),
                None => job,
            };
            let state = if v["status"] != "COMPLETED" {
                CheckRunState::Pending
            } else {
                match v["conclusion"].as_str() {
                    Some("SUCCESS") => CheckRunState::Passed,
                    Some("NEUTRAL" | "SKIPPED") => CheckRunState::Skipped,
                    _ => CheckRunState::Failed,
                }
            };
            Some(CheckRun {
                name,
                state,
                url: opt_str(&v["detailsUrl"]),
                started_at: opt_str(&v["startedAt"]),
                completed_at: opt_str(&v["completedAt"]),
            })
        }
        "StatusContext" => Some(CheckRun {
            name: str_of(&v["context"]),
            state: match v["state"].as_str() {
                Some("SUCCESS") => CheckRunState::Passed,
                Some("PENDING" | "EXPECTED") => CheckRunState::Pending,
                _ => CheckRunState::Failed,
            },
            url: opt_str(&v["targetUrl"]),
            started_at: opt_str(&v["createdAt"]),
            completed_at: None,
        }),
        _ => None,
    }
}

/// A timeline node. `None` for a pending review: only its author sees it, and it may yet go.
fn timeline_item(v: &Value) -> Result<Option<TimelineItem>> {
    let kind = v["__typename"]
        .as_str()
        .context("timeline item __typename")?;
    let actor = person(&v["actor"]);
    let item = |actor, event| {
        Some(TimelineItem {
            at: str_of(&v["createdAt"]),
            actor,
            event,
        })
    };
    Ok(match kind {
        "PullRequestCommit" => {
            let c = &v["commit"];
            let a = &c["author"];
            // An author without a GitHub account still has a name and a gravatar.
            let actor = person(&a["user"]).or_else(|| {
                Some(Person {
                    login: a["name"].as_str()?.to_owned(),
                    avatar_url: str_of(&a["avatarUrl"]),
                    bot: false,
                })
            });
            Some(TimelineItem {
                at: str_of(&c["committedDate"]),
                actor,
                event: TimelineEvent::Commit {
                    sha: str_of(&c["oid"]),
                    headline: str_of(&c["messageHeadline"]),
                },
            })
        }
        "PullRequestReview" if v["state"] == "PENDING" => None,
        "PullRequestReview" => item(
            person(&v["author"]),
            TimelineEvent::Review {
                state: str_of(&v["state"]),
                body_html: str_of(&v["bodyHTML"]),
            },
        ),
        "IssueComment" => item(
            person(&v["author"]),
            TimelineEvent::Comment {
                body_html: str_of(&v["bodyHTML"]),
            },
        ),
        "ReviewRequestedEvent" => {
            let r = &v["requestedReviewer"];
            let reviewer = r["login"].as_str().or(r["name"].as_str());
            // A reviewer the user cannot see, such as a secret team, comes back empty.
            reviewer.and_then(|reviewer| {
                item(
                    actor,
                    TimelineEvent::ReviewRequested {
                        reviewer: reviewer.to_owned(),
                    },
                )
            })
        }
        "LabeledEvent" | "UnlabeledEvent" => item(
            actor,
            TimelineEvent::Labeled {
                label: label(&v["label"]),
                added: kind == "LabeledEvent",
            },
        ),
        "HeadRefForcePushedEvent" => item(
            actor,
            TimelineEvent::ForcePushed {
                before: str_of(&v["beforeCommit"]["oid"]),
                after: str_of(&v["afterCommit"]["oid"]),
            },
        ),
        "ReadyForReviewEvent" => item(actor, TimelineEvent::ReadyForReview),
        "ConvertToDraftEvent" => item(actor, TimelineEvent::ConvertedToDraft),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> RepoName {
        RepoName::parse("acme/app").unwrap()
    }

    /// A private repo's PR, its description one image of the repo's own `a.png`.
    const PRIVATE_PR: &str = r#"{ "data": { "repository": { "isPrivate": true, "pullRequest": {
        "bodyHTML": "<img src=\"https://github.com/acme/app/blob/m/a.png?raw=true\">" } } } }"#;
    const SIGNED: &str =
        r#"{ "download_url": "https://raw.githubusercontent.com/acme/app/m/a.png?token=t" }"#;

    #[tokio::test]
    async fn an_image_is_signed_once_per_viewer_while_it_keeps() {
        let (gh, requests) = crate::fake::as_user(vec![
            (200, "", PRIVATE_PR),
            (200, "", SIGNED),
            (200, "", PRIVATE_PR),
        ])
        .await;
        let signed = SignedImages::default();
        for _ in 0..2 {
            let d = gh.pull_detail(&repo(), 1, &signed).await.unwrap();
            assert_eq!(
                d.body_html,
                r#"<img src="https://raw.githubusercontent.com/acme/app/m/a.png?token=t">"#
            );
        }
        assert_eq!(
            *requests.lock().unwrap(),
            [
                "POST /graphql HTTP/1.1",
                "GET /repos/acme/app/contents/a.png?ref=m HTTP/1.1",
                "POST /graphql HTTP/1.1",
            ]
        );
    }

    #[tokio::test]
    async fn a_used_up_rate_limit_leaves_the_images_and_does_not_hold_the_pr() {
        let (gh, _) = crate::fake::as_user(vec![
            (200, "", PRIVATE_PR),
            (429, "retry-after: 60\r\n", ""),
        ])
        .await;
        let signed = SignedImages::within(Duration::from_millis(100));
        let repo = repo();
        let read = tokio::time::timeout(Duration::from_secs(5), gh.pull_detail(&repo, 1, &signed));
        let d = read.await.expect("held past its time to sign").unwrap();
        assert!(
            d.body_html
                .contains(r#"src="https://github.com/acme/app/blob/m/a.png?raw=true""#)
        );
    }

    #[test]
    fn finds_the_repos_own_images() {
        let html = r#"<a href="https://github.com/acme/app/blob/m/a.png?raw=true"><img src="https://github.com/acme/app/blob/m/a.png?raw=true" width="220"></a>
            <img src="https://github.com/acme/other/blob/m/b.png?raw=true">
            <img src="https://private-user-images.githubusercontent.com/1/c.png?jwt=x">
            <img alt="d" src="https://github.com/acme/app/raw/m/d.png">"#;
        assert_eq!(
            repo_images(html, &repo()),
            [
                "https://github.com/acme/app/blob/m/a.png?raw=true",
                "https://github.com/acme/app/raw/m/d.png",
            ]
        );
    }

    #[test]
    fn tries_each_split_of_a_ref_with_slashes() {
        let src = "https://github.com/acme/app/blob/pr-assets/one/dir/a.png?raw=true";
        let splits = ref_and_path(&repo(), src);
        let splits: Vec<_> = splits.iter().map(|(r, p)| (&**r, &**p)).collect();
        assert_eq!(
            splits,
            [
                ("pr-assets", "one/dir/a.png"),
                ("pr-assets/one", "dir/a.png"),
                ("pr-assets/one/dir", "a.png"),
            ]
        );
    }

    #[test]
    fn reads_no_file_outside_the_repo() {
        for src in [
            "https://github.com/acme/app/blob/../../../user",
            "https://github.com/acme/app/blob/%2e%2e/%2e%2e/%2e%2e/user",
            "https://github.com/acme/app/pull/1",
            "not a url",
        ] {
            assert_eq!(ref_and_path(&repo(), src), [], "{src}");
        }
    }

    #[test]
    fn takes_only_a_download_url_in_the_repo() {
        let signed = "https://raw.githubusercontent.com/acme/app/m/a.png?token=t";
        let body = |url: &str| json!({ "download_url": url }).to_string();
        assert_eq!(
            download_url(&repo(), &body(signed)).as_deref(),
            Some(signed)
        );
        assert_eq!(
            download_url(&repo(), &body("https://evil.test/a.png")),
            None
        );
        assert_eq!(download_url(&repo(), r#"[{"name":"a directory"}]"#), None);
    }

    fn user(login: &str) -> Value {
        json!({ "__typename": "User", "login": login, "avatarUrl": login })
    }

    fn ann() -> Option<Person> {
        Some(Person {
            login: "ann".into(),
            avatar_url: "ann".into(),
            bot: false,
        })
    }

    #[test]
    fn reads_review_threads_and_names_each_by_its_first_line() {
        let pr = json!({
            "reviewThreads": { "totalCount": 120, "nodes": [
                { "isResolved": false, "path": "src/a.rs", "line": 7, "comments": { "totalCount": 2, "nodes": [
                    { "author": { "login": "mergeloop" }, "url": "u1",
                      "bodyText": "Sent back to the author · f1 · other · 60% sure\n\nThe retry loop has no cap\n\nIt spins." } ] } },
                { "isResolved": true, "path": "src/b.rs", "line": null, "comments": { "totalCount": 1, "nodes": [
                    { "author": null, "url": "u2", "bodyText": "\n  Rename this  \nplease" } ] } },
            ] },
        });
        let d = pull_detail(&pr).unwrap();
        assert_eq!(d.thread_count, 120);
        assert_eq!(
            d.threads,
            [
                ThreadSummary {
                    resolved: false,
                    path: "src/a.rs".into(),
                    line: Some(7),
                    author: Some("mergeloop".into()),
                    title: "The retry loop has no cap".into(),
                    url: "u1".into(),
                    comments: 2,
                },
                ThreadSummary {
                    resolved: true,
                    path: "src/b.rs".into(),
                    line: None,
                    author: None,
                    title: "Rename this".into(),
                    url: "u2".into(),
                    comments: 1,
                },
            ]
        );
        assert_eq!(thread_title(&"x".repeat(200)).chars().count(), 141);
    }

    #[test]
    fn reads_a_pull_request() {
        let pr = json!({
            "bodyHTML": "<p>Fix it</p>",
            "labels": { "nodes": [{ "name": "bug", "color": "d73a4a" }] },
            "commits": { "nodes": [{ "commit": { "statusCheckRollup": { "contexts": { "nodes": [
                { "__typename": "CheckRun", "name": "test", "status": "COMPLETED", "conclusion": "SUCCESS",
                  "detailsUrl": "u", "startedAt": "s", "completedAt": "c",
                  "checkSuite": { "workflowRun": { "workflow": { "name": "ci" } } } },
                { "__typename": "CheckRun", "name": "lint", "status": "IN_PROGRESS", "conclusion": null,
                  "detailsUrl": null, "startedAt": "s", "completedAt": null, "checkSuite": { "workflowRun": null } },
                { "__typename": "CheckRun", "name": "e2e", "status": "COMPLETED", "conclusion": "SKIPPED",
                  "detailsUrl": null, "startedAt": null, "completedAt": null, "checkSuite": null },
                { "__typename": "CheckRun", "name": "build", "status": "COMPLETED", "conclusion": "TIMED_OUT",
                  "detailsUrl": null, "startedAt": null, "completedAt": null, "checkSuite": null },
                { "__typename": "StatusContext", "context": "deploy", "state": "PENDING", "targetUrl": "t", "createdAt": "a" },
            ] } } } }] },
            "timelineItems": { "nodes": [
                { "__typename": "PullRequestCommit", "commit": { "oid": "abc", "messageHeadline": "fix", "committedDate": "1",
                  "author": { "name": "Ann", "avatarUrl": "g", "user": { "login": "ann", "avatarUrl": "ann" } } } },
                { "__typename": "PullRequestCommit", "commit": { "oid": "def", "messageHeadline": "more", "committedDate": "2",
                  "author": { "name": "Bo", "avatarUrl": "g", "user": null } } },
                { "__typename": "ReviewRequestedEvent", "actor": user("ann"), "createdAt": "3", "requestedReviewer": null },
                { "__typename": "ReviewRequestedEvent", "actor": user("ann"), "createdAt": "4",
                  "requestedReviewer": { "__typename": "Team", "name": "core" } },
                { "__typename": "PullRequestReview", "author": user("ann"), "state": "PENDING", "createdAt": "5", "bodyHTML": "" },
                { "__typename": "PullRequestReview", "author": user("ann"), "state": "APPROVED", "createdAt": "6", "bodyHTML": "<p>ok</p>" },
                { "__typename": "IssueComment", "author": null, "createdAt": "7", "bodyHTML": "<p>hi</p>" },
                { "__typename": "UnlabeledEvent", "actor": user("ann"), "createdAt": "8", "label": { "name": "bug", "color": "d73a4a" } },
                { "__typename": "HeadRefForcePushedEvent", "actor": user("ann"), "createdAt": "9",
                  "beforeCommit": { "oid": "abc" }, "afterCommit": { "oid": "def" } },
                { "__typename": "ReadyForReviewEvent", "actor": user("ann"), "createdAt": "10" },
            ] },
        });
        let d = pull_detail(&pr).unwrap();
        assert_eq!(d.body_html, "<p>Fix it</p>");
        assert_eq!(
            d.labels,
            [Label {
                name: "bug".into(),
                color: "d73a4a".into()
            }]
        );
        let checks: Vec<_> = d
            .checks
            .iter()
            .map(|c| (c.name.as_str(), c.state))
            .collect();
        assert_eq!(
            checks,
            [
                ("ci / test", CheckRunState::Passed),
                ("lint", CheckRunState::Pending),
                ("e2e", CheckRunState::Skipped),
                ("build", CheckRunState::Failed),
                ("deploy", CheckRunState::Pending),
            ]
        );
        let events: Vec<_> = d.timeline.iter().map(|t| &t.event).collect();
        assert_eq!(
            events,
            [
                &TimelineEvent::Commit {
                    sha: "abc".into(),
                    headline: "fix".into()
                },
                &TimelineEvent::Commit {
                    sha: "def".into(),
                    headline: "more".into()
                },
                &TimelineEvent::ReviewRequested {
                    reviewer: "core".into()
                },
                &TimelineEvent::Review {
                    state: "APPROVED".into(),
                    body_html: "<p>ok</p>".into()
                },
                &TimelineEvent::Comment {
                    body_html: "<p>hi</p>".into()
                },
                &TimelineEvent::Labeled {
                    label: Label {
                        name: "bug".into(),
                        color: "d73a4a".into()
                    },
                    added: false
                },
                &TimelineEvent::ForcePushed {
                    before: "abc".into(),
                    after: "def".into()
                },
                &TimelineEvent::ReadyForReview,
            ]
        );
        assert_eq!(d.timeline[0].actor, ann());
        assert_eq!(d.timeline[0].at, "1");
        // A commit author without a GitHub account keeps their name.
        assert_eq!(d.timeline[1].actor.as_ref().unwrap().login, "Bo");
        assert_eq!(d.timeline[4].actor, None);
    }

    #[test]
    fn a_missing_pull_request_fails() {
        assert!(pull_detail(&Value::Null).is_err());
    }

    const HEAD: &str = "1111111111111111111111111111111111111111";
    const BASE: &str = "2222222222222222222222222222222222222222";

    #[tokio::test]
    async fn a_diff_is_read_once_per_viewer_while_it_keeps() {
        let (mut gh, requests) = crate::fake::as_user(vec![
            (200, "", "commit diff"),
            (200, "", "pull diff"),
            (200, "", "pushed diff"),
            (200, "", "commit diff"),
        ])
        .await;
        let kept = PullDiffs::default();
        let commit = DiffOf::Commit(HEAD);
        let pull = DiffOf::Pull(Some((HEAD, BASE)));
        let pushed = DiffOf::Pull(Some((BASE, BASE)));
        let read = async |gh: &AsUser, of| {
            gh.pull_diff(&repo(), 7, of, &kept)
                .await
                .unwrap()
                .as_deref()
                .map(str::to_owned)
        };
        for _ in 0..2 {
            assert_eq!(read(&gh, commit).await.as_deref(), Some("commit diff"));
            assert_eq!(read(&gh, pull).await.as_deref(), Some("pull diff"));
        }
        // A push is another head: GitHub again.
        assert_eq!(read(&gh, pushed).await.as_deref(), Some("pushed diff"));
        assert_eq!(read(&gh, pull).await.as_deref(), Some("pull diff"));
        // Another viewer reads it from GitHub, as themselves.
        gh.token = "u".into();
        assert_eq!(read(&gh, commit).await.as_deref(), Some("commit diff"));
        assert_eq!(
            *requests.lock().unwrap(),
            [
                format!("GET /repos/acme/app/commits/{HEAD} HTTP/1.1"),
                "GET /repos/acme/app/pulls/7 HTTP/1.1".into(),
                "GET /repos/acme/app/pulls/7 HTTP/1.1".into(),
                format!("GET /repos/acme/app/commits/{HEAD} HTTP/1.1"),
            ]
        );
    }

    #[tokio::test]
    async fn a_diff_too_large_is_kept_and_one_without_its_commits_is_not() {
        let (gh, requests) = crate::fake::as_user(vec![
            (406, "", "too large"),
            (200, "", "pull diff"),
            (200, "", "pull diff"),
        ])
        .await;
        let kept = PullDiffs::default();
        for _ in 0..2 {
            let of = DiffOf::Pull(Some((HEAD, BASE)));
            assert_eq!(*gh.pull_diff(&repo(), 7, of, &kept).await.unwrap(), None);
        }
        for _ in 0..2 {
            let diff = gh.pull_diff(&repo(), 7, DiffOf::Pull(None), &kept).await;
            assert_eq!(diff.unwrap().as_deref(), Some("pull diff"));
        }
        assert_eq!(requests.lock().unwrap().len(), 3);
    }

    #[test]
    fn the_oldest_diffs_go_past_the_bound_and_one_too_large_is_not_kept() {
        let kept = PullDiffs::default();
        let key = |sha: &str| Diff {
            viewer: "v".into(),
            repo: repo(),
            of: Of::Commit(sha.into()),
        };
        let largest = Arc::new(Some("x".repeat(DIFF_MAX_BYTES)));
        for i in 0..DIFFS_MAX_BYTES / DIFF_MAX_BYTES {
            kept.put(key(&i.to_string()), largest.clone());
        }
        assert!(kept.get(&key("0")).is_some());
        kept.put(key("last"), largest.clone());
        assert!(kept.get(&key("0")).is_none());
        assert!(kept.get(&key("1")).is_some());
        assert!(kept.get(&key("last")).is_some());
        assert_eq!(kept.0.lock().unwrap().bytes, DIFFS_MAX_BYTES);

        kept.put(key("huge"), Arc::new(Some("x".repeat(DIFF_MAX_BYTES + 1))));
        assert!(kept.get(&key("huge")).is_none());
        assert!(kept.get(&key("1")).is_some());
    }
}
