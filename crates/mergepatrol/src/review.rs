//! The agent's review, the queue's first gate. A tick starts a review of each queued PR's head;
//! this worker runs it in the sandbox, sends what the agent is sure should change back to the
//! PR's author, and asks a human only about what the author cannot settle, on GitHub and on the
//! dashboard. It also has the agent answer questions about its findings: a human's, or the
//! author's reply to a finding sent back, which the agent may withdraw on.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use mergepatrol_core::{
    Answered, Depth, Evidence, Finding, Judged, Lean, PrNumber, RepoName, RepoSettings,
    ReviewSettings, ReviewStatus, SentBack, Verdict, Verification, review_status, review_url,
    settled_by,
};
use mergepatrol_executor::protocol::{
    ANSWER_FILE, JobOutcome, JobSpec, REVIEW_FILE, ReviewOutput, Task,
};
use mergepatrol_github::setup::Verdict as ReviewVerdict;
use mergepatrol_github::{App, CodeOwners, Installation, PullRequest, ReviewThread, ThreadComment};
use mergepatrol_resolver::withdraws;
use mergepatrol_store::{LaneKey, ReviewDone, ReviewRecord};
use mergepatrol_web::merge_requested;
use tokio::task::JoinSet;

use crate::github::GitHub;
use crate::queue::{Alive, Ctx, app_credentials, job_spec, record, unix_now};

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
    Ok(file.settings(&defaults).unwrap_or(defaults))
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

    let found = match pr.is_cross_repository {
        // A fork's code is not trusted: no agent reads it, as none rebases or fixes it.
        true => Err("The agent does not review PRs from forks.".to_owned()),
        false => run_review(ctx, &gh, &key, &pr, (&settings, bars.depth), &r).await,
    };
    let output = match found {
        Ok(output) => Ok(output),
        Err(why) if r.attempts + 1 < ATTEMPTS && !why.starts_with(NO_KEY) => {
            ctx.store
                .retry_review(r.id, r.claim, &why, RETRY_AFTER)
                .await?;
            return Ok(());
        }
        Err(why) => Err(why),
    };
    conclude(ctx, &gh, &key, &pr, &r, &settings, &bars, output).await
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

const NO_KEY: &str = "No model API key";

/// What a review goes on from: what humans answered on earlier heads, and what the last review
/// sent back, with the author's replies in its threads, and the head it was of.
struct Earlier {
    answered: Vec<Answered>,
    sent_back: Vec<SentBack>,
    reviewed: Option<String>,
}

async fn earlier(ctx: &Ctx, gh: &Installation, r: &ReviewRecord) -> Result<Earlier> {
    let earlier = earlier_reviews(ctx, r).await?;
    let answered = answered_before(&earlier);
    let threads = threads(gh, r).await;
    let (reviewed, findings) = sent_back_before(&earlier).unzip();
    let sent_back = findings
        .unwrap_or_default()
        .into_iter()
        .map(|finding| {
            let thread = threads
                .iter()
                .find(|t| title_of(t) == Some(norm(&finding.title)));
            SentBack {
                reply: thread
                    .and_then(|t| t.reply.as_ref())
                    .filter(|(_, body)| !REPLY_MARKS.iter().any(|m| body.contains(m)))
                    .map(|(_, body)| body.clone())
                    .unwrap_or_default(),
                resolved: thread.is_some_and(|t| t.resolved),
                finding,
            }
        })
        .collect();
    Ok(Earlier {
        answered,
        sent_back,
        reviewed: reviewed.map(str::to_owned),
    })
}

/// Runs the review in the sandbox. `Err` says why the agent could not review.
async fn run_review(
    ctx: &Ctx,
    gh: &Installation,
    key: &LaneKey,
    pr: &PullRequest,
    (settings, depth): (&RepoSettings, Depth),
    r: &ReviewRecord,
) -> std::result::Result<ReviewOutput, String> {
    let Earlier {
        answered,
        sent_back,
        reviewed,
    } = earlier(ctx, gh, r).await.map_err(|e| format!("{e:#}"))?;
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
            reviewed,
            depth,
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

/// What the last review sent back to the author and still held the PR for, and the head it was
/// of: the next checks the fix, and reads only what changed since.
fn sent_back_before(earlier: &[ReviewRecord]) -> Option<(&str, Vec<Finding>)> {
    let last = earlier.first()?;
    let findings: Vec<_> = last
        .findings
        .iter()
        .filter(|j| j.sent_back)
        .map(|j| j.finding.clone())
        .collect();
    (!findings.is_empty()).then_some((last.head_sha.as_str(), findings))
}

/// Whether the author has had their rounds: the last `ROUNDS` reviews each sent findings back to
/// them, and no human stepped in.
fn rounds_spent(earlier: &[ReviewRecord]) -> bool {
    let rounds: Vec<_> = earlier.iter().take(ROUNDS).collect();
    rounds.len() == ROUNDS
        && rounds
            .iter()
            .all(|e| e.answered_by.is_none() && e.findings.iter().any(|j| j.sent_back))
}

/// Ends the review: approved when it holds nothing, sent back to the author when it found what
/// they should fix, else it asks the people who own the code in question. `Err` is why the
/// agent could not review: a human reviews the whole PR. Then posts it on GitHub.
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
    // The PR may have moved while the agent ran: then the review posts nothing and the PR ends no
    // hold. It is dropped, for a review of a later head to take its place.
    let Some(now) = as_reviewed(ctx, gh, key, r, settings).await? else {
        return ctx.store.stale_review(r.id, r.claim).await;
    };
    let done = judge(ctx, gh, pr, r, bars, output).await?;
    if !ctx.store.finish_review(r.id, r.claim, &done).await? {
        return Ok(());
    }
    settle_earlier(ctx, r, &done).await?;
    let (kind, message) = ended(&done);
    record(ctx, key, r.number, kind, &message).await?;
    publish(ctx, gh, key, &now, r, &done).await
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
    let earlier = earlier_reviews(ctx, r).await?;
    // The author had their rounds and it still finds what to send back: a human decides.
    if findings.iter().any(|f| f.sent_back) && rounds_spent(&earlier) {
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

/// What became of what earlier reviews held the PR for, as this one says. Each earlier review
/// is matched on its own: the last may have settled nothing, as when it asked a human or failed.
/// An outcome already given stands.
async fn settle_earlier(ctx: &Ctx, r: &ReviewRecord, done: &ReviewDone) -> Result<()> {
    let found: Vec<Finding> = done.findings.iter().map(|j| j.finding.clone()).collect();
    for earlier in earlier_reviews(ctx, r).await? {
        let settled = settled_by(&earlier.findings, &earlier.answers, &found);
        if !settled.is_empty() {
            ctx.store.settle_findings(earlier.id, &settled).await?;
        }
    }
    Ok(())
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
/// with no thread waits for a push. What it sends back requests changes, so GitHub holds the PR
/// too; anything else is a comment, never an approval: an App's approval could meet a rule that
/// asks for a human's.
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
        logins = owners_of(&owners, findings);
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

/// The code owners of the files the asks are about, at every place: users, and teams as
/// `org/team`.
fn owners_of(owners: &CodeOwners, findings: &[Judged]) -> Vec<String> {
    let files = findings
        .iter()
        .filter(|f| f.asked)
        .flat_map(|f| f.finding.locations().map(|(file, _)| file));
    files
        .flat_map(|file| owners.owners(file))
        // `@login`, or a team as `@org/team`; not an email address.
        .filter_map(|o| o.strip_prefix('@').map(str::to_owned))
        .collect()
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
/// Each mark after the first is how MergeLoop wrote it as Mergepatrol, on threads still open.
const MARKS: [&str; 2] = ["<!-- mergeloop-finding: ", "<!-- mergepatrol-finding: "];
const MARK: &str = MARKS[0];
/// End the body of each of MergeLoop's reviews, so its request for changes is dismissed once
/// what it sent back no longer holds.
pub(crate) const REVIEW_MARKS: [&str; 2] =
    ["<!-- mergeloop-review -->", "<!-- mergepatrol-review -->"];
const REVIEW_MARK: &str = REVIEW_MARKS[0];
/// End each of MergeLoop's replies in a thread, so a reply is not taken for the author's.
const REPLY_MARKS: [&str; 2] = ["<!-- mergeloop-reply -->", "<!-- mergepatrol-reply -->"];
const REPLY_MARK: &str = REPLY_MARKS[0];

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
    let rest = MARKS
        .iter()
        .find_map(|m| Some(&t.body[t.body.find(m)? + m.len()..]))?;
    Some(rest[..rest.find(" -->")?].to_owned())
}

/// MergeLoop's threads on the PR. None when GitHub could not say.
async fn threads(gh: &Installation, r: &ReviewRecord) -> Vec<ReviewThread> {
    gh.threads_marked(&r.repo, r.number, &MARKS)
        .await
        .inspect_err(|e| {
            tracing::warn!(repo = %r.repo, pr = r.number, error = %format!("{e:#}"), "could not read the review threads");
        })
        .unwrap_or_default()
}

/// Posts the review: a summary, and a thread for each finding that holds the PR. A finding a
/// thread is already about gets a reply there, and its thread opens again; a thread whose
/// finding no longer holds is resolved, and a request for changes of an earlier review that no
/// longer holds is dismissed. A review that leaves nobody anything to do posts no summary.
/// Returns the ids of the findings that have a thread.
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
    let sent_back = held.iter().any(|f| f.sent_back);
    if done.error.is_some() || !held.is_empty() {
        let body = format!("{}\n\n{REVIEW_MARK}", github_body(ctx, key, r.number, done));
        let verdict = match sent_back {
            true => ReviewVerdict::RequestChanges,
            false => ReviewVerdict::Comment,
        };
        gh.review_lines(&r.repo, r.number, &r.head_sha, verdict, &body, &new)
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
    if !sent_back {
        let why = format!("Nothing it asked for holds since the review of `{short}`.");
        let dismissed = gh.dismiss_reviews_marked(&r.repo, r.number, &REVIEW_MARKS, &why);
        if let Err(e) = dismissed.await {
            tracing::warn!(repo = %r.repo, pr = r.number, error = %format!("{e:#}"), "could not dismiss the review's request for changes");
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

/// What showed a finding's change is needed on `head`: its evidence, if MergeLoop saw it fail,
/// else its panel's votes.
fn shown(f: &Judged, head: &str, patch: bool) -> String {
    match (&f.finding.evidence, &f.finding.verified) {
        (Some(e), _) if e.failed => shows_it(e, head, patch),
        (_, Some(v)) => voted(v),
        _ => String::new(),
    }
}

/// What a finding's panel said of it.
fn voted(v: &Verification) -> String {
    let n = v.votes.len();
    let whys: String = v
        .votes
        .iter()
        .map(|v| {
            format!(
                "\n- {}: {}",
                verdict(v.verdict),
                clip(v.why.trim(), TEXT_CHARS)
            )
        })
        .collect();
    format!(
        "**Checked:** MergeLoop had {n} fresh agents check it, each given only this finding and the code: {} of {n} say it holds, and {} had to.\n\n<details><summary>What each said</summary>\n{whys}\n\n</details>\n\n",
        v.holds(),
        v.needed,
    )
}

fn verdict(v: Verdict) -> &'static str {
    match v {
        Verdict::Holds => "holds",
        Verdict::DoesNotHold => "does not hold",
        Verdict::Abstained => "no vote",
    }
}

/// The other places a bug that recurs is, for its author to fix at each.
fn also_at(f: &Judged) -> String {
    let others = f.finding.also_at.iter().map(|p| p.to_string());
    places("Also at", others.collect())
}

/// `places` under `label`, each in code, asking for a fix at each when there are more than one.
fn places(label: &str, places: Vec<String>) -> String {
    let each = match places.len() {
        0 => return String::new(),
        1 => "",
        _ => " Fix it at each place.",
    };
    let places: Vec<String> = places.iter().map(|p| format!("`{p}`")).collect();
    format!(
        "**{label}:** {}.{each}\n\n",
        clip(&places.join(", "), TEXT_CHARS)
    )
}

/// The reply in a finding's thread when a later head's review finds it again, with the evidence
/// that head's run showed.
fn still_holds(f: &Judged, head: &str) -> String {
    fitted(|patch| {
        format!(
            "Still holds on `{head}`: {}\n\n{}{}{}\n\n{REPLY_MARK}",
            clip(&f.finding.finding, TEXT_CHARS),
            places("Still at", f.finding.places()),
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
            "**{to}** · `{}` · {} · {}% sure{whole}\n\n**{}**\n\n{}\n\n{}{}{}\n\n{MARK}{} -->",
            f.finding.id,
            f.finding.kind.as_str(),
            f.finding.confidence,
            clip(&f.finding.title, TITLE_CHARS),
            clip(&f.finding.finding, TEXT_CHARS),
            also_at(f),
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

/// Why a change the review cleared is only a note. Empty for one it found fine.
fn cleared_note(f: &Finding) -> String {
    match (f.lean, f.shows_the_change(), &f.verified) {
        (Lean::Fine | Lean::PreExisting, ..) => String::new(),
        (Lean::Change, true, _) => ", a note under the bar".into(),
        (Lean::Change, false, Some(v)) => format!(
            ", a note: {} of {} fresh agents said it holds, and {} had to",
            v.holds(),
            v.votes.len(),
            v.needed
        ),
        (Lean::Change, false, None) => ", a note: nothing showed the change was needed".into(),
    }
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
        let note = cleared_note(&f.finding);
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
    let withdrawn = answer
        .as_deref()
        .ok()
        .filter(|_| sent_back)
        .and_then(withdraws);
    if !ctx
        .store
        .answer_question(r.id, r.claim, &q.id, answer)
        .await?
    {
        return Ok(());
    }
    if let Some(outcome) = withdrawn
        && let Some(status) = ctx
            .store
            .withdraw_finding(r.id, &q.finding, outcome)
            .await?
    {
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
            verified: None,
            settled: None,
            code: Vec::new(),
            related: Vec::new(),
            also_at: Vec::new(),
        };
        ReviewRecord {
            since_head: None,
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
                outcome: None,
            }],
            asked: Vec::new(),
            answers: Default::default(),
            answered_by: answered_by.map(str::to_owned),
            note: String::new(),
            questions: Vec::new(),
            waited_secs: None,
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
        // A bug that recurs names every other place it is, here and in each later reply.
        f.finding.also_at = vec![
            mergepatrol_core::Place {
                file: "b.rs".into(),
                line: Some(9),
                code: Vec::new(),
            },
            mergepatrol_core::Place {
                file: "c.rs".into(),
                line: None,
                code: Vec::new(),
            },
        ];
        let thread = thread_body(&f, "def5678");
        assert!(
            thread.contains("**Also at:** `b.rs:9`, `c.rs`. Fix it at each place."),
            "{thread}"
        );
        // A later reply names every place it still holds, its own too: the thread sits on the
        // first place it held, which a fix there moves on from.
        f.finding.file = Some("b.rs".into());
        f.finding.line = Some(9);
        f.finding.also_at.remove(0);
        let reply = still_holds(&f, "def5678");
        assert!(
            reply.contains("**Still at:** `b.rs:9`, `c.rs`. Fix it at each place."),
            "{reply}"
        );
        f.finding.also_at.clear();
        let reply = still_holds(&f, "def5678");
        assert!(reply.contains("**Still at:** `b.rs:9`.\n"), "{reply}");
        f.finding.file = None;
        assert!(!still_holds(&f, "def5678").contains("Still at"));
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
    fn a_security_findings_thread_and_note_say_what_its_panel_voted() {
        use mergepatrol_core::{Panel, Verdict, Vote};
        let vote = |verdict| Vote {
            verdict,
            why: "The header reaches it.".into(),
        };
        let mut f = review(1, true, None).findings.remove(0);
        f.finding.kind = Kind::Security;
        f.finding.evidence = None;
        let cast = vec![
            vote(Verdict::Holds),
            vote(Verdict::Holds),
            vote(Verdict::DoesNotHold),
        ];
        f.finding.verified = Some(Panel::FINDING.verdict(cast.clone()));
        let body = thread_body(&f, "abc1234");
        assert!(
            body.contains("MergeLoop had 3 fresh agents check it, each given only this finding and the code: 2 of 3 say it holds, and 2 had to."),
            "{body}"
        );
        assert!(body.contains("The header reaches it."));
        // Voted down, it is a note that says so.
        let down = vec![
            vote(Verdict::Holds),
            vote(Verdict::DoesNotHold),
            vote(Verdict::Abstained),
        ];
        f.finding.verified = Some(Panel::FINDING.verdict(down));
        f.sent_back = false;
        let done = ReviewDone {
            status: ReviewStatus::Approved,
            error: None,
            summary: String::new(),
            findings: vec![f],
            asked: Vec::new(),
        };
        let body = review_body("https://m.test", &RepoName::parse("o/r").unwrap(), 7, &done);
        assert!(
            body.contains("a note: 1 of 3 fresh agents said it holds, and 2 had to"),
            "{body}"
        );
    }

    /// However long what the agent wrote, a comment fits GitHub without its test, and with it
    /// only whole: the thread, a reply in it, and the review's summary.
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
    fn a_human_is_asked_by_the_code_owners_of_every_place_a_finding_is() {
        let owners = CodeOwners::parse("main", "/a.rs @ann\n/b.rs @org/bees\n/c.rs bo@x.io\n");
        let mut asked = review(1, false, None).findings.remove(0);
        asked.asked = true;
        asked.finding.file = Some("a.rs".into());
        asked.finding.also_at = ["b.rs", "c.rs"]
            .map(|file| mergepatrol_core::Place {
                file: file.into(),
                line: None,
                code: Vec::new(),
            })
            .to_vec();
        let mut sent = asked.clone();
        sent.asked = false;
        sent.finding.file = Some("z.rs".into());
        assert_eq!(owners_of(&owners, &[asked, sent]), ["ann", "org/bees"]);
    }

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
        // The next review checks the fix, and reads only what changed since the head it was of.
        let earlier = [back(3), back(2)];
        let (head, findings) = sent_back_before(&earlier).unwrap();
        assert_eq!((head, findings.len()), ("head3", 1));
        assert!(sent_back_before(&[review(3, false, None), back(2)]).is_none());
        assert!(sent_back_before(&[]).is_none());
    }
}
