//! The agent's review, the queue's first gate. A tick starts a review of each queued PR's head;
//! this worker runs it in the sandbox, clears what the agent is sure of, and asks a human about
//! the rest, on GitHub and on the dashboard. It also has the agent answer the humans' questions
//! about its findings.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use mergepatrol_core::{
    Answered, Judged, PrNumber, RepoName, RepoSettings, ReviewSettings, ReviewStatus, review_url,
};
use mergepatrol_executor::protocol::{
    ANSWER_FILE, JobOutcome, JobSpec, REVIEW_FILE, ReviewOutput, Task,
};
use mergepatrol_github::{App, CodeOwners, Installation, PullRequest};
use mergepatrol_store::{LaneKey, ReviewDone, ReviewRecord};
use tokio::task::JoinSet;

use crate::github::GitHub;
use crate::queue::{Ctx, app_credentials, job_spec, record, unix_now};

/// Reviews running at once, across workspaces. Each also takes one of the sandbox's job slots.
const REVIEWS_AT_ONCE: usize = 4;
/// How often to look for work a wake-up missed: a question asked on another instance.
const POLL: Duration = Duration::from_secs(5);
/// A run taken this long ago was cut short, as by a restart: it runs again.
const STALE: Duration = Duration::from_secs(45 * 60);
/// How many runs may fail on errors outside the PR before a human reviews it instead.
const ATTEMPTS: u32 = 3;
const RETRY_AFTER: Duration = Duration::from_secs(60);
/// Where GitHub looks for CODEOWNERS, in its order.
const CODEOWNERS: [&str; 3] = [".github/CODEOWNERS", "CODEOWNERS", "docs/CODEOWNERS"];

/// Runs reviews and questions as they come, until the queue lock is lost: dropping the future
/// stops the runs it started.
pub async fn run(ctx: Arc<Ctx>) {
    let mut running = JoinSet::new();
    loop {
        while running.try_join_next().is_some() {}
        while running.len() < REVIEWS_AT_ONCE {
            match next(&ctx).await {
                Ok(Some(work)) => {
                    let ctx = ctx.clone();
                    running.spawn(async move { work.run(&ctx).await });
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::error!(error = %format!("{e:#}"), "could not take a review");
                    break;
                }
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(POLL) => {}
            _ = ctx.reviews.notified() => {}
            Some(_) = running.join_next(), if !running.is_empty() => {}
        }
    }
}

enum Work {
    Review(ReviewRecord),
    Question(ReviewRecord, mergepatrol_core::Question),
}

async fn next(ctx: &Ctx) -> Result<Option<Work>> {
    if let Some(r) = ctx.store.claim_review(STALE).await? {
        return Ok(Some(Work::Review(r)));
    }
    Ok(ctx
        .store
        .claim_question(STALE)
        .await?
        .map(|(r, q)| Work::Question(r, q)))
}

impl Work {
    async fn run(self, ctx: &Ctx) {
        let (r, what) = match &self {
            Work::Review(r) => (r, "review"),
            Work::Question(r, _) => (r, "question"),
        };
        let (installation, repo, number) = (r.installation_id, r.repo.clone(), r.number);
        let done = match self {
            Work::Review(r) => review(ctx, r).await,
            Work::Question(r, q) => question(ctx, r, q).await,
        };
        if let Err(e) = done {
            let error = format!("{e:#}");
            tracing::error!(installation, %repo, pr = number, what, %error, "the agent's review failed");
        }
        // The PR's hold may have changed.
        ctx.wakers.wake_repo(installation, &repo);
    }
}

/// The installation's client, and the PR as GitHub has it now.
async fn pull(ctx: &Ctx, r: &ReviewRecord) -> Result<(Installation, PullRequest)> {
    let creds = app_credentials(ctx)
        .await?
        .context("no GitHub App is set up")?;
    let gh = App::from_credentials(&creds)?.installation(r.installation_id);
    let pr = gh.pull_request(&r.repo, r.number).await?;
    Ok((gh, pr))
}

/// What the repo's queue runs with: its config file over the defaults, which a file that does
/// not parse leaves as they are.
async fn settings(ctx: &Ctx, gh: &Installation, repo: &RepoName) -> Result<RepoSettings> {
    let file = GitHub::config_file(gh, repo).await?;
    let defaults = &ctx.config.repo_defaults;
    Ok(RepoSettings::from_file(file.as_deref(), defaults).unwrap_or_else(|_| defaults.clone()))
}

fn lane_key(r: &ReviewRecord, pr: &PullRequest) -> LaneKey {
    LaneKey {
        installation_id: r.installation_id,
        repo: r.repo.clone(),
        trunk: pr.merges_into().to_owned(),
    }
}

async fn review(ctx: &Ctx, r: ReviewRecord) -> Result<()> {
    let (gh, pr) = pull(ctx, &r).await?;
    // A fork PR's head is its pull ref's, which its GraphQL lags behind: no run, so no check.
    if !pr.is_cross_repository && pr.head_ref_oid != r.head_sha {
        // A newer head has its own review, which the next tick starts.
        return ctx.store.stale_review(r.id).await;
    }
    let key = lane_key(&r, &pr);
    let settings = settings(ctx, &gh, &r.repo).await?;
    let workspace = ctx
        .store
        .review_settings(r.installation_id)
        .await?
        .unwrap_or_default();
    let bars = workspace.with(&settings.review);

    let found = match pr.is_cross_repository {
        // A fork's code is not trusted: no agent reads it, as none rebases or fixes it.
        true => Err("The agent does not review PRs from forks.".to_owned()),
        false => run_review(ctx, &gh, &key, &pr, &settings, &r).await,
    };
    let output = match found {
        Ok(output) => output,
        Err(why) if r.attempts + 1 < ATTEMPTS && !why.starts_with(NO_KEY) => {
            ctx.store.retry_review(r.id, &why, RETRY_AFTER).await?;
            return Ok(());
        }
        Err(why) => {
            return finish(ctx, &gh, &key, &pr, &r, &bars, Err(why)).await;
        }
    };
    finish(ctx, &gh, &key, &pr, &r, &bars, Ok(output)).await
}

const NO_KEY: &str = "No model API key";

/// Runs the review in the sandbox. `Err` says why the agent could not review.
async fn run_review(
    ctx: &Ctx,
    gh: &Installation,
    key: &LaneKey,
    pr: &PullRequest,
    settings: &RepoSettings,
    r: &ReviewRecord,
) -> std::result::Result<ReviewOutput, String> {
    let answered = answered_before(ctx, r)
        .await
        .map_err(|e| format!("{e:#}"))?;
    let job_id = format!(
        "{}-{}-{}-review-{}",
        r.repo.owner,
        r.repo.name,
        r.number,
        &r.head_sha[..8]
    );
    let ran = async {
        let trunk = gh.branch_sha(&r.repo, pr.merges_into()).await?;
        let task = Task::Review { answered };
        let (spec, granted) =
            job_spec(ctx, gh, key, pr, settings, &r.head_sha, &trunk, None, task).await?;
        if granted.is_none() {
            bail!(
                "{NO_KEY} in the workspace's settings for {}: the agent cannot review.",
                settings.resolver.provider.name()
            );
        }
        let out = run_job(ctx, gh, settings, key, r.number, &job_id, &spec).await?;
        let text = tokio::fs::read(out.join(REVIEW_FILE))
            .await
            .context("the job wrote no review")?;
        anyhow::Ok(serde_json::from_slice::<ReviewOutput>(&text)?)
    };
    ran.await.map_err(|e| format!("{e:#}"))
}

/// Runs a job in the sandbox, on one of its slots: the directory its output is in. Hosted, it
/// counts as the queue's jobs do.
async fn run_job(
    ctx: &Ctx,
    gh: &Installation,
    settings: &RepoSettings,
    key: &LaneKey,
    number: PrNumber,
    job_id: &str,
    spec: &JobSpec,
) -> Result<std::path::PathBuf> {
    if let Some(b) = &ctx.billing {
        let private = GitHub::snapshot(gh, &key.repo, &settings.labels())
            .await?
            .private;
        b.job(key.installation_id, private, unix_now()).await?;
    }
    let out = ctx.config.data_dir.join("jobs").join(job_id);
    let _ = tokio::fs::remove_dir_all(&out).await;
    let result = {
        let _slot = ctx.jobs.acquire().await?;
        ctx.executor.run(job_id, spec, &out).await?
    };
    for e in &result.events {
        record(ctx, key, number, &e.kind, &e.message).await?;
    }
    if let JobOutcome::Failed { reason } = result.outcome {
        bail!("{reason}");
    }
    Ok(out)
}

/// What humans answered on the PR's last answered review, of an earlier head.
async fn answered_before(ctx: &Ctx, r: &ReviewRecord) -> Result<Vec<Answered>> {
    let earlier = ctx
        .store
        .reviews_of(r.installation_id, &r.repo, r.number, 10)
        .await?;
    let Some(last) = earlier
        .into_iter()
        .find(|e| e.id != r.id && !e.answers.is_empty())
    else {
        return Ok(Vec::new());
    };
    let by = last.answered_by.clone().unwrap_or_default();
    Ok(last
        .findings
        .into_iter()
        .filter_map(|j| {
            let answer = last.answers.get(&j.finding.id)?.clone();
            Some(Answered {
                finding: j.finding,
                answer,
                by: by.clone(),
            })
        })
        .collect())
}

/// Ends the review: approved when it asks about nothing, else it asks the people who own the
/// code in question. `Err` is why the agent could not review: a human reviews the whole PR.
#[allow(clippy::too_many_arguments)]
async fn finish(
    ctx: &Ctx,
    gh: &Installation,
    key: &LaneKey,
    pr: &PullRequest,
    r: &ReviewRecord,
    bars: &ReviewSettings,
    output: std::result::Result<ReviewOutput, String>,
) -> Result<()> {
    let (summary, findings, error) = match output {
        Ok(o) => (o.summary, bars.judge(o.findings), None),
        Err(why) => (String::new(), Vec::new(), Some(why)),
    };
    let asks = findings.iter().filter(|f| f.asked).count();
    let status = match (asks, &error) {
        (0, None) => ReviewStatus::Approved,
        _ => ReviewStatus::Asks,
    };
    let asked = match status {
        ReviewStatus::Asks => askees(gh, &r.repo, pr, &findings).await,
        _ => Vec::new(),
    };
    let done = ReviewDone {
        status,
        error: error.clone(),
        summary,
        findings,
        asked,
    };
    if !ctx.store.finish_review(r.id, &done).await? {
        return Ok(());
    }
    let (kind, message) = match (&error, status) {
        (Some(why), _) => ("review-failed", why.clone()),
        (None, ReviewStatus::Approved) => (
            "review-approved",
            format!("cleared {} things alone", done.findings.len()),
        ),
        (None, _) => (
            "review-asks",
            format!(
                "asks a human about {asks} of {} things",
                done.findings.len()
            ),
        ),
    };
    record(ctx, key, r.number, kind, &message).await?;

    // GitHub hears of it too, where the PR's people are. Failing to post loses no state. A
    // comment, never an approval: an App's approval could meet a rule that asks for a human's.
    let body = github_body(ctx, key, r.number, &done);
    let posted = gh
        .review(&r.repo, r.number, &r.head_sha, "COMMENT", &body)
        .await;
    if let Err(e) = posted {
        tracing::warn!(repo = %r.repo, pr = r.number, error = %format!("{e:#}"), "could not post the review");
    }
    if let Err(e) = gh.request_reviewers(&r.repo, r.number, &done.asked).await {
        tracing::warn!(repo = %r.repo, pr = r.number, error = %format!("{e:#}"), "could not ask for reviews");
    }
    Ok(())
}

/// Who the agent asks: the code owners of the files its asks are about, users and teams as
/// `org/team`, else the reviewers the PR already asked. Never its author. Empty: anyone who may
/// approve it.
async fn askees(
    gh: &Installation,
    repo: &RepoName,
    pr: &PullRequest,
    findings: &[Judged],
) -> Vec<String> {
    let author = pr.summary().author.map(|a| a.login.to_lowercase());
    let mut logins: Vec<String> = Vec::new();
    if let Some(owners) = codeowners(gh, repo).await {
        for f in findings.iter().filter(|f| f.asked) {
            let Some(file) = &f.finding.file else {
                continue;
            };
            logins.extend(
                owners
                    .owners(file)
                    .iter()
                    // `@login`, or a team as `@org/team`; not an email address.
                    .filter_map(|o| o.strip_prefix('@'))
                    .map(str::to_owned),
            );
        }
    }
    if logins.is_empty() {
        logins = pr
            .summary()
            .requested
            .into_iter()
            .map(|p| p.login)
            .collect();
    }
    let mut seen = std::collections::HashSet::new();
    logins.retain(|l| Some(l.to_lowercase()) != author && seen.insert(l.to_lowercase()));
    logins
}

async fn codeowners(gh: &Installation, repo: &RepoName) -> Option<CodeOwners> {
    for path in CODEOWNERS {
        if let Ok(file) = gh.default_branch_file(repo, path).await
            && let Some(text) = file.text
        {
            return Some(CodeOwners::parse(&file.branch, &text));
        }
    }
    None
}

/// The review as GitHub shows it: what the agent asks, and what it cleared.
fn github_body(ctx: &Ctx, key: &LaneKey, number: PrNumber, done: &ReviewDone) -> String {
    let url = review_url(&ctx.config.public_url, &key.repo, &key.trunk, number);
    let cleared: Vec<&Judged> = done.findings.iter().filter(|f| !f.asked).collect();
    let mut body = String::from("**Mergepatrol's review**");
    if let Some(why) = &done.error {
        body.push_str(&format!(
            "\n\nThe agent could not review this PR: {why}\n\nA human reviews it instead: [approve it, or ask for changes]({url}), or approve it here on GitHub."
        ));
        return body;
    }
    if !done.summary.is_empty() {
        body.push_str(&format!("\n\n{}", done.summary));
    }
    let asks: Vec<&Judged> = done.findings.iter().filter(|f| f.asked).collect();
    if asks.is_empty() {
        body.push_str(&format!(
            "\n\nIt cleared all {} things it looked at. Next: the merge queue.",
            cleared.len()
        ));
    } else {
        body.push_str(&format!(
            "\n\nIt cleared {} things on its own, and asks a human about {}:\n",
            cleared.len(),
            asks.len()
        ));
        for (i, f) in asks.iter().enumerate() {
            let place = match (&f.finding.file, f.finding.line) {
                (Some(file), Some(line)) => format!(" (`{file}:{line}`)"),
                (Some(file), None) => format!(" (`{file}`)"),
                _ => String::new(),
            };
            body.push_str(&format!(
                "\n{}. **{}**{place}, {}% sure",
                i + 1,
                f.finding.title,
                f.finding.confidence
            ));
        }
        let who = match done.asked.as_slice() {
            [] => "Anyone who may approve this PR".to_owned(),
            logins => logins
                .iter()
                .map(|l| format!("@{l}"))
                .collect::<Vec<_>>()
                .join(", "),
        };
        body.push_str(&format!(
            "\n\n{who}: [answer them in Mergepatrol]({url}), or approve the PR here on GitHub if they are all fine as they are. A comment doesn't answer them. The PR keeps its place in the queue meanwhile."
        ));
    }
    if !cleared.is_empty() {
        body.push_str("\n\n<details><summary>What it cleared</summary>\n");
        for f in cleared {
            body.push_str(&format!(
                "\n- {} ({}%)",
                f.finding.title, f.finding.confidence
            ));
        }
        body.push_str("\n\n</details>");
    }
    body
}

async fn question(ctx: &Ctx, r: ReviewRecord, q: mergepatrol_core::Question) -> Result<()> {
    let answer = async {
        let (gh, pr) = pull(ctx, &r).await?;
        if pr.head_ref_oid != r.head_sha {
            bail!("The PR moved on since the review: its new head gets a review of its own.");
        }
        let finding = r
            .findings
            .iter()
            .find(|f| f.finding.id == q.finding)
            .context("no such finding")?
            .finding
            .clone();
        let key = lane_key(&r, &pr);
        let settings = settings(ctx, &gh, &r.repo).await?;
        let trunk = gh.branch_sha(&r.repo, pr.merges_into()).await?;
        let task = Task::Question {
            finding: Box::new(finding),
            question: q.question.clone(),
        };
        let (spec, granted) = job_spec(
            ctx,
            &gh,
            &key,
            &pr,
            &settings,
            &r.head_sha,
            &trunk,
            None,
            task,
        )
        .await?;
        if granted.is_none() {
            bail!("{NO_KEY} in the workspace's settings: the agent cannot answer.");
        }
        let job_id = format!(
            "{}-{}-{}-{}-{}",
            r.repo.owner,
            r.repo.name,
            r.number,
            q.id,
            &r.head_sha[..8]
        );
        let out = run_job(ctx, &gh, &settings, &key, r.number, &job_id, &spec).await?;
        let text = tokio::fs::read_to_string(out.join(ANSWER_FILE))
            .await
            .context("the job wrote no answer")?;
        anyhow::Ok(text)
    };
    let answer = answer.await.map_err(|e| format!("{e:#}"));
    ctx.store.answer_question(r.id, &q.id, answer).await
}
