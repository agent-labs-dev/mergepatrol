//! The agent's review, the queue's first gate. A tick starts a review of each queued PR's head;
//! this worker runs it in the sandbox, sends what the agent is sure should change back to the
//! PR's author, and asks a human only about what the author cannot settle, on GitHub and on the
//! dashboard. It also has the agent answer questions about its findings: a human's, or the
//! author's reply to a finding sent back, which the agent may withdraw on.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use mergepatrol_core::{
    Answered, Event, Evidence, Finding, Judged, Lean, PrNumber, RepoName, RepoSettings,
    ReviewSettings, ReviewStatus, Runs, SentBack, review_status, review_url,
};
use mergepatrol_executor::protocol::{
    ANSWER_FILE, JobOutcome, JobSpec, REVIEW_FILE, ReviewOutput, Task,
};
use mergepatrol_github::{App, CodeOwners, Installation, PullRequest, ReviewThread, ThreadComment};
use mergepatrol_resolver::{ReviewRequest, Verify, author_review_brief, withdraws};
use mergepatrol_store::{LaneKey, ReviewDone, ReviewRecord};
use mergepatrol_web::merge_requested;
use tokio::task::JoinSet;

use crate::github::GitHub;
use crate::queue::{
    Alive, Claim, Ctx, Unit, app_credentials, fix_findings, job_spec, record, units, unix_now,
};

/// Reviews running at once, across workspaces. Each also takes one of the sandbox's job slots.
const REVIEWS_AT_ONCE: usize = 4;
/// How often to look for work a wake-up missed: a question asked on another instance.
const POLL: Duration = Duration::from_secs(5);
/// A run taken this long ago lost its worker, as to a panic: it runs again. A restart needs no
/// such wait: the next worker lets go of everything at the start of `run`.
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
pub async fn run(ctx: Arc<Ctx>, alive: Alive) {
    match ctx.store.release_reviews().await {
        Ok(0) => {}
        Ok(released) => tracing::info!(released, "took back reviews a restart cut short"),
        Err(e) => tracing::error!(error = %format!("{e:#}"), "could not take back reviews"),
    }
    let mut running = JoinSet::new();
    loop {
        while running.try_join_next().is_some() {}
        while running.len() < REVIEWS_AT_ONCE {
            match next(&ctx).await {
                Ok(Some(work)) => {
                    let (ctx, alive) = (ctx.clone(), alive.clone());
                    running.spawn(async move {
                        let _alive = alive;
                        work.run(&ctx).await
                    });
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

/// What the repo's queue runs with: its config file over the workspace's defaults, which a file
/// that does not parse leaves as they are.
async fn settings(ctx: &Ctx, gh: &Installation, repo: &RepoName) -> Result<RepoSettings> {
    let file = GitHub::config_file(gh, repo).await?;
    let defaults = ctx
        .store
        .inherited(GitHub::installation_id(gh), &ctx.config.repo_defaults)
        .await?;
    Ok(RepoSettings::from_file(file.as_deref(), &defaults).unwrap_or(defaults))
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
        return ctx.store.stale_review(r.id, r.claim).await;
    }
    let key = lane_key(&r, &pr);
    let settings = settings(ctx, &gh, &r.repo).await?;
    // Only a PR told to merge gets the agent's review: no label, or no place in the lane, and the
    // review is dropped rather than run. Nothing is run and nothing is posted.
    if !merge_requested(ctx.store.as_ref(), &gh, &r.repo, &pr, &settings.label).await? {
        return ctx.store.stale_review(r.id, r.claim).await;
    }
    let workspace = ctx
        .store
        .review_settings(r.installation_id)
        .await?
        .unwrap_or_default();
    let bars = workspace.with(&settings.review);

    let by_author = settings.agent.runs == Runs::Local;
    let found = match pr.is_cross_repository {
        // A fork's code is not trusted: no agent reads it, as none rebases or fixes it.
        true => Err("The agent does not review PRs from forks.".to_owned()),
        // The author's agent reviews, and posts what it found: this only judges that.
        false if by_author => match &r.submitted {
            Some(review) => posted_review(review),
            None => {
                let brief = author_brief(ctx, &gh, &pr, &settings, &r).await?;
                return ctx.store.await_author(r.id, r.claim, &brief).await;
            }
        },
        false => run_review(ctx, &gh, &key, &pr, &settings, &r).await,
    };
    let output = match found {
        Ok(output) => Ok(output),
        // What the author's agent posted reads the same each time.
        Err(why) if r.attempts + 1 < ATTEMPTS && !why.starts_with(NO_KEY) && !by_author => {
            ctx.store
                .retry_review(r.id, r.claim, &why, RETRY_AFTER)
                .await?;
            return Ok(());
        }
        Err(why) => Err(why),
    };
    conclude(ctx, &gh, &key, &pr, &r, &settings, &bars, output).await
}

/// Ends the review, and posts it on GitHub only for what it leaves a person to do: what it sent
/// back goes to the agent's fix first, and the author hears only of what the fix left them.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn conclude(
    ctx: &Ctx,
    gh: &Installation,
    key: &LaneKey,
    pr: &PullRequest,
    r: &ReviewRecord,
    settings: &RepoSettings,
    bars: &ReviewSettings,
    output: std::result::Result<ReviewOutput, String>,
) -> Result<()> {
    let Some((held, done)) = finish(ctx, gh, key, pr, r, settings, bars, output).await? else {
        return Ok(());
    };
    let fix = fix_sent_back(ctx, gh, key, pr, settings, r.id, held)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(repo = %r.repo, pr = r.number, error = %format!("{e:#}"), "could not fix the review's findings");
            Fix::Unfixed
        });
    match fix {
        Fix::Pushed => ctx.store.settle_review(r.id, r.claim).await,
        Fix::Superseded => ctx.store.stale_review(r.id, r.claim).await,
        Fix::Unfixed => match left_to_author(ctx, gh, key, r, settings, &done).await {
            Ok(()) => Ok(()),
            // The author has not heard of it: the review runs again rather than hold the PR.
            Err(e) => {
                tracing::warn!(repo = %r.repo, pr = r.number, error = %format!("{e:#}"), "could not post what the fix left");
                ctx.store.rerun_review(r.id, r.claim, RETRY_AFTER).await
            }
        },
    }
}

/// Posts what the fix left the author, on the PR as it is now. A PR that moved while the fix ran
/// gets nothing.
async fn left_to_author(
    ctx: &Ctx,
    gh: &Installation,
    key: &LaneKey,
    r: &ReviewRecord,
    settings: &RepoSettings,
    done: &ReviewDone,
) -> Result<()> {
    let Some(now) = as_reviewed(ctx, gh, key, r, settings).await? else {
        return ctx.store.stale_review(r.id, r.claim).await;
    };
    publish(ctx, gh, key, &now, r, done).await?;
    ctx.store.settle_review(r.id, r.claim).await
}

/// The PR as GitHub has it now, if it is still the one the review is of: the head it reviewed,
/// into the same trunk, told to merge. A pushed or retargeted head gets its own review, and one
/// no longer queued gets none.
async fn as_reviewed(
    ctx: &Ctx,
    gh: &Installation,
    key: &LaneKey,
    r: &ReviewRecord,
    settings: &RepoSettings,
) -> Result<Option<PullRequest>> {
    let now = GitHub::pull_request(gh, &r.repo, r.number).await?;
    // A fork PR's head is its pull ref's, which its GraphQL lags behind.
    let moved = !now.is_cross_repository && now.head_ref_oid != r.head_sha;
    if moved
        || now.merges_into() != key.trunk
        || !merge_requested(ctx.store.as_ref(), gh, &r.repo, &now, &settings.label).await?
    {
        return Ok(None);
    }
    Ok(Some(now))
}

/// What the agent's fix can take on from a review: what it sent back to the author, not what a
/// human asked for, and not on a PR from a fork. Empty otherwise.
fn fixable(r: &ReviewRecord, fork: bool) -> Vec<Finding> {
    if r.status != ReviewStatus::Changes || r.answered_by.is_some() || fork {
        return Vec::new();
    }
    r.findings
        .iter()
        .filter(|f| f.sent_back)
        .map(|f| f.finding.clone())
        .collect()
}

/// The PR, or the stack it is a layer of, held for the agent's fix of what a review sent back.
pub(crate) struct HeldForFix {
    /// Whose budget the fix shares, and whose layers above it move.
    unit: Unit,
    _claim: Claim,
}

/// Holds the PR for the agent's fix, as its review ends, so it goes from reviewing to fixing:
/// `None` on a fork's, or while a job has it, as a fix of its checks on this head.
pub(crate) async fn hold_for_fix(
    ctx: &Ctx,
    gh: &Installation,
    key: &LaneKey,
    pr: &PullRequest,
) -> Result<Option<HeldForFix>> {
    if pr.is_cross_repository {
        return Ok(None);
    }
    let Some(unit) = units(gh, &key.repo, std::slice::from_ref(pr)).await?.pop() else {
        return Ok(None);
    };
    Ok(ctx.prep.claim(key, unit.number).map(|claim| HeldForFix {
        unit,
        _claim: claim,
    }))
}

/// How the agent's fix of what a review sent back ended.
#[derive(Debug, PartialEq)]
pub(crate) enum Fix {
    Pushed,
    /// The PR's branch moved under the fix: the review is of a head it no longer has.
    Superseded,
    /// No fix: the findings wait for the author.
    Unfixed,
}

/// While the PR has fixes left, the agent's fix goes first, while `held`. Without one, or if the
/// agent cannot fix it, the findings wait for the author, as they would anyway.
pub(crate) async fn fix_sent_back(
    ctx: &Ctx,
    gh: &Installation,
    key: &LaneKey,
    pr: &PullRequest,
    settings: &RepoSettings,
    review: i64,
    held: HeldForFix,
) -> Result<Fix> {
    let Some(r) = ctx.store.review(review).await? else {
        return Ok(Fix::Unfixed);
    };
    let findings = fixable(&r, pr.is_cross_repository);
    if findings.is_empty() {
        return Ok(Fix::Unfixed);
    }
    // The review took a while: the PR may have moved on.
    let to_fix = held.unit.to_fix(gh, key, settings, r.number, &r.head_sha);
    let Some((now, layer)) = to_fix.await? else {
        return Ok(Fix::Unfixed);
    };
    let what = format!("{} sent back", findings.len());
    record(ctx, key, r.number, "review-fixing", &what).await?;
    let event = fix_findings(ctx, gh, key, &now, layer, settings, findings).await;
    let fix = match event {
        Ok(Event::Fixed { .. }) => Fix::Pushed,
        Ok(Event::HeadMoved) => Fix::Superseded,
        _ => Fix::Unfixed,
    };
    let (kind, message) = match event {
        Ok(Event::Fixed { head }) => {
            ctx.store.mark_agent_fixed(r.id).await?;
            ("review-fixed", head)
        }
        Ok(Event::FixFailed(why) | Event::FixGaveUp(why)) => ("review-unfixed", why),
        Ok(event) => ("review-unfixed", format!("{event:?}")),
        Err(e) => ("review-unfixed", format!("{e:#}")),
    };
    record(ctx, key, r.number, kind, &message).await?;
    ctx.reviews.notify_one();
    Ok(fix)
}

const NO_KEY: &str = "No model API key";

/// What the review of the author's agent asks of it: the prompt of MergeLoop's own, with what
/// humans answered and what the last review sent back.
async fn author_brief(
    ctx: &Ctx,
    gh: &Installation,
    pr: &PullRequest,
    settings: &RepoSettings,
    r: &ReviewRecord,
) -> Result<String> {
    let (answered, sent_back) = earlier(ctx, gh, r).await?;
    let verify = match settings.verify.is_empty() {
        true => {
            let ci =
                mergepatrol_core::verify_from_ci(&gh.workflows_at(&r.repo, &r.head_sha).await?);
            match ci.is_empty() {
                true => Verify::None,
                false => Verify::Ci(ci),
            }
        }
        false => Verify::Configured(settings.verify.clone()),
    };
    let base = format!("$(git merge-base origin/{} HEAD)", pr.merges_into());
    let req = ReviewRequest {
        workdir: std::path::Path::new("."),
        base: &base,
        pr_title: &pr.title,
        pr_body: &pr.body,
        trunk: pr.merges_into(),
        answered: &answered,
        sent_back: &sent_back,
        verify: &verify,
        queue_commits: &[],
        transcript: std::path::Path::new("."),
    };
    let repo = r.repo.to_string();
    Ok(author_review_brief(&req, &repo, r.number, &r.head_sha))
}

/// The review the author's agent posted, as MergeLoop's own job would have written it. Its
/// evidence is the agent's word that the command failed: nothing runs it again.
fn posted_review(review: &serde_json::Value) -> std::result::Result<ReviewOutput, String> {
    let mut output: ReviewOutput = serde_json::from_value(review.clone())
        .map_err(|e| format!("The review the author's agent posted does not read: {e}."))?;
    if let Some(by) = review["by"].as_str() {
        output.summary = format!("`{by}`'s agent reviewed this. {}", output.summary);
    }
    output.findings.truncate(crate::job::FINDINGS_MAX);
    for (i, f) in output.findings.iter_mut().enumerate() {
        f.id = format!("f{}", i + 1);
        f.confidence = f.confidence.min(100);
        if let Some(e) = &mut f.evidence {
            e.failed = !e.command.trim().is_empty();
        }
    }
    Ok(output)
}

/// What humans answered on earlier heads, and what the last review sent back, with the author's
/// replies in its threads.
async fn earlier(
    ctx: &Ctx,
    gh: &Installation,
    r: &ReviewRecord,
) -> Result<(Vec<Answered>, Vec<SentBack>)> {
    let earlier = earlier_reviews(ctx, r).await?;
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
    Ok((answered, sent_back))
}

/// Runs the review in the sandbox. `Err` says why the agent could not review.
async fn run_review(
    ctx: &Ctx,
    gh: &Installation,
    key: &LaneKey,
    pr: &PullRequest,
    settings: &RepoSettings,
    r: &ReviewRecord,
) -> std::result::Result<ReviewOutput, String> {
    let (answered, sent_back) = earlier(ctx, gh, r).await.map_err(|e| format!("{e:#}"))?;
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
            since: r.since_head.clone(),
        };
        let (spec, granted) =
            job_spec(ctx, gh, key, pr, settings, &r.head_sha, &trunk, None, task).await?;
        if granted.is_none() {
            bail!(
                "{NO_KEY} in the workspace's settings for {}: the agent cannot review.",
                spec.resolver.provider.name()
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
        .reviews_of(r.installation_id, &r.repo, r.number, u32::MAX)
        .await?;
    earlier.retain(|e| {
        e.id != r.id
            && e.head_sha != r.head_sha
            && !matches!(
                e.status,
                ReviewStatus::Pending
                    | ReviewStatus::Running
                    | ReviewStatus::Author
                    | ReviewStatus::Stale
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

/// Whether the author has had their rounds: the last `ROUNDS` reviews each sent findings back to
/// them, and no human stepped in. A round the queue's agent fixed was not theirs.
fn rounds_spent(earlier: &[ReviewRecord]) -> bool {
    let rounds: Vec<_> = earlier
        .iter()
        .filter(|e| !e.agent_fixed)
        .take(ROUNDS)
        .collect();
    rounds.len() == ROUNDS
        && rounds
            .iter()
            .all(|e| e.answered_by.is_none() && e.findings.iter().any(|j| j.sent_back))
}

/// Ends the review: approved when it holds nothing, sent back to the author when it found what
/// they should fix, else it asks the people who own the code in question. `Err` is why the
/// agent could not review: a human reviews the whole PR. A review the agent's fix may take on
/// is not posted: it comes back held for the fix, which goes first.
#[allow(clippy::too_many_arguments)]
async fn finish(
    ctx: &Ctx,
    gh: &Installation,
    key: &LaneKey,
    pr: &PullRequest,
    r: &ReviewRecord,
    settings: &RepoSettings,
    bars: &ReviewSettings,
    output: std::result::Result<ReviewOutput, String>,
) -> Result<Option<(HeldForFix, ReviewDone)>> {
    // The PR may have moved while the agent ran: then the review posts nothing and the PR ends no
    // hold. It is dropped, for a review of a later head to take its place.
    let Some(now) = as_reviewed(ctx, gh, key, r, settings).await? else {
        ctx.store.stale_review(r.id, r.claim).await?;
        return Ok(None);
    };
    let done = judge(ctx, gh, pr, r, bars, output).await?;
    // Held before the review ends: the PR goes from reviewing to fixing, never by "sent back".
    let held = held_for_fix(ctx, gh, key, &now, r, settings, done.status).await;
    if !ctx
        .store
        .finish_review(r.id, r.claim, &done, held.is_some())
        .await?
    {
        return Ok(None);
    }
    let (kind, message) = ended(&done);
    record(ctx, key, r.number, kind, &message).await?;
    match held {
        Some(held) => Ok(Some((held, done))),
        None => {
            publish(ctx, gh, key, &now, r, &done).await?;
            Ok(None)
        }
    }
}

/// The PR held for the agent's fix of what the review sent back, if it fixes it. The author's
/// agent fixes what its own review sent back, as the author.
async fn held_for_fix(
    ctx: &Ctx,
    gh: &Installation,
    key: &LaneKey,
    now: &PullRequest,
    r: &ReviewRecord,
    settings: &RepoSettings,
    status: ReviewStatus,
) -> Option<HeldForFix> {
    if status != ReviewStatus::Changes || settings.agent.runs == Runs::Local {
        return None;
    }
    hold_for_fix(ctx, gh, key, now).await.unwrap_or_else(|e| {
        tracing::warn!(repo = %r.repo, pr = r.number, error = %format!("{e:#}"), "could not hold the PR for a fix");
        None
    })
}

/// What the review holds the PR for, and who it asks: what the agent found, judged by the
/// workspace's bars, or why it could not review.
async fn judge(
    ctx: &Ctx,
    gh: &Installation,
    pr: &PullRequest,
    r: &ReviewRecord,
    bars: &ReviewSettings,
    output: std::result::Result<ReviewOutput, String>,
) -> Result<ReviewDone> {
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
    let status = review_status(&findings, error.is_some());
    let asked = match status {
        ReviewStatus::Asks => askees(gh, &r.repo, pr, &findings).await,
        _ => Vec::new(),
    };
    Ok(ReviewDone {
        status,
        error,
        summary,
        findings,
        asked,
    })
}

/// How the review ended, as the PR's activity says it.
fn ended(done: &ReviewDone) -> (&'static str, String) {
    let all = done.findings.len();
    let count = |of: fn(&Judged) -> bool| done.findings.iter().filter(|f| of(f)).count();
    match (&done.error, done.status) {
        (Some(why), _) => ("review-failed", why.clone()),
        (None, ReviewStatus::Approved) => {
            ("review-approved", format!("cleared {all} things alone"))
        }
        (None, ReviewStatus::Changes) => (
            "review-sent-back",
            format!(
                "sent {} of {all} things back to the author",
                count(|f| f.sent_back)
            ),
        ),
        (None, _) => (
            "review-asks",
            format!("asks a human about {} of {all} things", count(|f| f.asked)),
        ),
    }
}

/// GitHub hears of the review where the PR's people are: each finding that holds the PR in a
/// thread on its line, which its author resolves. Failing to post loses no state, but a finding
/// with no thread waits for a push. A comment, never an approval: an App's approval could meet a
/// rule that asks for a human's.
async fn publish(
    ctx: &Ctx,
    gh: &Installation,
    key: &LaneKey,
    pr: &PullRequest,
    r: &ReviewRecord,
    done: &ReviewDone,
) -> Result<()> {
    match post(ctx, gh, key, pr, r, done).await {
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

/// A title as threads match it: as bounded as the comment that carries it.
fn norm(title: &str) -> String {
    clip(&title.trim().to_lowercase().replace("--", "-"), TITLE_CHARS).into_owned()
}

/// The most characters a comment shows of a finding's title, of its other text (what it found,
/// the change it asks for, the command's output, the review's summary), and of a command or its
/// files. Bounded so, a comment always fits without its test, which goes in only whole.
const TITLE_CHARS: usize = 300;
const TEXT_CHARS: usize = 4_000;
const COMMAND_CHARS: usize = 1_000;

/// `text`, at most `max` characters of it: one cut short ends in `…`.
fn clip(text: &str, max: usize) -> std::borrow::Cow<'_, str> {
    if text.chars().count() <= max {
        return text.into();
    }
    let mut cut: String = text.chars().take(max - 1).collect();
    cut.push('…');
    cut.into()
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
/// finding no longer holds is resolved. A review that leaves nobody anything to do posts no
/// summary. Returns the ids of the findings that have a thread.
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
            let body = still_holds(f, short);
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
            body: thread_body(f, short),
        });
    }
    if done.error.is_some() || !held.is_empty() {
        let body = github_body(ctx, key, r.number, done);
        gh.review_lines(&r.repo, r.number, &r.head_sha, &body, &new)
            .await?;
    }
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
        false => match clip(&f.finding.suggestion, TEXT_CHARS).as_ref() {
            "" => "Fix it and push, or reply with why it doesn't apply. Then resolve this thread."
                .to_owned(),
            change => format!(
                "**Change:** {change}\n\nFix it and push, or reply with why it doesn't apply. Then resolve this thread."
            ),
        },
    }
}

/// How long a finding's comment may be, all of it: GitHub takes up to 65536 characters, and a
/// review that falls back to holding its comments in its body adds a line for each.
const BODY_CHARS: usize = 60_000;

/// A finding's comment, with its test when the whole comment fits, else without it: a part of a
/// patch does not apply.
fn fitted(render: impl Fn(bool) -> String) -> String {
    let whole = render(true);
    match whole.chars().count() <= BODY_CHARS {
        true => whole,
        false => render(false),
    }
}

/// What MergeLoop ran on `head` to show a finding, and what it printed: the author runs the same.
/// With `patch`, the test it wrote, whole.
fn shows_it(e: &Evidence, head: &str, patch: bool) -> String {
    let mut text = format!(
        "**Shows it:** MergeLoop ran `{}` on `{head}` and it failed:\n\n```\n{}\n```\n\n",
        clip(&e.command, COMMAND_CHARS),
        clip(e.output.trim_end(), TEXT_CHARS)
    );
    let files = clip(&e.files.join(", "), COMMAND_CHARS).into_owned();
    match (e.patch.is_empty(), patch) {
        (true, _) => {}
        (false, true) => text.push_str(&format!(
            "<details><summary>With the test it wrote, in {files}</summary>\n\n```diff\n{}\n```\n\n</details>\n\n",
            e.patch.trim_end(),
        )),
        (false, false) => text.push_str(&format!(
            "With the test it wrote, in {files}, too long to show here: the PR's review in MergeLoop has it to download.\n\n"
        )),
    }
    text
}

/// What a finding's evidence showed on `head`, if MergeLoop saw it fail.
fn shown(f: &Judged, head: &str, patch: bool) -> String {
    match &f.finding.evidence {
        Some(e) if e.failed => shows_it(e, head, patch),
        _ => String::new(),
    }
}

/// The reply in a finding's thread when a later head's review finds it again, with the evidence
/// that head's run showed.
fn still_holds(f: &Judged, head: &str) -> String {
    fitted(|patch| {
        format!(
            "Still holds on `{head}`: {}\n\n{}{}\n\n{REPLY_MARK}",
            clip(&f.finding.finding, TEXT_CHARS),
            shown(f, head, patch),
            still(f)
        )
    })
}

/// The comment that starts a finding's thread on `head`.
fn thread_body(f: &Judged, head: &str) -> String {
    let to = match f.asked {
        true => "For a human",
        false => "Sent back to the author",
    };
    let whole = match f.finding.file {
        None => " (about the whole PR)",
        Some(_) => "",
    };
    fitted(|patch| {
        format!(
            "**{to}** · `{}` · {} · {}% sure{whole}\n\n**{}**\n\n{}\n\n{}{}\n\n{MARK}{} -->",
            f.finding.id,
            f.finding.kind.as_str(),
            f.finding.confidence,
            clip(&f.finding.title, TITLE_CHARS),
            clip(&f.finding.finding, TEXT_CHARS),
            shown(f, head, patch),
            still(f),
            norm(&f.finding.title),
        )
    })
}

/// The review's summary on GitHub: what it sent back to the author and asks a human, each in
/// a thread on its line, and what it cleared.
fn github_body(ctx: &Ctx, key: &LaneKey, number: PrNumber, done: &ReviewDone) -> String {
    let url = review_url(&ctx.config.public_url, &key.repo, &key.trunk, number);
    review_body(&url, &key.repo, number, done)
}

/// The review's summary, with `url` where a human answers it: bounded, whatever the agent wrote.
fn review_body(url: &str, repo: &RepoName, number: PrNumber, done: &ReviewDone) -> String {
    let mut body = String::from("**MergeLoop's review**");
    if let Some(why) = &done.error {
        body.push_str(&format!(
            "\n\nThe agent could not review this PR: {}\n\nA human reviews it instead: [approve it, or ask for changes]({url}), or approve it here on GitHub.",
            clip(why, TEXT_CHARS)
        ));
        return body;
    }
    if !done.summary.is_empty() {
        body.push_str(&format!("\n\n{}", clip(&done.summary, TEXT_CHARS)));
    }
    let sent = done.findings.iter().filter(|f| f.sent_back).count();
    let asks = done.findings.iter().filter(|f| f.asked).count();
    // Of the rest, a bug the PR did not bring is one to file; the others it cleared.
    let (mut noted, mut cleared) = (Vec::new(), Vec::new());
    for f in done.findings.iter().filter(|f| !f.asked && !f.sent_back) {
        match f.finding.issue_url(repo, number) {
            Some(issue) => noted.push((f, issue)),
            None => cleared.push(f),
        }
    }
    if sent > 0 {
        body.push_str(&format!(
            "\n\nIt sent {sent} back to the author, each in a thread on its line. For each, fix it and push, or reply with why it doesn't apply, then resolve the thread. Once every thread is resolved, the PR goes on to the merge queue with no human approval."
        ));
    }
    if asks > 0 {
        body.push_str(&asks_section(asks, &done.asked, url));
    }
    body.push_str(&cleared_section(&cleared));
    body.push_str(&noted_section(&noted));
    body
}

/// Who it asks about how many findings, and where they answer.
fn asks_section(asks: usize, asked: &[String], url: &str) -> String {
    let who = match asked {
        [] => "Anyone who may approve this PR".to_owned(),
        logins => logins
            .iter()
            .map(|l| format!("@{l}"))
            .collect::<Vec<_>>()
            .join(", "),
    };
    format!(
        "\n\nIt asks a human about {asks}, each in a thread on its line. {who}: [answer them in MergeLoop]({url}), or approve the PR here on GitHub if they are all fine as they are. The PR keeps its place in the queue meanwhile."
    )
}

/// What it cleared, folded away, each with why when it leaned to a change. Nothing when none.
fn cleared_section(cleared: &[&Judged]) -> String {
    if cleared.is_empty() {
        return String::new();
    }
    let mut section = String::from("\n\n<details><summary>What it cleared</summary>\n");
    for f in cleared {
        let note = match (f.finding.lean, f.finding.shows_the_change()) {
            (Lean::Change, false) => ", a note: nothing showed the change was needed",
            (Lean::Change, true) => ", a note under the bar",
            (Lean::Fine | Lean::PreExisting, _) => "",
        };
        section.push_str(&format!(
            "\n- {} ({}%{note})",
            clip(&f.finding.title, TITLE_CHARS),
            f.finding.confidence
        ));
    }
    section + "\n\n</details>"
}

/// The bugs it found that the PR did not bring, each with its link to file it. Nothing when none.
fn noted_section(noted: &[(&Judged, String)]) -> String {
    if noted.is_empty() {
        return String::new();
    }
    let mut section = String::from(
        "\n\n**Already there before this PR**: bugs in code it did not bring. They are not this PR's to fix, and hold nothing.\n",
    );
    for (f, issue) in noted {
        section.push_str(&format!(
            "\n- {} · [file an issue]({issue})",
            clip(&f.finding.title, TITLE_CHARS)
        ));
    }
    section
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
    if !ctx
        .store
        .answer_question(r.id, r.claim, &q.id, answer)
        .await?
    {
        return Ok(());
    }
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
    use mergepatrol_core::{Bar, Kind};

    #[test]
    fn a_posted_review_is_numbered_and_its_evidence_counts_as_failed() {
        let posted = serde_json::json!({
            "summary": "s",
            "findings": [
                { "kind": "other", "title": "a", "confidence": 120, "lean": "change", "finding": "x",
                  "evidence": { "command": "cargo test a" } },
                { "kind": "other", "title": "b", "confidence": 90, "lean": "fine", "finding": "y" },
            ],
        });
        let output = posted_review(&posted).unwrap();
        let ids: Vec<&str> = output.findings.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, ["f1", "f2"]);
        assert_eq!(output.findings[0].confidence, 100);
        assert!(output.findings[0].shows_the_change());
        assert!(posted_review(&serde_json::json!({ "findings": "no" })).is_err());
        // At most as many as MergeLoop's own agent may report, so the summary fits GitHub.
        let many: Vec<_> = (0..30)
            .map(|i| serde_json::json!({ "kind": "other", "title": format!("t{i}"), "confidence": 90, "lean": "pre_existing", "finding": "x" }))
            .collect();
        let many = serde_json::json!({ "summary": "s", "findings": many });
        assert_eq!(posted_review(&many).unwrap().findings.len(), 25);
        // Whoever posted it is named, in code so GitHub pings no one, as any writer of the repo may review for its author.
        let by = serde_json::json!({ "summary": "s", "findings": [], "by": "octo" });
        assert_eq!(
            posted_review(&by).unwrap().summary,
            "`octo`'s agent reviewed this. s"
        );
    }

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
            evidence: Some(mergepatrol_core::Evidence {
                command: "cargo test".into(),
                failed: true,
                ..Default::default()
            }),
            needs_human: false,
            code: Vec::new(),
            related: Vec::new(),
        };
        ReviewRecord {
            since_head: None,
            agent_fixed: false,
            claim: 0,
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
            brief: None,
            submitted: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn a_findings_thread_carries_its_title_for_the_next_review_to_find() {
        let mut f = review(1, true, None).findings.remove(0);
        f.finding.title = "Retry loop has no cap --> ever".into();
        f.finding.evidence = Some(Evidence {
            command: "cargo test retry_stops".into(),
            files: vec!["tests/retry.rs".into()],
            patch: "+fn retry_stops() {}".into(),
            output: "test retry_stops ... FAILED".into(),
            failed: true,
        });
        let thread = ReviewThread {
            id: "t".into(),
            resolved: false,
            comment_id: 1,
            body: thread_body(&f, "abc1234"),
            reply: None,
        };
        assert_eq!(
            title_of(&thread),
            Some(norm("retry loop has NO cap --> ever "))
        );
        assert!(thread.body.contains("Sent back to the author"));
        assert!(thread.body.contains("Then resolve this thread."));
        // What MergeLoop ran and saw fail, so the author knows what to run and what to see change.
        assert!(
            thread
                .body
                .contains("ran `cargo test retry_stops` on `abc1234` and it failed")
        );
        assert!(thread.body.contains("test retry_stops ... FAILED"));
        assert!(thread.body.contains("in tests/retry.rs"));
        assert!(thread.body.contains("+fn retry_stops() {}"));
        // What the agent only says it ran shows nothing.
        f.finding.evidence.as_mut().unwrap().failed = false;
        assert!(!thread_body(&f, "abc1234").contains("Shows it"));
        // Found again on a later head: the reply shows what that head's run printed.
        f.finding.evidence.as_mut().unwrap().failed = true;
        f.finding.evidence.as_mut().unwrap().output = "test retry_stops ... FAILED again".into();
        let reply = still_holds(&f, "def5678");
        assert!(reply.contains("Still holds on `def5678`"), "{reply}");
        assert!(
            reply.contains("ran `cargo test retry_stops` on `def5678`"),
            "{reply}"
        );
        assert!(reply.contains("FAILED again"), "{reply}");
        // A comment with a test too long for it goes without the test, never with part of it.
        let e = f.finding.evidence.as_mut().unwrap();
        e.patch = format!("+{}", "x".repeat(BODY_CHARS));
        for body in [thread_body(&f, "def5678"), still_holds(&f, "def5678")] {
            assert!(body.chars().count() <= BODY_CHARS);
            assert!(body.contains("too long to show here") && !body.contains("+xxx"));
            assert!(body.contains("FAILED again"));
        }
    }

    #[test]
    fn a_pre_existing_bug_is_a_note_to_file_not_one_the_review_cleared() {
        let repo = RepoName::parse("o/r").unwrap();
        let mut old = review(1, false, None).findings.remove(0).finding;
        old.title = "The retry never stops".into();
        old.lean = Lean::PreExisting;
        let findings = ReviewSettings::default().judge(vec![old.clone()]);
        let done = ReviewDone {
            status: review_status(&findings, false),
            error: None,
            summary: String::new(),
            findings,
            asked: Vec::new(),
        };
        assert_eq!(done.status, ReviewStatus::Approved);
        let body = review_body("https://m.test", &repo, 7, &done);
        let issue = old.issue_url(&repo, 7).unwrap();
        assert!(
            body.contains(&format!(
                "- The retry never stops · [file an issue]({issue})"
            )),
            "{body}"
        );
        assert!(!body.contains("What it cleared"), "{body}");
    }

    /// However long what the agent wrote, a comment fits GitHub without its test, and with it
    /// only whole: the thread, a reply in it, and the review's summary.
    #[test]
    fn every_comment_fits_github_however_long_what_the_agent_wrote() {
        let huge = "y".repeat(200_000);
        let mut f = review(1, true, None).findings.remove(0);
        f.finding.title = huge.clone();
        f.finding.finding = huge.clone();
        f.finding.suggestion = huge.clone();
        f.finding.evidence = Some(Evidence {
            command: huge.clone(),
            files: vec![huge.clone()],
            patch: "+short test".into(),
            output: huge.clone(),
            failed: true,
        });
        for body in [thread_body(&f, "abc1234"), still_holds(&f, "abc1234")] {
            assert!(
                body.chars().count() <= BODY_CHARS,
                "{}",
                body.chars().count()
            );
            assert!(
                body.contains("+short test"),
                "the test fits, so it goes in whole"
            );
        }
        // The thread's marker survives, so the next review still finds it.
        let thread = ReviewThread {
            id: "t".into(),
            resolved: false,
            comment_id: 1,
            body: thread_body(&f, "abc1234"),
            reply: None,
        };
        assert_eq!(title_of(&thread), Some(norm(&huge)));
        assert!(still_holds(&f, "abc1234").ends_with(REPLY_MARK));

        let mut cleared = f.clone();
        cleared.sent_back = false;
        let done = ReviewDone {
            status: ReviewStatus::Changes,
            error: None,
            summary: huge.clone(),
            findings: std::iter::repeat_n(cleared, 25).chain([f]).collect(),
            asked: Vec::new(),
        };
        assert!(
            review_body("https://m.test", &RepoName::parse("o/r").unwrap(), 7, &done)
                .chars()
                .count()
                <= BODY_CHARS
        );
        let failed = ReviewDone {
            error: Some(huge),
            ..done
        };
        assert!(
            review_body(
                "https://m.test",
                &RepoName::parse("o/r").unwrap(),
                7,
                &failed
            )
            .chars()
            .count()
                <= BODY_CHARS
        );
    }

    #[test]
    fn the_agent_fixes_only_what_it_sent_back_on_the_authors_own_pr() {
        let r = review(1, true, None);
        assert_eq!(fixable(&r, false).len(), 1);
        assert!(fixable(&r, true).is_empty(), "a fork");
        assert!(fixable(&review(1, false, None), false).is_empty());
        let asked = review(1, true, Some("ann"));
        assert!(fixable(&asked, false).is_empty(), "a human's call");
        let mut approved = review(1, true, None);
        approved.status = ReviewStatus::Approved;
        assert!(fixable(&approved, false).is_empty());
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
        // The agent's own rounds are not the author's.
        let fixed = |id| ReviewRecord {
            agent_fixed: true,
            claim: 0,
            ..back(id)
        };
        assert!(!rounds_spent(&[fixed(3), fixed(2), fixed(1)]));
        assert!(rounds_spent(&[
            back(5),
            fixed(4),
            back(3),
            fixed(2),
            back(1)
        ]));
        assert_eq!(sent_back_before(&[back(3)]).len(), 1);
        assert!(sent_back_before(&[]).is_empty());
    }
}
