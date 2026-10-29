//! Drives one repo: polls GitHub, keeps the lane in sync, and runs the PR at the head of the lane
//! through rebase, checks, fixes for failed checks, and merge.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use std::path::Path;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use futures_util::future::try_join_all;
use mergepatrol_analytics::Analytics;
use mergepatrol_core::{
    Candidate, CheckFailure, EjectReason, Event, Job, Lane, LaneChange, Phase, PrNumber,
    REPO_CONFIG_PATH, RepoName, RepoSettings, Transition,
};
use mergepatrol_executor::Executor;
use mergepatrol_executor::protocol::{BUNDLE_FILE, JobOutcome, JobSpec, ResolverSpec, Task};
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
/// With no required checks, how long after a publish its checks may first count as green: a
/// slow CI may not have created its checks yet, while a quick app's are already green.
const NO_CHECKS_GRACE: Duration = Duration::from_secs(120);
/// How soon a repo turned on in the dashboard starts its queue.
const SUPERVISE_EVERY: Duration = Duration::from_secs(10);
/// How often to look for the config file in every repo of every installation. A push webhook
/// that adds or removes it is quicker.
const DISCOVER_EVERY: Duration = Duration::from_secs(5 * 60);
const RESOLVER_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const RESOLVER_MAX_TURNS: u32 = 50;
const MAX_BUNDLE_BYTES: u64 = 200 * 1024 * 1024;
const MERGE_RETRIES: u32 = 15;
const MERGE_RETRY_AFTER: Duration = Duration::from_secs(1);
/// A fix may change any file but these: CI's own config, and the queue's.
const FIX_PROTECTED: [&str; 2] = [".github/", ".mergepatrol/"];

pub struct Ctx {
    pub config: Config,
    /// The hosted GitHub App. `None` self-hosted: the App is in the vault.
    pub hosted_app: Option<AppCredentials>,
    pub vault: Arc<Vault>,
    pub store: Arc<dyn Store>,
    pub executor: Arc<dyn Executor>,
    pub jobs: Semaphore,
    pub wakers: Arc<Wakers>,
    pub analytics: Analytics,
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

/// Runs a queue for each repo that is on, and stops the queues of repos that are turned off.
/// Waits until the GitHub App exists.
pub async fn supervise(ctx: Arc<Ctx>) {
    let mut running = Running::new();
    loop {
        if let Err(e) = reconcile(&ctx, &mut running).await {
            tracing::error!(error = %format!("{e:#}"), "could not update the running queues");
        }
        tokio::time::sleep(SUPERVISE_EVERY).await;
    }
}

/// Keeps track of which repos have the config file on their default branch, so that a repo
/// queues once it has one.
pub async fn discover(ctx: Arc<Ctx>) {
    loop {
        if let Err(e) = discover_once(&ctx).await {
            tracing::error!(error = %format!("{e:#}"), "could not look for config files");
        }
        tokio::time::sleep(DISCOVER_EVERY).await;
    }
}

async fn discover_once(ctx: &Ctx) -> Result<()> {
    let Some(creds) = app_credentials(ctx).await? else {
        return Ok(());
    };
    let app = App::from_credentials(&creds)?;
    for inst in app.installations().await? {
        let gh = app.installation(inst.id);
        // One installation failing (suspended, say) does not stop the others.
        let found = async {
            let repos: Vec<RepoName> = gh
                .repositories()
                .await?
                .into_iter()
                .map(|r| r.repo)
                .collect();
            let has = gh.have_file(&repos, REPO_CONFIG_PATH).await?;
            anyhow::Ok(
                repos
                    .into_iter()
                    .zip(has)
                    .filter_map(|(r, has)| has.then_some(r))
                    .collect::<Vec<_>>(),
            )
        };
        match found.await {
            Ok(with_config) => ctx.store.replace_has_config(inst.id, &with_config).await?,
            Err(e) => tracing::warn!(
                installation = inst.id,
                account = %inst.account,
                error = %format!("{e:#}"),
                "could not look for config files"
            ),
        }
    }
    Ok(())
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
        .repos()
        .await?
        .into_iter()
        .filter(|s| s.on())
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
    let sandbox = JobSandbox { ctx: &ctx, gh: &gh };
    loop {
        match tick(&ctx, &gh, &sandbox, &repo).await {
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

async fn tick(ctx: &Ctx, gh: &dyn GitHub, sandbox: &dyn Sandbox, repo: &RepoName) -> Result<Ran> {
    let installation = gh.installation_id();
    let file = gh.config_file(repo).await?;
    let settings = match RepoSettings::from_file(file.as_deref(), &ctx.config.repo_defaults) {
        Ok(s) => s,
        Err(e) => {
            // Paused: the settings it would run with are a guess.
            tracing::warn!(installation, %repo, error = %e, "the config file does not parse");
            ctx.store
                .set_config_error(installation, repo, Some(&e))
                .await?;
            return Ok(Ran::Idle);
        }
    };
    ctx.store.set_config_error(installation, repo, None).await?;
    let labels = settings.labels();
    let snap = gh.snapshot(repo, &labels).await?;
    // So people find the labels in GitHub's picker, not only in the docs.
    for spec in labels.specs() {
        if snap.missing_labels.iter().any(|m| m == spec.name) {
            gh.create_label(repo, &spec).await?;
        }
    }
    let key = LaneKey {
        installation_id: installation,
        repo: repo.clone(),
        trunk: snap.default_branch.clone(),
    };
    let mut lane = ctx.store.load_lane(&key).await?;

    let mut candidates = Vec::new();
    for pr in &snap.labeled {
        if pr.is_cross_repository {
            eject(
                ctx,
                &settings,
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
            summary: pr.summary(),
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
        next = lane.next(settings.batch_size);
    }
    let Some(&number) = next.first() else {
        return Ok(Ran::Idle);
    };
    let required = gh.required_checks(repo, &key.trunk).await?;

    // GitHub merges a PR behind the trunk, which a batch's PRs are, unless the trunk is strict.
    if next.len() > 1 && !required.strict {
        let prs = batchable(gh, repo, &snap, &required.names, &next).await?;
        if let Some(batch) = build_batch(ctx, gh, &key, &snap, &mut lane, &prs).await? {
            run_batch(ctx, &settings, gh, &key, &snap, &mut lane, batch).await?;
            return Ok(Ran::Job);
        }
    }

    lane.activate(number);
    ctx.store.save_lane(&key, &lane).await?;

    let _slot = ctx.jobs.acquire().await?;
    let outcome = match run_job(ctx, &settings, gh, sandbox, &key, &snap, number).await {
        Ok(o) => o,
        Err(e) => Outcome::Ejected(EjectReason::RebaseFailed(format!("{e:#}"))),
    };
    finish(ctx, &settings, gh, &key, &mut lane, number, outcome).await?;
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
    settings: &RepoSettings,
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
            let labels = settings.labels();
            let comment = format!("Merged as {sha}.");
            tokio::try_join!(
                gh.sticky_comment(&key.repo, number, &comment),
                gh.remove_label(&key.repo, number, &labels.trigger),
                gh.remove_label(&key.repo, number, &labels.priority),
            )?;
        }
        Outcome::Ejected(reason) => {
            lane.eject(number);
            eject(
                ctx,
                settings,
                gh,
                key,
                number,
                &format!("Ejected: {reason}."),
            )
            .await?;
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
#[allow(clippy::too_many_arguments)]
async fn run_batch(
    ctx: &Ctx,
    settings: &RepoSettings,
    gh: &dyn GitHub,
    key: &LaneKey,
    snap: &RepoSnapshot,
    lane: &mut Lane,
    batch: Batch,
) -> Result<()> {
    let repo = &key.repo;
    let members: Vec<PrNumber> = batch.steps.iter().map(|(pr, _)| pr.number).collect();
    for n in &members {
        lane.activate(*n);
    }
    ctx.store.save_lane(key, lane).await?;

    let result = test_and_merge(ctx, settings, gh, key, snap, lane, &batch, &members).await;
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
    settings: &RepoSettings,
    gh: &dyn GitHub,
    key: &LaneKey,
    snap: &RepoSnapshot,
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
    lane.batch_pr = Some(draft);
    ctx.store.save_lane(key, lane).await?;
    try_join_all(members.iter().map(|n| async move {
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
        .await
    }))
    .await?;

    let head = &batch.steps.last().expect("a batch has PRs").1.sha;
    let opened = Instant::now();
    let tested = loop {
        let now = gh.snapshot(repo, &settings.labels()).await?;
        if let Some(why) = stale(&now, snap, batch) {
            break Tested::Stale(why);
        }
        match checks_verdict(gh, key, head, opened).await? {
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
            try_join_all(members.iter().map(|n| async {
                record(ctx, key, *n, "batch-failed", &why).await?;
                gh.sticky_comment(
                    repo,
                    *n,
                    &format!("The batch with {} failed: {why}. Mergepatrol now tests these PRs in smaller batches, to find the one that broke it.", refs(*n)),
                )
                .await
            }))
            .await?;
            Ok(())
        }
        Tested::Passed => merge_batch(ctx, settings, gh, key, snap, lane, batch).await,
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
    settings: &RepoSettings,
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
        if !pr.has_label(&settings.label)
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
            return finish(ctx, settings, gh, key, lane, n, Outcome::Held(reason)).await;
        }
        match merge_retrying(gh, repo, n, &tested.head_ref_oid, &snap.merge_method).await? {
            MergeOutcome::Merged { sha } => {
                let tree = gh.commit_tree(repo, &sha).await?;
                finish(
                    ctx,
                    settings,
                    gh,
                    key,
                    lane,
                    n,
                    Outcome::Merged(sha.clone()),
                )
                .await?;
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
                return finish(ctx, settings, gh, key, lane, n, Outcome::Ejected(reason)).await;
            }
        }
    }
    Ok(())
}

/// The verdict on `head`'s checks, against the trunk's required checks as they are now, not as
/// the job began: a ruleset may change while it waits. With none required, every check counts,
/// and a pass only after `NO_CHECKS_GRACE` from `published`.
async fn checks_verdict(
    gh: &dyn GitHub,
    key: &LaneKey,
    head: &str,
    published: Instant,
) -> Result<(CheckState, Vec<String>)> {
    let required = gh.required_checks(&key.repo, &key.trunk).await?.names;
    let verdict = gh.checks(&key.repo, head).await?.verdict(&required);
    Ok(match verdict {
        (CheckState::Passed, _) if required.is_empty() && published.elapsed() < NO_CHECKS_GRACE => {
            (CheckState::Pending, vec![])
        }
        v => v,
    })
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

/// A queue event: shown on the dashboard, logged, and counted for its workspace.
async fn record(
    ctx: &Ctx,
    key: &LaneKey,
    number: PrNumber,
    kind: &str,
    message: &str,
) -> Result<()> {
    tracing::info!(repo = %key.repo, pr = number, kind, detail = message, "queue");
    ctx.analytics.queue_event(
        key.installation_id,
        &key.repo.to_string(),
        &key.trunk,
        number,
        kind,
    );
    ctx.store.record(key, number, kind, message).await
}

async fn eject(
    ctx: &Ctx,
    settings: &RepoSettings,
    gh: &dyn GitHub,
    key: &LaneKey,
    number: PrNumber,
    why: &str,
) -> Result<()> {
    let labels = settings.labels();
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

#[allow(clippy::too_many_arguments)]
async fn run_job(
    ctx: &Ctx,
    settings: &RepoSettings,
    gh: &dyn GitHub,
    sandbox: &dyn Sandbox,
    key: &LaneKey,
    snap: &RepoSnapshot,
    number: PrNumber,
) -> Result<Outcome> {
    let repo = &key.repo;
    let mut job = Job::start(number, settings.fix_ci.allowed());
    let mut published_at = Instant::now();

    loop {
        let pr = gh.pull_request(repo, number).await?;
        if !pr.has_label(&settings.label) {
            return Ok(Outcome::Cancelled);
        }

        let event = match &job.phase {
            Phase::Rebasing => {
                record(ctx, key, number, "rebasing", &format!("onto {}", key.trunk)).await?;
                gh.sticky_comment(repo, number, &format!("Rebasing onto `{}`.", key.trunk))
                    .await?;
                let event = sandbox.rebase(key, snap, &pr, settings).await?;
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
                    match checks_verdict(gh, key, head, published_at).await? {
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
            Phase::Fixing { head, base, failed } => {
                let attempt = format!("attempt {} of {}", job.fixes, job.fix_attempts);
                record(
                    ctx,
                    key,
                    number,
                    "fixing",
                    &format!("{}, {attempt}", failed.join(", ")),
                )
                .await?;
                gh.sticky_comment(
                    repo,
                    number,
                    &format!(
                        "Required checks failed: {}. Fixing them, {attempt}.",
                        failed.join(", ")
                    ),
                )
                .await?;
                let failures = gh.check_failures(repo, head, failed).await?;
                let event = sandbox
                    .fix(key, snap, &pr, settings, head, base, &failures)
                    .await?;
                published_at = Instant::now();
                if let Event::Fixed { head } = &event {
                    record(ctx, key, number, "published", head).await?;
                    gh.sticky_comment(
                        repo,
                        number,
                        &format!("Pushed a fix as {head}. Waiting for checks."),
                    )
                    .await?;
                }
                event
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

/// The jobs that change a PR's branch, and publish the result to it.
#[async_trait]
trait Sandbox: Send + Sync {
    /// Rebases the PR onto the trunk: `Event::Rebased` with the new head, or why it could not.
    async fn rebase(
        &self,
        key: &LaneKey,
        snap: &RepoSnapshot,
        pr: &PullRequest,
        settings: &RepoSettings,
    ) -> Result<Event>;

    /// Commits a fix for `failures` on top of `head`, which sits on `base`: `Event::Fixed` with
    /// the new head, or why it could not.
    #[allow(clippy::too_many_arguments)]
    async fn fix(
        &self,
        key: &LaneKey,
        snap: &RepoSnapshot,
        pr: &PullRequest,
        settings: &RepoSettings,
        head: &str,
        base: &str,
        failures: &[CheckFailure],
    ) -> Result<Event>;
}

/// A sandboxed job, whose result is checked, then published through the Git Data API. A rebase
/// of a PR already on the trunk needs no job.
struct JobSandbox<'a> {
    ctx: &'a Ctx,
    gh: &'a Installation,
}

#[async_trait]
impl Sandbox for JobSandbox<'_> {
    async fn rebase(
        &self,
        key: &LaneKey,
        snap: &RepoSnapshot,
        pr: &PullRequest,
        settings: &RepoSettings,
    ) -> Result<Event> {
        rebase_and_publish(self.ctx, self.gh, key, snap, pr, settings).await
    }

    async fn fix(
        &self,
        key: &LaneKey,
        snap: &RepoSnapshot,
        pr: &PullRequest,
        settings: &RepoSettings,
        head: &str,
        base: &str,
        failures: &[CheckFailure],
    ) -> Result<Event> {
        fix_and_publish(
            self.ctx, self.gh, key, snap, pr, settings, head, base, failures,
        )
        .await
    }
}

/// The repo's object cache, with the trunk and the PR branch fetched: their heads.
async fn fetch_pr(
    ctx: &Ctx,
    gh: &Installation,
    git: &Git,
    key: &LaneKey,
    pr: &PullRequest,
) -> Result<(std::path::PathBuf, String, String)> {
    let repo = &key.repo;
    let cache = cache_path(&ctx.config.data_dir, &repo.owner, &repo.name);
    git.ensure_cache(&cache, &clone_url(repo)).await?;
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
    Ok((cache, head, trunk))
}

fn clone_url(repo: &RepoName) -> String {
    format!("https://github.com/{repo}.git")
}

/// The spec of a job on the PR at `head`, which sits on, or goes onto, `trunk`.
#[allow(clippy::too_many_arguments)]
async fn job_spec(
    ctx: &Ctx,
    gh: &Installation,
    key: &LaneKey,
    pr: &PullRequest,
    settings: &RepoSettings,
    head: &str,
    trunk: &str,
    task: Task,
) -> Result<JobSpec> {
    Ok(JobSpec {
        repo: key.repo.to_string(),
        clone_url: clone_url(&key.repo),
        token: gh.read_token(&key.repo).await?,
        head_ref: pr.head_ref_name.clone(),
        head_sha: head.to_owned(),
        trunk: key.trunk.clone(),
        trunk_sha: trunk.to_owned(),
        pr_title: pr.title.clone(),
        pr_body: pr.body.clone(),
        verify: settings.verify.clone(),
        resolver: ResolverSpec {
            model: settings.resolver.model.clone(),
            effort: settings.resolver.effort.clone(),
            timeout_secs: RESOLVER_TIMEOUT.as_secs(),
            max_turns: RESOLVER_MAX_TURNS,
            anthropic_api_key: ctx
                .vault
                .get(Scope::Installation(key.installation_id), ANTHROPIC_API_KEY)
                .await?,
        },
        task,
    })
}

/// Runs a job in the sandbox, with its output in `out`. `Err` says why it failed.
async fn run_sandboxed(
    ctx: &Ctx,
    key: &LaneKey,
    number: PrNumber,
    job_id: &str,
    spec: &JobSpec,
    out: &Path,
) -> Result<()> {
    let _ = tokio::fs::remove_dir_all(out).await;
    record(ctx, key, number, "job", job_id).await?;
    let result = ctx
        .executor
        .run(job_id, spec, out)
        .await
        .context("the job failed")?;
    for e in &result.events {
        record(ctx, key, number, &e.kind, &e.message).await?;
    }
    if let JobOutcome::Failed { reason } = result.outcome {
        bail!("{reason}");
    }
    Ok(())
}

/// Publishes the commits `base..local_head` to the PR branch, if it is still at `head`.
#[allow(clippy::too_many_arguments)]
async fn publish_to_pr(
    gh: &Installation,
    git: &Git,
    cache: &Path,
    key: &LaneKey,
    snap: &RepoSnapshot,
    pr: &PullRequest,
    head: &str,
    base: &str,
    local_head: &str,
) -> Result<std::result::Result<String, Event>> {
    let published = publish(
        gh,
        git,
        cache,
        Publish {
            repo: &key.repo,
            repo_id: &snap.id,
            branch: &pr.head_ref_name,
            expected_head: head,
            base,
            local_head,
        },
    )
    .await;
    match published {
        Ok(sha) => Ok(Ok(sha)),
        // `updateRefs` refuses when the branch moved: the author pushed while the job ran.
        Err(_) if gh.branch_sha(&key.repo, &pr.head_ref_name).await? != head => {
            Ok(Err(Event::HeadMoved))
        }
        Err(e) => bail!("publishing failed: {e:#}"),
    }
}

#[allow(clippy::too_many_arguments)]
async fn fix_and_publish(
    ctx: &Ctx,
    gh: &Installation,
    key: &LaneKey,
    snap: &RepoSnapshot,
    pr: &PullRequest,
    settings: &RepoSettings,
    head: &str,
    base: &str,
    failures: &[CheckFailure],
) -> Result<Event> {
    let repo = &key.repo;
    let git = Git;
    let (cache, fetched, _) = fetch_pr(ctx, gh, &git, key, pr).await?;
    if fetched != head {
        return Ok(Event::HeadMoved);
    }
    let job_id = format!(
        "{}-{}-{}-fix-{}",
        repo.owner,
        repo.name,
        pr.number,
        &head[..8]
    );
    let out = ctx.config.data_dir.join("jobs").join(&job_id);
    let task = Task::FixChecks {
        failures: failures.to_vec(),
    };
    let spec = job_spec(ctx, gh, key, pr, settings, head, base, task).await?;
    let fixed = async {
        run_sandboxed(ctx, key, pr.number, &job_id, &spec, &out).await?;
        let local_head = check_fix_output(&git, &cache, &out, pr.number, head).await?;
        publish_to_pr(gh, &git, &cache, key, snap, pr, head, head, &local_head).await
    };
    Ok(match fixed.await {
        Ok(Ok(sha)) => Event::Fixed { head: sha },
        Ok(Err(event)) => event,
        Err(e) => Event::FixFailed(format!("{e:#}")),
    })
}

async fn rebase_and_publish(
    ctx: &Ctx,
    gh: &Installation,
    key: &LaneKey,
    snap: &RepoSnapshot,
    pr: &PullRequest,
    settings: &RepoSettings,
) -> Result<Event> {
    let repo = &key.repo;
    let git = Git;
    let (cache, head, trunk) = fetch_pr(ctx, gh, &git, key, pr).await?;
    if git.is_ancestor(&cache, &trunk, &head).await? {
        return Ok(Event::Rebased { head, base: trunk });
    }

    let job_id = format!("{}-{}-{}-{}", repo.owner, repo.name, pr.number, &head[..8]);
    let out = ctx.config.data_dir.join("jobs").join(&job_id);
    let spec = job_spec(ctx, gh, key, pr, settings, &head, &trunk, Task::Rebase).await?;
    let rebased = async {
        run_sandboxed(ctx, key, pr.number, &job_id, &spec, &out).await?;
        let local_head = check_job_output(&git, &cache, &out, pr.number, &trunk, &head).await?;
        publish_to_pr(gh, &git, &cache, key, snap, pr, &head, &trunk, &local_head).await
    };
    Ok(match rebased.await {
        Ok(Ok(sha)) => Event::Rebased {
            head: sha,
            base: trunk,
        },
        Ok(Err(event)) => event,
        Err(e) => Event::RebaseFailed(format!("{e:#}")),
    })
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
    let local_head = import_job_output(git, cache, out, number).await?;
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

/// A fix may change any file but CI's and the queue's config, and only on top of the PR head.
async fn check_fix_output(
    git: &Git,
    cache: &Path,
    out: &Path,
    number: PrNumber,
    head: &str,
) -> Result<String> {
    let local_head = import_job_output(git, cache, out, number).await?;
    if !git.is_ancestor(cache, head, &local_head).await? || local_head == head {
        bail!("the fix is not a commit on top of the PR");
    }
    let protected: Vec<String> = git
        .changed_files(cache, head, &local_head)
        .await?
        .into_iter()
        .filter(|f| FIX_PROTECTED.iter().any(|p| f.starts_with(p)))
        .collect();
    if !protected.is_empty() {
        bail!("the fix changed files it may not: {}", protected.join(", "));
    }
    Ok(local_head)
}

/// Imports the job's bundle, once its size is checked: the commit it holds.
async fn import_job_output(
    git: &Git,
    cache: &Path,
    out: &Path,
    number: PrNumber,
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

    /// A fix job result that changes `files` on top of the PR head.
    async fn check_fix(files: &[(&str, &str)]) -> Result<String> {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir_all(work.join(".github")).unwrap();
        git(&work, &["init", "-q", "-b", "main"]);
        commit(&work, &[("a.txt", "1\n")]);
        let head = commit(&work, &[("a.txt", "pr\n")]);
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
            ],
        );
        let out = tmp.path().join("out");
        std::fs::create_dir(&out).unwrap();
        Git.create_bundle(&work, &out.join(BUNDLE_FILE), &result, &head)
            .await
            .unwrap();
        check_fix_output(&Git, &cache, &out, 1, &head).await
    }

    #[tokio::test]
    async fn a_fix_may_change_files_outside_the_pr() {
        check_fix(&[("b.txt", "a test it needed\n")]).await.unwrap();
    }

    #[tokio::test]
    async fn a_fix_may_not_change_ci_config() {
        let err = check_fix(&[("b.txt", "ok\n"), (".github/ci.yml", "skip: true\n")])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("may not: .github/ci.yml"), "{err}");
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
