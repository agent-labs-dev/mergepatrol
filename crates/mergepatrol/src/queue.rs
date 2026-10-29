//! Drives one repo: polls GitHub, keeps the lane in sync, and runs the PR at the head of the lane
//! through rebase, checks, and merge.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use std::path::Path;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use mergepatrol_core::{
    Candidate, EjectReason, Event, Job, Lane, LaneChange, Phase, PrNumber, RepoName, Transition,
};
use mergepatrol_executor::Executor;
use mergepatrol_executor::protocol::{BUNDLE_FILE, JobOutcome, JobSpec, ResolverSpec};
use mergepatrol_git::{Git, cache_path};
use mergepatrol_github::{
    App, AppCredentials, BranchMerge, CheckState, Installation, MergeOutcome, PullRequest,
    RepoSnapshot,
};
use mergepatrol_store::{ANTHROPIC_API_KEY, GITHUB_APP, LaneKey, Scope, Store, Vault};
use tokio::sync::{Notify, Semaphore};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::config::Config;
use crate::github::GitHub;
use crate::publish::{Publish, publish};

const CHECKS_POLL: Duration = Duration::from_secs(15);
/// With no required checks, a commit that has no checks at all after this long counts as green.
const NO_CHECKS_GRACE: Duration = Duration::from_secs(120);
/// How soon a repo turned on in the dashboard starts its queue.
const SUPERVISE_EVERY: Duration = Duration::from_secs(10);
const RESOLVER_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const RESOLVER_MAX_TURNS: u32 = 50;
const MAX_BUNDLE_BYTES: u64 = 200 * 1024 * 1024;
const MERGE_RETRIES: u32 = 3;
const MERGE_RETRY_AFTER: Duration = Duration::from_secs(5);

pub struct Ctx {
    pub config: Config,
    /// The hosted GitHub App. `None` self-hosted: the App is in the vault.
    pub hosted_app: Option<AppCredentials>,
    pub vault: Arc<Vault>,
    pub store: Arc<dyn Store>,
    pub executor: Arc<dyn Executor>,
    pub jobs: Semaphore,
    pub wakers: Arc<Wakers>,
}

/// Webhooks wake a repo's queue before its poll is due. Polling still runs, in case one is lost.
#[derive(Default)]
pub struct Wakers(std::sync::Mutex<HashMap<(u64, RepoName), Arc<Notify>>>);

impl Wakers {
    fn get(&self, installation: u64, repo: &RepoName) -> Arc<Notify> {
        let mut map = self.0.lock().unwrap();
        map.entry((installation, repo.clone())).or_default().clone()
    }

    /// Waits `timeout`, or less if a webhook arrives for the repo meanwhile, or arrived since
    /// the last wait.
    async fn wait(&self, installation: u64, repo: &RepoName, timeout: Duration) {
        let woken = self.get(installation, repo);
        let _ = tokio::time::timeout(timeout, woken.notified()).await;
    }
}

impl mergepatrol_web::Wake for Wakers {
    fn wake(&self, installation: u64, repo: RepoName) {
        self.get(installation, &repo).notify_one();
    }
}

/// Runs a queue for each repo that is on in the dashboard, and stops the queues of repos that
/// are turned off. Waits until the GitHub App exists.
pub async fn supervise(ctx: Arc<Ctx>) {
    let mut running = Running::new();
    loop {
        if let Err(e) = reconcile(&ctx, &mut running).await {
            tracing::error!(error = %format!("{e:#}"), "could not update the running queues");
        }
        tokio::time::sleep(SUPERVISE_EVERY).await;
    }
}

/// The GitHub App: the hosted one from the environment, or the self-hosted one from the vault.
async fn app_credentials(ctx: &Ctx) -> Result<Option<AppCredentials>> {
    if let Some(app) = &ctx.hosted_app {
        return Ok(Some(app.clone()));
    }
    match ctx.vault.get(Scope::Instance, GITHUB_APP).await? {
        Some(json) => Ok(Some(serde_json::from_str(&json)?)),
        None => Ok(None),
    }
}

/// Keyed by (installation, repo). The value keeps the App ID the task started with.
type Running = HashMap<(u64, RepoName), (u64, JoinHandle<()>)>;

async fn reconcile(ctx: &Arc<Ctx>, running: &mut Running) -> Result<()> {
    let Some(creds) = app_credentials(ctx).await? else {
        return Ok(());
    };
    let wanted: Vec<(u64, RepoName)> = ctx
        .store
        .repo_settings()
        .await?
        .into_iter()
        .filter(|s| s.enabled)
        .map(|s| (s.installation_id, s.repo))
        .collect();

    running.retain(|key, (app_id, task)| {
        // A new App (set up again) means new credentials: restart.
        let keep = wanted.contains(key) && *app_id == creds.id && !task.is_finished();
        if !keep {
            tracing::info!(installation = key.0, repo = %key.1, "stopping");
            task.abort();
        }
        keep
    });
    for key in wanted {
        if let std::collections::hash_map::Entry::Vacant(slot) = running.entry(key) {
            let (installation, repo) = slot.key().clone();
            tracing::info!(installation, %repo, "watching");
            let gh = App::from_credentials(&creds)?.installation(installation);
            slot.insert((creds.id, tokio::spawn(run_repo(ctx.clone(), gh, repo))));
        }
    }
    Ok(())
}

async fn run_repo(ctx: Arc<Ctx>, gh: Installation, repo: RepoName) {
    let rebase = JobRebase { ctx: &ctx, gh: &gh };
    loop {
        match tick(&ctx, &gh, &rebase, &repo).await {
            // The lane may have a next PR: start it now, not a poll later.
            Ok(Ran::Job) => continue,
            Ok(Ran::Idle) => {}
            Err(e) => {
                tracing::error!(installation = gh.id, %repo, error = %format!("{e:#}"), "tick failed")
            }
        }
        ctx.wakers.wait(gh.id, &repo, ctx.config.poll).await;
    }
}

enum Ran {
    Job,
    Idle,
}

enum Outcome {
    Merged(String),
    Ejected(EjectReason),
    Held(String),
    /// The label was removed while the job ran.
    Cancelled,
}

async fn tick(ctx: &Ctx, gh: &dyn GitHub, rebase: &dyn Rebase, repo: &RepoName) -> Result<Ran> {
    let labels = &ctx.config.labels;
    let snap = gh.snapshot(repo, &labels.trigger).await?;
    let key = LaneKey {
        installation_id: gh.installation_id(),
        repo: repo.clone(),
        trunk: snap.default_branch.clone(),
    };
    let mut lane = ctx.store.load_lane(&key).await?;

    let mut candidates = Vec::new();
    for pr in &snap.labeled {
        if pr.is_cross_repository {
            eject(
                ctx,
                gh,
                &key,
                pr.number,
                "PRs from forks are not supported yet.",
            )
            .await?;
            continue;
        }
        let hold = if pr.base_ref_name != snap.default_branch {
            Some(format!(
                "targets `{}`; only `{}` has a queue for now",
                pr.base_ref_name, snap.default_branch
            ))
        } else {
            pr.hold_reason()
        };
        candidates.push(Candidate {
            number: pr.number,
            priority: pr.has_label(&labels.priority),
            hold,
        });
    }

    for change in lane.sync(&candidates) {
        match change {
            LaneChange::Enqueued(n) => {
                record(ctx, &key, n, "enqueued", "added to the queue").await?
            }
            LaneChange::Dequeued { number, .. } => {
                record(ctx, &key, number, "dequeued", "left the queue").await?
            }
        }
    }
    ctx.store.save_lane(&key, &lane).await?;

    // Jobs run inside the tick, so a job still active here was cut short by a restart. A single
    // PR runs again; a batch is built again.
    if let Some(branch) = lane.batch_branch.take() {
        gh.delete_branch(repo, &branch).await?;
        lane.stop();
        ctx.store.save_lane(&key, &lane).await?;
    }
    let mut next = lane.active();
    if next.is_empty() {
        next = lane.next(ctx.config.batch_size);
    }
    let Some(&number) = next.first() else {
        return Ok(Ran::Idle);
    };
    let required = gh.required_checks(repo, &key.trunk).await?;

    // GitHub merges a PR behind the trunk, which a batch's PRs are, unless the trunk is strict.
    if next.len() > 1 && !required.strict {
        let prs = batchable(gh, repo, &snap, &required.names, &next).await?;
        if let Some(batch) = build_batch(ctx, gh, &key, &snap, &mut lane, &prs).await? {
            run_batch(ctx, gh, &key, &snap, &required.names, &mut lane, batch).await?;
            return Ok(Ran::Job);
        }
    }

    lane.activate(number);
    ctx.store.save_lane(&key, &lane).await?;

    let _slot = ctx.jobs.acquire().await?;
    let outcome = match run_job(ctx, gh, rebase, &key, &snap, &required.names, number).await {
        Ok(o) => o,
        Err(e) => Outcome::Ejected(EjectReason::RebaseFailed(format!("{e:#}"))),
    };
    finish(ctx, gh, &key, &mut lane, number, outcome).await?;
    Ok(Ran::Job)
}

/// The PRs of `next` that can join a batch as they are, in order, up to the first that cannot:
/// GitHub must merge each without a rebase, so its own required checks have passed.
async fn batchable(
    gh: &dyn GitHub,
    repo: &RepoName,
    snap: &RepoSnapshot,
    required: &[String],
    next: &[PrNumber],
) -> Result<Vec<PullRequest>> {
    let mut prs = Vec::new();
    for number in next {
        let Some(pr) = snap.labeled.iter().find(|p| p.number == *number) else {
            break;
        };
        let checks = gh.checks(repo, &pr.head_ref_oid).await?;
        if checks.verdict(required).0 != CheckState::Passed {
            break;
        }
        prs.push(pr.clone());
    }
    Ok(prs)
}

async fn finish(
    ctx: &Ctx,
    gh: &dyn GitHub,
    key: &LaneKey,
    lane: &mut Lane,
    number: PrNumber,
    outcome: Outcome,
) -> Result<()> {
    match outcome {
        Outcome::Merged(sha) => {
            lane.remove(number);
            record(ctx, key, number, "merged", &sha).await?;
            gh.sticky_comment(&key.repo, number, &format!("Merged as {sha}."))
                .await?;
            let labels = &ctx.config.labels;
            for label in [&labels.trigger, &labels.priority] {
                gh.remove_label(&key.repo, number, label).await?;
            }
        }
        Outcome::Ejected(reason) => {
            lane.eject(number);
            eject(ctx, gh, key, number, &format!("Ejected: {reason}.")).await?;
        }
        Outcome::Held(reason) => {
            lane.hold(number, reason.clone());
            record(ctx, key, number, "held", &reason).await?;
            gh.sticky_comment(
                &key.repo,
                number,
                &format!("On hold: {reason}. It stays in its place in the queue."),
            )
            .await?;
        }
        Outcome::Cancelled => {
            lane.remove(number);
            record(ctx, key, number, "cancelled", "the label was removed").await?;
        }
    }
    ctx.store.save_lane(key, lane).await
}

/// PRs merged together on a branch of their own, which a draft PR puts through CI.
struct Batch {
    branch: String,
    /// Each PR, with the merge commit it made on the branch.
    steps: Vec<(PullRequest, BranchMerge)>,
}

/// Merges `prs` one after the other onto a new branch at the trunk, up to the first that
/// conflicts. `None` when fewer than two merged: the first PR then runs alone.
async fn build_batch(
    ctx: &Ctx,
    gh: &dyn GitHub,
    key: &LaneKey,
    snap: &RepoSnapshot,
    lane: &mut Lane,
    prs: &[PullRequest],
) -> Result<Option<Batch>> {
    if prs.len() < 2 {
        return Ok(None);
    }
    let repo = &key.repo;
    let numbers: Vec<String> = prs.iter().map(|p| p.number.to_string()).collect();
    let branch = format!("mergepatrol/batch-{}", numbers.join("-"));
    lane.batch_branch = Some(branch.clone());
    ctx.store.save_lane(key, lane).await?;

    // A batch of the same PRs that stopped earlier left its branch.
    gh.delete_branch(repo, &branch).await?;
    gh.create_branch(repo, &branch, &snap.trunk_sha).await?;
    let mut steps = Vec::new();
    for pr in prs {
        let message = format!("Merge #{} into the batch", pr.number);
        match gh
            .merge_into_branch(repo, &branch, &pr.head_ref_oid, &message)
            .await?
        {
            Some(m) => steps.push((pr.clone(), m)),
            None => break,
        }
    }
    if steps.len() < 2 {
        gh.delete_branch(repo, &branch).await?;
        lane.batch_branch = None;
        ctx.store.save_lane(key, lane).await?;
        return Ok(None);
    }
    Ok(Some(Batch { branch, steps }))
}

/// How a batch's checks ended.
enum Tested {
    Passed,
    Failed(Vec<String>),
    /// Something changed under the batch, so what it tested is stale.
    Stale(String),
}

/// Puts a batch through CI on a draft PR. When it passes, merges its PRs in order, each only
/// while the trunk comes out as the tree the batch tested. The PRs it does not finish go back
/// to their places.
async fn run_batch(
    ctx: &Ctx,
    gh: &dyn GitHub,
    key: &LaneKey,
    snap: &RepoSnapshot,
    required: &[String],
    lane: &mut Lane,
    batch: Batch,
) -> Result<()> {
    let repo = &key.repo;
    let members: Vec<PrNumber> = batch.steps.iter().map(|(pr, _)| pr.number).collect();
    for n in &members {
        lane.activate(*n);
    }
    ctx.store.save_lane(key, lane).await?;

    let result = test_and_merge(ctx, gh, key, snap, required, lane, &batch, &members).await;
    // Deleting the branch closes the draft PR.
    gh.delete_branch(repo, &batch.branch).await?;
    lane.batch_branch = None;
    lane.stop();
    ctx.store.save_lane(key, lane).await?;
    result
}

#[allow(clippy::too_many_arguments)]
async fn test_and_merge(
    ctx: &Ctx,
    gh: &dyn GitHub,
    key: &LaneKey,
    snap: &RepoSnapshot,
    required: &[String],
    lane: &mut Lane,
    batch: &Batch,
    members: &[PrNumber],
) -> Result<()> {
    let repo = &key.repo;
    let (first, _) = &batch.steps[0];
    let refs = |except: PrNumber| -> String {
        members
            .iter()
            .filter(|n| **n != except)
            .map(|n| format!("#{n}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    // `#n` in code spans: plain references would add a timeline event to every PR, on every batch.
    let list: Vec<String> = batch
        .steps
        .iter()
        .map(|(pr, _)| format!("- `#{}` {}", pr.number, pr.title))
        .collect();
    let title = format!(
        "{} (+{} in a Mergepatrol batch)",
        first.title,
        members.len() - 1
    );
    let body = format!(
        "Mergepatrol tests these PRs together, then merges them one by one. It closes this PR when it is done: do not merge it.\n\n{}",
        list.join("\n")
    );
    let draft = gh
        .open_draft_pr(repo, &batch.branch, &key.trunk, &title, &body)
        .await?;
    for n in members {
        record(
            ctx,
            key,
            *n,
            "batch",
            &format!("tested with {} in #{draft}", refs(*n)),
        )
        .await?;
        gh.sticky_comment(
            repo,
            *n,
            &format!("Testing together with {} in #{draft}.", refs(*n)),
        )
        .await?;
    }

    let head = &batch.steps.last().expect("a batch has PRs").1.sha;
    let opened = Instant::now();
    let tested = loop {
        let now = gh.snapshot(repo, &ctx.config.labels.trigger).await?;
        if let Some(why) = stale(&now, snap, batch) {
            break Tested::Stale(why);
        }
        let checks = gh.checks(repo, head).await?;
        match checks.verdict(required) {
            (CheckState::Passed, _) if required.is_empty() && checks.by_name.is_empty() => {
                if opened.elapsed() >= NO_CHECKS_GRACE {
                    break Tested::Passed;
                }
            }
            (CheckState::Passed, _) => break Tested::Passed,
            (CheckState::Failed, names) => break Tested::Failed(names),
            (CheckState::Pending, _) => {}
        }
        ctx.wakers
            .wait(key.installation_id, repo, CHECKS_POLL)
            .await;
    };

    match tested {
        Tested::Stale(why) => {
            for n in members {
                record(ctx, key, *n, "batch-stopped", &why).await?;
            }
            Ok(())
        }
        Tested::Failed(names) => {
            lane.batch_failed(members);
            let why = format!("required checks failed: {}", names.join(", "));
            for n in members {
                record(ctx, key, *n, "batch-failed", &why).await?;
                gh.sticky_comment(
                    repo,
                    *n,
                    &format!("The batch with {} failed: {why}. Mergepatrol now tests these PRs in smaller batches, to find the one that broke it.", refs(*n)),
                )
                .await?;
            }
            Ok(())
        }
        Tested::Passed => merge_batch(ctx, gh, key, snap, lane, batch).await,
    }
}

/// Why what the batch tested no longer stands: the trunk moved, or one of its PRs changed.
fn stale(now: &RepoSnapshot, then: &RepoSnapshot, batch: &Batch) -> Option<String> {
    if now.trunk_sha != then.trunk_sha {
        return Some("the trunk moved outside the queue".into());
    }
    for (pr, _) in &batch.steps {
        let Some(current) = now.labeled.iter().find(|p| p.number == pr.number) else {
            return Some(format!("#{} left the queue", pr.number));
        };
        if current.head_ref_oid != pr.head_ref_oid {
            return Some(format!("#{} was pushed to", pr.number));
        }
        if let Some(hold) = current.hold_reason() {
            return Some(format!("#{} is on hold: {hold}", pr.number));
        }
    }
    None
}

/// Merges the PRs of a batch that passed, in its order. Each merge must leave the trunk at the
/// tree of the batch's commit for that PR; otherwise the rest would merge untested.
async fn merge_batch(
    ctx: &Ctx,
    gh: &dyn GitHub,
    key: &LaneKey,
    snap: &RepoSnapshot,
    lane: &mut Lane,
    batch: &Batch,
) -> Result<()> {
    let repo = &key.repo;
    for (tested, step) in &batch.steps {
        let n = tested.number;
        let pr = gh.pull_request(repo, n).await?;
        if !pr.has_label(&ctx.config.labels.trigger)
            || gh.branch_sha(repo, &pr.head_ref_name).await? != tested.head_ref_oid
        {
            record(
                ctx,
                key,
                n,
                "batch-stopped",
                "the PR changed before its merge",
            )
            .await?;
            return Ok(());
        }
        if let Some(reason) = pr.hold_reason() {
            return finish(ctx, gh, key, lane, n, Outcome::Held(reason)).await;
        }
        match merge_retrying(gh, repo, n, &tested.head_ref_oid, &snap.merge_method).await? {
            MergeOutcome::Merged { sha } => {
                let tree = gh.commit_tree(repo, &sha).await?;
                finish(ctx, gh, key, lane, n, Outcome::Merged(sha.clone())).await?;
                if tree != step.tree {
                    record(
                        ctx,
                        key,
                        n,
                        "diverged",
                        &format!(
                            "the trunk came out as tree {tree}, not the batch's {}",
                            step.tree
                        ),
                    )
                    .await?;
                    return Ok(());
                }
            }
            MergeOutcome::Rejected(r) => {
                let reason = EjectReason::MergeRejected(r);
                return finish(ctx, gh, key, lane, n, Outcome::Ejected(reason)).await;
            }
        }
    }
    Ok(())
}

/// GitHub rejects a merge right after the one before it, until it has worked out the PR's
/// mergeability against the new trunk ("Base branch was modified"). It accepts it moments later.
async fn merge_retrying(
    gh: &dyn GitHub,
    repo: &RepoName,
    number: PrNumber,
    head: &str,
    method: &str,
) -> Result<MergeOutcome> {
    let mut tries = 0;
    loop {
        match gh.merge(repo, number, head, method).await? {
            MergeOutcome::Rejected(_) if tries < MERGE_RETRIES => {
                tries += 1;
                tokio::time::sleep(MERGE_RETRY_AFTER).await;
            }
            outcome => return Ok(outcome),
        }
    }
}

/// A queue event: shown on the dashboard, and logged.
async fn record(
    ctx: &Ctx,
    key: &LaneKey,
    number: PrNumber,
    kind: &str,
    message: &str,
) -> Result<()> {
    tracing::info!(repo = %key.repo, pr = number, kind, detail = message, "queue");
    ctx.store.record(key, number, kind, message).await
}

async fn eject(
    ctx: &Ctx,
    gh: &dyn GitHub,
    key: &LaneKey,
    number: PrNumber,
    why: &str,
) -> Result<()> {
    let labels = &ctx.config.labels;
    gh.remove_label(&key.repo, number, &labels.trigger).await?;
    gh.add_label(&key.repo, number, &labels.failed).await?;
    gh.sticky_comment(
        &key.repo,
        number,
        &format!("{why}\n\nAdd `{}` again to retry.", labels.trigger),
    )
    .await?;
    record(ctx, key, number, "ejected", why).await
}

async fn run_job(
    ctx: &Ctx,
    gh: &dyn GitHub,
    rebase: &dyn Rebase,
    key: &LaneKey,
    snap: &RepoSnapshot,
    required: &[String],
    number: PrNumber,
) -> Result<Outcome> {
    let repo = &key.repo;
    let mut job = Job::start(number);
    let mut published_at = Instant::now();

    loop {
        let pr = gh.pull_request(repo, number).await?;
        if !pr.has_label(&ctx.config.labels.trigger) {
            return Ok(Outcome::Cancelled);
        }

        let event = match &job.phase {
            Phase::Rebasing => {
                record(ctx, key, number, "rebasing", &format!("onto {}", key.trunk)).await?;
                gh.sticky_comment(repo, number, &format!("Rebasing onto `{}`.", key.trunk))
                    .await?;
                let event = rebase.rebase(key, snap, &pr).await?;
                published_at = Instant::now();
                if let Event::Rebased { head, .. } = &event {
                    record(ctx, key, number, "published", head).await?;
                    gh.sticky_comment(
                        repo,
                        number,
                        &format!("Rebased to {head}. Waiting for checks."),
                    )
                    .await?;
                }
                event
            }
            Phase::AwaitingChecks { head, base } => {
                let trunk = gh.branch_sha(repo, &key.trunk).await?;
                // The ref, not `pr.head_ref_oid`: the PR lags behind our own publish for a few seconds.
                if gh.branch_sha(repo, &pr.head_ref_name).await? != *head {
                    record(
                        ctx,
                        key,
                        number,
                        "head-moved",
                        "the PR branch was pushed to",
                    )
                    .await?;
                    Event::HeadMoved
                } else if trunk != *base {
                    record(
                        ctx,
                        key,
                        number,
                        "base-moved",
                        "the trunk moved outside the queue",
                    )
                    .await?;
                    Event::BaseMoved
                } else {
                    let checks = gh.checks(repo, head).await?;
                    match checks.verdict(required) {
                        (CheckState::Passed, _)
                            if required.is_empty() && checks.by_name.is_empty() =>
                        {
                            if published_at.elapsed() < NO_CHECKS_GRACE {
                                ctx.wakers
                                    .wait(key.installation_id, repo, CHECKS_POLL)
                                    .await;
                                continue;
                            }
                            Event::ChecksPassed
                        }
                        (CheckState::Passed, _) => Event::ChecksPassed,
                        (CheckState::Failed, names) => Event::ChecksFailed(names),
                        (CheckState::Pending, _) => {
                            ctx.wakers
                                .wait(key.installation_id, repo, CHECKS_POLL)
                                .await;
                            continue;
                        }
                    }
                }
            }
            Phase::Merging { head } => {
                if pr.hold_reason().is_some() {
                    Event::NeedsApproval
                } else {
                    match gh.merge(repo, number, head, &snap.merge_method).await? {
                        MergeOutcome::Merged { sha } => return Ok(Outcome::Merged(sha)),
                        MergeOutcome::Rejected(r) => Event::MergeRejected(r),
                    }
                }
            }
        };

        job = match job.step(event) {
            Transition::Continue(j) => j,
            Transition::Merged => unreachable!("merges return above"),
            Transition::Ejected(r) => return Ok(Outcome::Ejected(r)),
            Transition::Held(r) => return Ok(Outcome::Held(r)),
        };
    }
}

/// Rebases a PR onto the trunk and publishes the result to its branch: `Event::Rebased` with
/// the new head, or why it could not.
#[async_trait]
trait Rebase: Send + Sync {
    async fn rebase(&self, key: &LaneKey, snap: &RepoSnapshot, pr: &PullRequest) -> Result<Event>;
}

/// A plain `git rebase` here when the PR is already on the trunk, else a sandboxed job, whose
/// result is checked, then published through the Git Data API.
struct JobRebase<'a> {
    ctx: &'a Ctx,
    gh: &'a Installation,
}

#[async_trait]
impl Rebase for JobRebase<'_> {
    async fn rebase(&self, key: &LaneKey, snap: &RepoSnapshot, pr: &PullRequest) -> Result<Event> {
        rebase_and_publish(self.ctx, self.gh, key, snap, pr).await
    }
}

async fn rebase_and_publish(
    ctx: &Ctx,
    gh: &Installation,
    key: &LaneKey,
    snap: &RepoSnapshot,
    pr: &PullRequest,
) -> Result<Event> {
    let repo = &key.repo;
    let git = Git;
    let cache = cache_path(&ctx.config.data_dir, &repo.owner, &repo.name);
    let clone_url = format!("https://github.com/{repo}.git");
    git.ensure_cache(&cache, &clone_url).await?;

    let pr_ref = format!("refs/mergepatrol/pr/{}", pr.number);
    let trunk_ref = format!("refs/remotes/origin/{}", key.trunk);
    git.fetch(
        &cache,
        &gh.token().await?,
        &[
            &format!("+refs/heads/{}:{trunk_ref}", key.trunk),
            &format!("+refs/heads/{}:{pr_ref}", pr.head_ref_name),
        ],
    )
    .await?;
    let head = git.rev_parse(&cache, &pr_ref).await?;
    let trunk = git.rev_parse(&cache, &trunk_ref).await?;
    if git.is_ancestor(&cache, &trunk, &head).await? {
        return Ok(Event::Rebased { head, base: trunk });
    }

    let job_id = format!("{}-{}-{}-{}", repo.owner, repo.name, pr.number, &head[..8]);
    let out = ctx.config.data_dir.join("jobs").join(&job_id);
    let _ = tokio::fs::remove_dir_all(&out).await;
    let spec = JobSpec {
        repo: repo.to_string(),
        clone_url,
        token: gh.read_token(repo).await?,
        head_ref: pr.head_ref_name.clone(),
        head_sha: head.clone(),
        trunk: key.trunk.clone(),
        trunk_sha: trunk.clone(),
        pr_title: pr.title.clone(),
        pr_body: pr.body.clone(),
        resolver: ResolverSpec {
            model: ctx.config.model.clone(),
            effort: ctx.config.effort.clone(),
            timeout_secs: RESOLVER_TIMEOUT.as_secs(),
            max_turns: RESOLVER_MAX_TURNS,
            anthropic_api_key: ctx
                .vault
                .get(Scope::Installation(key.installation_id), ANTHROPIC_API_KEY)
                .await?,
        },
    };
    record(ctx, key, pr.number, "job", &job_id).await?;
    let result = match ctx.executor.run(&job_id, &spec, &out).await {
        Ok(r) => r,
        Err(e) => return Ok(Event::RebaseFailed(format!("the rebase job failed: {e:#}"))),
    };
    for e in &result.events {
        record(ctx, key, pr.number, &e.kind, &e.message).await?;
    }
    if let JobOutcome::Failed { reason } = result.outcome {
        return Ok(Event::RebaseFailed(reason));
    }

    let local_head = match check_job_output(&git, &cache, &out, pr.number, &trunk, &head).await {
        Ok(h) => h,
        Err(e) => return Ok(Event::RebaseFailed(format!("{e:#}"))),
    };
    let published = publish(
        gh,
        &git,
        &cache,
        Publish {
            repo: &key.repo,
            repo_id: &snap.id,
            branch: &pr.head_ref_name,
            expected_head: &head,
            base: &trunk,
            local_head: &local_head,
        },
    )
    .await;
    match published {
        Ok(sha) => Ok(Event::Rebased {
            head: sha,
            base: trunk,
        }),
        // `updateRefs` refuses when the branch moved: the author pushed while we rebased.
        Err(_) if gh.branch_sha(&key.repo, &pr.head_ref_name).await? != head => {
            Ok(Event::HeadMoved)
        }
        Err(e) => Ok(Event::RebaseFailed(format!("publishing failed: {e:#}"))),
    }
}

/// The job is untrusted, so its output is checked here, not believed. A rebase only replays
/// the PR's changes, and a conflict can only be in a file the PR changed: the result may touch
/// no file outside the PR's own.
async fn check_job_output(
    git: &Git,
    cache: &Path,
    out: &Path,
    number: PrNumber,
    trunk: &str,
    head: &str,
) -> Result<String> {
    let bundle = out.join(BUNDLE_FILE);
    let size = tokio::fs::metadata(&bundle)
        .await
        .context("the job wrote no bundle")?
        .len();
    if size > MAX_BUNDLE_BYTES {
        bail!("the job's result is {size} bytes, over the {MAX_BUNDLE_BYTES} limit");
    }
    let local_head = git
        .import_bundle(cache, &bundle, &format!("refs/mergepatrol/result/{number}"))
        .await?;
    let _ = tokio::fs::remove_file(&bundle).await;
    if !git.is_ancestor(cache, trunk, &local_head).await? {
        bail!("the job's result does not sit on the base branch");
    }
    let pr_files: BTreeSet<String> = git
        .changed_files(cache, &git.merge_base(cache, trunk, head).await?, head)
        .await?
        .into_iter()
        .collect();
    let outside: Vec<String> = git
        .changed_files(cache, trunk, &local_head)
        .await?
        .into_iter()
        .filter(|f| !pr_files.contains(f))
        .collect();
    if !outside.is_empty() {
        bail!(
            "the rebase changed files outside the PR: {}",
            outside.join(", ")
        );
    }
    Ok(local_head)
}

#[cfg(test)]
mod loop_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    fn commit(dir: &Path, files: &[(&str, &str)]) -> String {
        for (f, c) in files {
            std::fs::write(dir.join(f), c).unwrap();
            git(dir, &["add", f]);
        }
        git(dir, &["commit", "-q", "-m", "c"]);
        git(dir, &["rev-parse", "HEAD"])
    }

    /// A job result that changes `files` on top of the trunk, checked against a PR that changed `a.txt`.
    async fn check(files: &[(&str, &str)]) -> Result<String> {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir(&work).unwrap();
        git(&work, &["init", "-q", "-b", "main"]);
        let base = commit(&work, &[("a.txt", "1\n"), ("b.txt", "1\n")]);
        let head = commit(&work, &[("a.txt", "pr\n")]);
        git(&work, &["checkout", "-q", "--detach", &base]);
        let trunk = commit(&work, &[("c.txt", "trunk\n")]);
        let result = commit(&work, files);

        let cache = tmp.path().join("cache");
        std::fs::create_dir(&cache).unwrap();
        git(&cache, &["init", "-q", "--bare"]);
        git(
            &cache,
            &[
                "fetch",
                "-q",
                work.to_str().unwrap(),
                &format!("{head}:refs/pr"),
                &format!("{trunk}:refs/trunk"),
            ],
        );

        let out = tmp.path().join("out");
        std::fs::create_dir(&out).unwrap();
        Git.create_bundle(&work, &out.join(BUNDLE_FILE), &result, &trunk)
            .await
            .unwrap();
        check_job_output(&Git, &cache, &out, 1, &trunk, &head).await
    }

    #[tokio::test(start_paused = true)]
    async fn a_webhook_cuts_the_wait_short_even_when_it_came_first() {
        use mergepatrol_web::Wake;
        let wakers = Wakers::default();
        let repo = RepoName::parse("o/r").unwrap();
        let hour = Duration::from_secs(3600);

        wakers.wake(1, repo.clone());
        let start = tokio::time::Instant::now();
        wakers.wait(1, &repo, hour).await;
        assert_eq!(start.elapsed(), Duration::ZERO);

        // Another repo's webhook does not count.
        wakers.wake(1, RepoName::parse("o/other").unwrap());
        wakers.wait(1, &repo, hour).await;
        assert_eq!(start.elapsed(), hour);
    }

    #[tokio::test]
    async fn job_output_inside_the_pr_files_passes() {
        check(&[("a.txt", "pr, resolved\n")]).await.unwrap();
    }

    #[tokio::test]
    async fn job_output_outside_the_pr_files_is_rejected() {
        let err = check(&[("a.txt", "pr\n"), ("b.txt", "sneaky\n")])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("outside the PR: b.txt"), "{err}");
    }
}
