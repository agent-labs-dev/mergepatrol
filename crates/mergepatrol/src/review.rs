//! The agent's review, the queue's first gate. A tick starts a review of each queued PR's head;
//! this worker runs it in the sandbox, sends what the agent is sure should change back to the
//! PR's author, and asks a human only about what the author cannot settle, on GitHub and on the
//! dashboard. It also has the agent answer questions about its findings: a human's, or the
//! author's reply to a finding sent back, which the agent may withdraw on.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use mergepatrol_core::{
    Answered, Finding, Judged, PrNumber, RepoName, RepoSettings, ReviewSettings, ReviewStatus,
    SentBack, review_status, review_url,
};
use mergepatrol_executor::protocol::{
    ANSWER_FILE, JobOutcome, JobSpec, REVIEW_FILE, ReviewOutput, Task,
};
use mergepatrol_github::{App, CodeOwners, Installation, PullRequest, ReviewThread, ThreadComment};
use mergepatrol_resolver::withdraws;
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
/// How many reviews in a row may send findings back to the author before the next asks a human
/// about them instead: the author and the agent disagree, or the fix keeps missing.
const ROUNDS: usize = 3;
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
    let earlier = earlier_reviews(ctx, r)
        .await
        .map_err(|e| format!("{e:#}"))?;
    let answered = answered_before(&earlier);
    let threads = threads(gh, r).await;
    let sent_back = sent_back_before(&earlier)
        .into_iter()
        .map(|finding| {
            let thread = threads
                .iter()
                .find(|t| title_of(t) == Some(norm(&finding.title)));
            SentBack {
                reply: thread
                    .and_then(|t| t.reply.as_ref())
                    .filter(|(_, body)| !body.contains(REPLY_MARK))
                    .map(|(_, body)| body.clone())
                    .unwrap_or_default(),
                resolved: thread.is_some_and(|t| t.resolved),
                finding,
            }
        })
        .collect();
    let job_id = format!(
        "{}-{}-{}-review-{}",
        r.repo.owner,
        r.repo.name,
        r.number,
        &r.head_sha[..8]
    );
    let ran = async {
        let trunk = gh.branch_sha(&r.repo, pr.merges_into()).await?;
        let task = Task::Review {
            answered,
            sent_back,
        };
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

/// The PR's reviews of earlier heads that finished, newest first.
async fn earlier_reviews(ctx: &Ctx, r: &ReviewRecord) -> Result<Vec<ReviewRecord>> {
    let mut earlier = ctx
        .store
        .reviews_of(r.installation_id, &r.repo, r.number, 10)
        .await?;
    earlier.retain(|e| {
        e.id != r.id
            && e.head_sha != r.head_sha
            && !matches!(
                e.status,
                ReviewStatus::Pending | ReviewStatus::Running | ReviewStatus::Stale
            )
    });
    Ok(earlier)
}

/// What humans answered on the PR's last answered review, of an earlier head.
fn answered_before(earlier: &[ReviewRecord]) -> Vec<Answered> {
    let Some(last) = earlier.iter().find(|e| !e.answers.is_empty()) else {
        return Vec::new();
    };
    let by = last.answered_by.clone().unwrap_or_default();
    last.findings
        .iter()
        .filter_map(|j| {
            let answer = last.answers.get(&j.finding.id)?.clone();
            Some(Answered {
                finding: j.finding.clone(),
                answer,
                by: by.clone(),
            })
        })
        .collect()
}

/// What the last review sent back to the author and still held the PR for: the next checks
/// the fix.
fn sent_back_before(earlier: &[ReviewRecord]) -> Vec<Finding> {
    earlier
        .first()
        .map(|last| {
            last.findings
                .iter()
                .filter(|j| j.sent_back)
                .map(|j| j.finding.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Whether the author has had their rounds: the last `ROUNDS` reviews each sent findings back,
/// and no human stepped in.
fn rounds_spent(earlier: &[ReviewRecord]) -> bool {
    earlier.len() >= ROUNDS
        && earlier[..ROUNDS]
            .iter()
            .all(|e| e.answered_by.is_none() && e.findings.iter().any(|j| j.sent_back))
}

/// Ends the review: approved when it holds nothing, sent back to the author when it found what
/// they should fix, else it asks the people who own the code in question. `Err` is why the
/// agent could not review: a human reviews the whole PR.
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
    let (summary, mut findings, error) = match output {
        Ok(o) => (o.summary, bars.judge(o.findings), None),
        Err(why) => (String::new(), Vec::new(), Some(why)),
    };
    // The author had their rounds and it still finds what to send back: a human decides.
    if findings.iter().any(|f| f.sent_back) && rounds_spent(&earlier_reviews(ctx, r).await?) {
        for f in findings.iter_mut().filter(|f| f.sent_back) {
            f.sent_back = false;
            f.asked = true;
        }
    }
    let asks = findings.iter().filter(|f| f.asked).count();
    let sent = findings.iter().filter(|f| f.sent_back).count();
    let status = review_status(&findings, error.is_some());
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
        (None, ReviewStatus::Changes) => (
            "review-sent-back",
            format!(
                "sent {sent} of {} things back to the author",
                done.findings.len()
            ),
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

    // GitHub hears of it too, where the PR's people are: each finding that holds the PR in a
    // thread on its line, which its author resolves. Failing to post loses no state, but a
    // finding with no thread waits for a push. A comment, never an approval: an App's approval
    // could meet a rule that asks for a human's.
    match post(ctx, gh, key, pr, r, &done).await {
        Ok(threaded) if !threaded.is_empty() => {
            ctx.store.mark_threaded(r.id, &threaded).await?;
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(repo = %r.repo, pr = r.number, error = %format!("{e:#}"), "could not post the review");
        }
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

/// Starts each of MergeLoop's threads, with the finding's title, so the next review finds it.
const MARK: &str = "<!-- mergepatrol-finding: ";
/// Ends each of MergeLoop's replies in a thread, so a reply is not taken for the author's.
const REPLY_MARK: &str = "<!-- mergepatrol-reply -->";

/// A title as threads match it.
fn norm(title: &str) -> String {
    title.trim().to_lowercase().replace("--", "-")
}

/// The title of the finding a thread of MergeLoop's is about.
fn title_of(t: &ReviewThread) -> Option<String> {
    let rest = &t.body[t.body.find(MARK)? + MARK.len()..];
    Some(rest[..rest.find(" -->")?].to_owned())
}

/// MergeLoop's threads on the PR. None when GitHub could not say.
async fn threads(gh: &Installation, r: &ReviewRecord) -> Vec<ReviewThread> {
    gh.threads_marked(&r.repo, r.number, MARK)
        .await
        .inspect_err(|e| {
            tracing::warn!(repo = %r.repo, pr = r.number, error = %format!("{e:#}"), "could not read the review threads");
        })
        .unwrap_or_default()
}

/// Posts the review: a summary, and a thread for each finding that holds the PR. A finding a
/// thread is already about gets a reply there, and its thread opens again; a thread whose
/// finding no longer holds is resolved. Returns the ids of the findings that have a thread.
async fn post(
    ctx: &Ctx,
    gh: &Installation,
    key: &LaneKey,
    pr: &PullRequest,
    r: &ReviewRecord,
    done: &ReviewDone,
) -> Result<Vec<String>> {
    let short = &r.head_sha[..7.min(r.head_sha.len())];
    let before = threads(gh, r).await;
    let held: Vec<&Judged> = done
        .findings
        .iter()
        .filter(|f| f.sent_back || f.asked)
        .collect();
    let mut threaded = Vec::new();
    let mut new = Vec::new();
    for f in &held {
        let title = norm(&f.finding.title);
        if let Some(t) = before.iter().find(|t| title_of(t).as_ref() == Some(&title)) {
            let body = format!(
                "Still holds on `{short}`: {}\n\n{}\n\n{REPLY_MARK}",
                f.finding.finding,
                still(f)
            );
            gh.reply_in_thread(&r.repo, r.number, t.comment_id, &body)
                .await?;
            if t.resolved {
                gh.resolve_thread(&t.id, false).await?;
            }
            threaded.push(f.finding.id.clone());
            continue;
        }
        // A finding about the whole PR goes on its first file.
        let Some(path) = f.finding.file.clone().or_else(|| pr.files.first().cloned()) else {
            continue;
        };
        let line = f.finding.file.as_ref().and(f.finding.line);
        new.push(ThreadComment {
            path,
            line,
            body: thread_body(f),
        });
    }
    let body = github_body(ctx, key, r.number, done);
    gh.review_lines(&r.repo, r.number, &r.head_sha, &body, &new)
        .await?;
    // Which new threads GitHub took: one turned down went in the body, and has none.
    let after = threads(gh, r).await;
    for f in &held {
        let title = norm(&f.finding.title);
        if !threaded.contains(&f.finding.id)
            && after.iter().any(|t| title_of(t).as_ref() == Some(&title))
        {
            threaded.push(f.finding.id.clone());
        }
    }
    // What no longer holds: the review found it fixed, or took the author's answer.
    let holds: Vec<String> = held.iter().map(|f| norm(&f.finding.title)).collect();
    for t in after.iter().filter(|t| !t.resolved) {
        if title_of(t).is_some_and(|title| !holds.contains(&title)) {
            let body = format!("Cleared by the review of `{short}`.\n\n{REPLY_MARK}");
            gh.reply_in_thread(&r.repo, r.number, t.comment_id, &body)
                .await?;
            gh.resolve_thread(&t.id, true).await?;
        }
    }
    Ok(threaded)
}

/// What the author does about a finding.
fn still(f: &Judged) -> String {
    match f.asked {
        true => {
            "A human decides this one: MergeLoop asked them. Resolve this thread once they have."
                .to_owned()
        }
        false => match f.finding.suggestion.as_str() {
            "" => "Fix it and push, or reply with why it doesn't apply. Then resolve this thread."
                .to_owned(),
            change => format!(
                "**Change:** {change}\n\nFix it and push, or reply with why it doesn't apply. Then resolve this thread."
            ),
        },
    }
}

/// The comment that starts a finding's thread.
fn thread_body(f: &Judged) -> String {
    let to = match f.asked {
        true => "For a human",
        false => "Sent back to the author",
    };
    let whole = match f.finding.file {
        None => " (about the whole PR)",
        Some(_) => "",
    };
    format!(
        "**{to}** · `{}` · {} · {}% sure{whole}\n\n**{}**\n\n{}\n\n{}\n\n{MARK}{} -->",
        f.finding.id,
        f.finding.kind.as_str(),
        f.finding.confidence,
        f.finding.title,
        f.finding.finding,
        still(f),
        norm(&f.finding.title),
    )
}

/// The review's summary on GitHub: what it sent back to the author and asks a human, each in
/// a thread on its line, and what it cleared.
fn github_body(ctx: &Ctx, key: &LaneKey, number: PrNumber, done: &ReviewDone) -> String {
    let url = review_url(&ctx.config.public_url, &key.repo, &key.trunk, number);
    let mut body = String::from("**MergeLoop's review**");
    if let Some(why) = &done.error {
        body.push_str(&format!(
            "\n\nThe agent could not review this PR: {why}\n\nA human reviews it instead: [approve it, or ask for changes]({url}), or approve it here on GitHub."
        ));
        return body;
    }
    if !done.summary.is_empty() {
        body.push_str(&format!("\n\n{}", done.summary));
    }
    let sent = done.findings.iter().filter(|f| f.sent_back).count();
    let asks = done.findings.iter().filter(|f| f.asked).count();
    let cleared: Vec<&Judged> = done
        .findings
        .iter()
        .filter(|f| !f.asked && !f.sent_back)
        .collect();
    if sent == 0 && asks == 0 {
        body.push_str(&format!(
            "\n\nIt cleared all {} things it looked at. Next: the merge queue.",
            cleared.len()
        ));
    }
    if sent > 0 {
        body.push_str(&format!(
            "\n\nIt sent {sent} back to the author, each in a thread on its line. For each, fix it and push, or reply with why it doesn't apply, then resolve the thread. Once every thread is resolved, the PR goes on to the merge queue with no human approval."
        ));
    }
    if asks > 0 {
        let who = match done.asked.as_slice() {
            [] => "Anyone who may approve this PR".to_owned(),
            logins => logins
                .iter()
                .map(|l| format!("@{l}"))
                .collect::<Vec<_>>()
                .join(", "),
        };
        body.push_str(&format!(
            "\n\nIt asks a human about {asks}, each in a thread on its line. {who}: [answer them in MergeLoop]({url}), or approve the PR here on GitHub if they are all fine as they are. The PR keeps its place in the queue meanwhile."
        ));
    }
    if !cleared.is_empty() {
        body.push_str("\n\n<details><summary>What it cleared</summary>\n");
        for f in cleared {
            let note = match f.finding.lean {
                mergepatrol_core::Lean::Change => ", a note under the bar",
                mergepatrol_core::Lean::Fine => "",
            };
            body.push_str(&format!(
                "\n- {} ({}%{note})",
                f.finding.title, f.finding.confidence
            ));
        }
        body.push_str("\n\n</details>");
    }
    body
}

async fn question(ctx: &Ctx, r: ReviewRecord, q: mergepatrol_core::Question) -> Result<()> {
    // A reply to a finding sent back to the author, which the agent may withdraw.
    let sent_back = r
        .findings
        .iter()
        .any(|f| f.finding.id == q.finding && f.sent_back);
    let mut lane = None;
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
        lane = Some(key.clone());
        let settings = settings(ctx, &gh, &r.repo).await?;
        let trunk = gh.branch_sha(&r.repo, pr.merges_into()).await?;
        let task = Task::Question {
            finding: Box::new(finding),
            question: q.question.clone(),
            sent_back,
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
    let withdrawn = sent_back && answer.as_deref().is_ok_and(withdraws);
    ctx.store.answer_question(r.id, &q.id, answer).await?;
    if withdrawn && let Some(status) = ctx.store.withdraw_finding(r.id, &q.finding).await? {
        tracing::info!(repo = %r.repo, pr = r.number, finding = %q.finding, status = status.as_str(), "the agent withdrew a finding");
        if let Some(key) = &lane {
            let message = format!("withdrew {} on the author's reply", q.finding);
            record(ctx, key, r.number, "review-withdrew", &message).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mergepatrol_core::{Bar, Kind, Lean};

    fn review(id: i64, sent_back: bool, answered_by: Option<&str>) -> ReviewRecord {
        let finding = Finding {
            id: "f1".into(),
            kind: Kind::Other,
            title: "t".into(),
            file: None,
            line: None,
            confidence: 90,
            lean: Lean::Change,
            finding: String::new(),
            unsure: String::new(),
            checked: Vec::new(),
            would_clear: String::new(),
            suggestion: "fix it".into(),
            needs_human: false,
            code: Vec::new(),
            related: Vec::new(),
        };
        ReviewRecord {
            id,
            installation_id: 1,
            repo: RepoName::parse("o/r").unwrap(),
            number: 7,
            head_sha: format!("head{id}"),
            status: ReviewStatus::Changes,
            error: None,
            attempts: 0,
            summary: String::new(),
            findings: vec![Judged {
                finding,
                asked: false,
                sent_back,
                withdrawn: false,
                threaded: false,
                bar: Bar::From(80),
            }],
            asked: Vec::new(),
            answers: Default::default(),
            answered_by: answered_by.map(str::to_owned),
            note: String::new(),
            questions: Vec::new(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn a_findings_thread_carries_its_title_for_the_next_review_to_find() {
        let mut f = review(1, true, None).findings.remove(0);
        f.finding.title = "Retry loop has no cap --> ever".into();
        let thread = ReviewThread {
            id: "t".into(),
            resolved: false,
            comment_id: 1,
            body: thread_body(&f),
            reply: None,
        };
        assert_eq!(
            title_of(&thread),
            Some(norm("retry loop has NO cap --> ever "))
        );
        assert!(thread.body.contains("Sent back to the author"));
        assert!(thread.body.contains("Then resolve this thread."));
    }

    #[test]
    fn the_author_gets_three_rounds_before_a_human_steps_in() {
        let back = |id| review(id, true, None);
        assert!(!rounds_spent(&[back(2), back(1)]));
        assert!(rounds_spent(&[back(3), back(2), back(1)]));
        // A round that sent nothing back, or that a human answered, starts the count over.
        assert!(!rounds_spent(&[back(3), review(2, false, None), back(1)]));
        assert!(!rounds_spent(&[
            back(3),
            review(2, true, Some("mona")),
            back(1)
        ]));
        assert_eq!(sent_back_before(&[back(3)]).len(), 1);
        assert!(sent_back_before(&[]).is_empty());
    }
}
