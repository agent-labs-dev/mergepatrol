//! What the dashboard shows of one open PR beyond its summary: its description, labels, check
//! runs, timeline and diff. Read as the user, on their own rate limit.

use anyhow::{Context, Result, bail};
use reqwest::StatusCode;
use serde_json::{Value, json};

use mergepatrol_core::{Person, PrNumber, RepoName};

use crate::app::AsUser;

#[derive(Debug, Clone, PartialEq)]
pub struct PullDetail {
    /// The description as GitHub renders it, sanitized by GitHub.
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
    pullRequest(number: $number) {
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

impl AsUser {
    pub async fn pull_detail(&self, repo: &RepoName, number: PrNumber) -> Result<PullDetail> {
        let data: Value = self
            .graphql(
                "pull detail",
                DETAIL,
                json!({ "owner": repo.owner, "name": repo.name, "number": number }),
            )
            .await?;
        pull_detail(&data["repository"]["pullRequest"])
    }

    /// The PR's diff, or one commit's with `commit`. `None` when GitHub will not make one this
    /// large: over 300 files or 20,000 lines.
    pub async fn pull_diff(
        &self,
        repo: &RepoName,
        number: PrNumber,
        commit: Option<&str>,
    ) -> Result<Option<String>> {
        let path = match commit {
            Some(sha) => format!("/repos/{repo}/commits/{sha}"),
            None => format!("/repos/{repo}/pulls/{number}"),
        };
        let (status, diff) = self.get_text(&path, "application/vnd.github.diff").await?;
        match status {
            s if s.is_success() => Ok(Some(diff)),
            StatusCode::NOT_ACCEPTABLE => Ok(None),
            s => bail!("GET {path} as a diff: {s} {diff}"),
        }
    }
}

/// A `pullRequest` node of the [`DETAIL`] query.
fn pull_detail(pr: &Value) -> Result<PullDetail> {
    if pr.is_null() {
        bail!("no such pull request");
    }
    let nodes = |v: &Value| v["nodes"].as_array().cloned().unwrap_or_default();
    let contexts = nodes(&pr["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"]);
    Ok(PullDetail {
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
                    { "author": { "login": "merge-patrol" }, "url": "u1",
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
                    author: Some("merge-patrol".into()),
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
}
