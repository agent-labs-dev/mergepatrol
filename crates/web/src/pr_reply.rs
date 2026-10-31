//! What people say on GitHub about a PR whose review waits for their answers. The dashboard is
//! where the agent's asks are answered, one by one, but people answer where the PR is. An
//! approval on GitHub, from someone who may answer, approves everything the review asks about,
//! as approving each on the dashboard would. Anything else said while the review waits, a
//! comment or a review that does not approve, cannot be read as answers: the PR is told once,
//! on GitHub, where they go, so nobody waits on a hold they think they cleared.

use anyhow::{Context, Result};
use mergepatrol_core::{Answer, PrNumber, RepoName, ReviewStatus, review_url};
use mergepatrol_github::App;
use mergepatrol_store::{ReviewAnswers, ReviewRecord};
use serde_json::Value;

use crate::AppState;
use crate::review::{ANSWERED_MARK, LINE_MARK};

/// A human's word on a PR, as a delivery brings it.
#[derive(Debug, PartialEq)]
pub(crate) struct Reply {
    pub number: PrNumber,
    pub login: String,
    pub kind: ReplyKind,
}

#[derive(Debug, PartialEq)]
pub(crate) enum ReplyKind {
    /// A review that approves the PR at `commit`.
    Approved { commit: String },
    /// A comment, or a review that does not approve.
    Said,
}

/// The reply the delivery brings, if it brings a human's. What the dashboard posts for its
/// answers is not one: the answers are stored already.
pub(crate) fn reply(event: Option<&str>, payload: &Value) -> Option<Reply> {
    let (number, said, kind) = match (event, payload["action"].as_str()) {
        (Some("pull_request_review"), Some("submitted")) => {
            let review = &payload["review"];
            let kind = match review["state"].as_str() {
                Some("approved") => ReplyKind::Approved {
                    commit: review["commit_id"].as_str()?.to_owned(),
                },
                _ => ReplyKind::Said,
            };
            (&payload["pull_request"]["number"], review, kind)
        }
        (Some("pull_request_review_comment"), Some("created")) => (
            &payload["pull_request"]["number"],
            &payload["comment"],
            ReplyKind::Said,
        ),
        (Some("issue_comment"), Some("created")) if !payload["issue"]["pull_request"].is_null() => {
            (
                &payload["issue"]["number"],
                &payload["comment"],
                ReplyKind::Said,
            )
        }
        _ => return None,
    };
    let user = &said["user"];
    if user["type"] == "Bot" {
        return None;
    }
    let body = said["body"].as_str().unwrap_or_default();
    if body.contains(ANSWERED_MARK) || body.contains(LINE_MARK) {
        return None;
    }
    Some(Reply {
        number: number.as_u64()?,
        login: user["login"].as_str()?.to_owned(),
        kind,
    })
}

/// Acts on a reply to a PR whose newest review waits for answers: approves the review for an
/// approval of its head by someone who may answer, else tells the PR where answers go.
pub(crate) async fn heard(
    app: &AppState,
    installation: u64,
    repo: &RepoName,
    reply: Reply,
) -> Result<()> {
    let Some(review) = app
        .store
        .latest_review(installation, repo, reply.number)
        .await?
    else {
        return Ok(());
    };
    if review.status != ReviewStatus::Asks {
        return Ok(());
    }
    let creds = app
        .app_credentials()
        .await?
        .context("no GitHub App is set up")?;
    let gh = App::from_credentials(&creds)?.installation(installation);
    // The dashboard takes answers only from those who may push; so does this.
    let missed = match &reply.kind {
        ReplyKind::Said => Missed::Said,
        ReplyKind::Approved { commit } if *commit != review.head_sha => Missed::OtherCommit {
            commit: commit.clone(),
        },
        ReplyKind::Approved { .. } => match gh.can_write(repo, &reply.login).await? {
            true => return approve(app, installation, repo, &review, &reply).await,
            false => Missed::NoWrite,
        },
    };
    if !app.store.nudge_review(review.id).await? {
        return Ok(());
    }
    let posted = async {
        let pr = gh.pull_request(repo, reply.number).await?;
        let url = review_url(&app.public_url, repo, pr.merges_into(), reply.number);
        let body = nudge(&review, &reply.login, &missed, &url);
        gh.comment(repo, reply.number, &body).await
    };
    if let Err(e) = posted.await {
        // Not told after all: a later reply tries again.
        app.store.release_nudge(review.id).await?;
        return Err(e);
    }
    tracing::info!(installation, %repo, number = reply.number, by = %reply.login, "told the PR to answer the agent's review in Mergepatrol");
    Ok(())
}

/// Approves the review for an approval of its head on GitHub: every ask is fine.
async fn approve(
    app: &AppState,
    installation: u64,
    repo: &RepoName,
    review: &ReviewRecord,
    reply: &Reply,
) -> Result<()> {
    let answered = ReviewAnswers {
        answers: review
            .asks()
            .map(|f| (f.finding.id.clone(), Answer::Fine))
            .collect(),
        by: reply.login.clone(),
        note: String::new(),
        status: ReviewStatus::Approved,
    };
    if app.store.answer_review(review.id, &answered).await? {
        tracing::info!(installation, %repo, number = reply.number, by = %reply.login, "approved on GitHub: the agent's review is answered");
        app.store
            .inbox_reviewed(installation, repo, reply.number, &reply.login)
            .await?;
        app.wake.wake(installation, repo.clone());
    }
    Ok(())
}

/// Why a reply did not answer the review.
enum Missed {
    /// A comment, or a review that does not approve.
    Said,
    /// An approval of another commit than the one the agent reviewed.
    OtherCommit { commit: String },
    /// An approval from someone who may not push to the repo.
    NoWrite,
}

/// What the PR is told: that its review still holds it, why the reply did not clear it, and
/// how to.
fn nudge(review: &ReviewRecord, login: &str, missed: &Missed, url: &str) -> String {
    let what = match review.asks().count() {
        // The agent could not review: a human approves the whole PR instead.
        0 => "the agent could not review it, so it waits for a human's approval".to_owned(),
        1 => "it asks a human about 1 thing".to_owned(),
        n => format!("it asks a human about {n} things"),
    };
    let how = match missed {
        Missed::Said => format!(
            "Comments on GitHub don't reach it. [Answer in Mergepatrol]({url}), or, if everything it asks about is fine as it is, approve the PR here on GitHub."
        ),
        Missed::OtherCommit { commit } => format!(
            "Your approval is of `{}`, but the agent reviewed `{}`, so it doesn't answer the review. [Answer in Mergepatrol]({url}), or approve the PR's newest commit here on GitHub.",
            short(commit),
            short(&review.head_sha),
        ),
        Missed::NoWrite => format!(
            "Your approval doesn't answer it: only someone who can push to the repo may. [Answer in Mergepatrol]({url}) if you can push, or ask someone who can to approve."
        ),
    };
    format!(
        "@{login}, Mergepatrol's review still holds this PR: {what}. {how} The PR keeps its place in the queue meanwhile."
    )
}

fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_human_reply_on_a_pr_is_read_and_the_rest_is_not() {
        let human = json!({ "login": "ann", "type": "User" });
        let bot = json!({ "login": "merge-patrol[bot]", "type": "Bot" });
        let pr = json!({ "number": 5 });
        let review = |state: &str, user: &Value, body: &str| {
            json!({ "action": "submitted", "pull_request": pr,
                    "review": { "state": state, "commit_id": "h1", "user": user, "body": body } })
        };
        let cases = [
            (
                "pull_request_review",
                review("approved", &human, ""),
                Some(ReplyKind::Approved {
                    commit: "h1".into(),
                }),
            ),
            (
                "pull_request_review",
                review("commented", &human, "q1: fine"),
                Some(ReplyKind::Said),
            ),
            (
                "pull_request_review",
                review("changes_requested", &human, "no"),
                Some(ReplyKind::Said),
            ),
            ("pull_request_review", review("approved", &bot, ""), None),
            (
                "pull_request_review",
                review(
                    "approved",
                    &human,
                    &format!("ok\n\n<sub>{ANSWERED_MARK}</sub>"),
                ),
                None,
            ),
            (
                "pull_request_review_comment",
                json!({ "action": "created", "pull_request": pr,
                        "comment": { "user": human, "body": "this is fine" } }),
                Some(ReplyKind::Said),
            ),
            (
                "pull_request_review_comment",
                json!({ "action": "created", "pull_request": pr,
                        "comment": { "user": human, "body": format!("x\n\n{LINE_MARK} t</sub>") } }),
                None,
            ),
            (
                "issue_comment",
                json!({ "action": "created", "issue": { "number": 5, "pull_request": {} },
                        "comment": { "user": human, "body": "1. yes 2. no" } }),
                Some(ReplyKind::Said),
            ),
            // An issue's comment, and an edit, are not.
            (
                "issue_comment",
                json!({ "action": "created", "issue": { "number": 5 },
                        "comment": { "user": human, "body": "hi" } }),
                None,
            ),
            (
                "issue_comment",
                json!({ "action": "edited", "issue": { "number": 5, "pull_request": {} },
                        "comment": { "user": human, "body": "hi" } }),
                None,
            ),
        ];
        for (event, payload, want) in cases {
            let got = reply(Some(event), &payload);
            let want = want.map(|kind| Reply {
                number: 5,
                login: "ann".into(),
                kind,
            });
            assert_eq!(got, want, "{event} {payload}");
        }
    }
}
