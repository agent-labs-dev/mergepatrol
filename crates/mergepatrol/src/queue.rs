//! Drives one repo: polls GitHub, keeps the lane in sync, and runs the PR at the head of the lane
//! through rebase, checks, fixes for failed checks, and merge.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use std::path::Path;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use futures_util::future::try_join_all;
use mergepatrol_analytics::{Analytics, Exit};
use mergepatrol_billing::{Billing, Hold};
use mergepatrol_core::{
    BatchStep, BuiltBatch, CHECKS_TIMEOUT, Candidate, CheckFailure, EjectReason, Event, Job, Lane,
    LaneChange, LayerHead, PendingMerge, Phase, PrNumber, PrSummary, REPO_CONFIG_PATH, RepoName,
    RepoSettings, Transition,
};
use mergepatrol_executor::Executor;
use mergepatrol_executor::protocol::{
    ApiAccess, BUNDLE_FILE, JobOutcome, JobSpec, MAX_BUNDLE_BYTES, ResolverSpec, Task,
};
use mergepatrol_executor::proxy::{Api, ApiProxy, Granted, REQUESTS_PER_JOB};
use mergepatrol_git::{Git, cache_path};
use mergepatrol_github::{
    App, AppCredentials, BranchMerge, CheckState, Installation, MergeOutcome, PullRequest,
    RateLimited, RepoSnapshot,
};
use mergepatrol_store::{Change, GITHUB_APP, LaneKey, PgStore, Scope, Store, Vault};
use secrecy::ExposeSecret;
use tokio::sync::{Notify, Semaphore, mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::cached::{Cached, Reader};
use crate::config::Config;
use crate::credentials::ChatGpt;
use crate::github::GitHub;
use crate::publish::{Publish, create_commits, publish};

const CHECKS_POLL: Duration = Duration::from_secs(15);
/// With no required checks, how long after a publish its checks may first count as green: a
/// slow CI may not have created its checks yet, while a quick app's are already green.
const NO_CHECKS_GRACE: Duration = Duration::from_secs(120);
/// How often the instance running the queues checks that it still holds the queue lock, and
/// how often the others try to take it.
const LOCK_EVERY: Duration = Duration::from_secs(5);
/// How often to delete what nothing reads anymore.
const PRUNE_EVERY: Duration = Duration::from_secs(60 * 60);
/// How long a job's output, with its agent's transcript, stays on disk: to look into a job.
const JOB_OUTPUTS_FOR: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// How soon a repo turned on in the dashboard starts its queue.
const SUPERVISE_EVERY: Duration = Duration::from_secs(10);
/// How often to look for the config file in every repo of every installation. A push webhook
/// that adds or removes it is quicker.
const DISCOVER_EVERY: Duration = Duration::from_secs(5 * 60);
const RESOLVER_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const RESOLVER_MAX_TURNS: u32 = 50;
const MERGE_RETRIES: u32 = 15;
const MERGE_RETRY_AFTER: Duration = Duration::from_secs(1);
/// How long a PR's job may keep failing on errors outside it, such as a GitHub outage, before
/// the PR is ejected. Shorter failures leave it at the head of the queue, to run again.
const JOB_ERRORS_FOR: Duration = Duration::from_secs(30 * 60);
/// A fix may change any file but these: CI's own config, and the queue's.
const FIX_PROTECTED: [&str; 2] = [".github/", ".mergepatrol/"];
const AWAITING_APPROVAL: &str = "waiting for a maintainer to approve its workflow runs";
/// How long to wait for a fork PR's pull ref to follow a push to the fork.
const PULL_REF_WAIT: Duration = Duration::from_secs(30);
/// How long a fork PR may wait for its pin, the head its label went on at, before it leaves.
const PIN_WAIT: Duration = Duration::from_secs(5 * 60);
/// How long the reads of GitHub that the queue no longer makes stay: a repo turned off, a commit
/// long merged.
const GITHUB_READS_FOR: Duration = Duration::from_secs(24 * 60 * 60);
/// How often each subscription's seats are brought in line with Stripe.
const SYNC_SEATS_EVERY: Duration = Duration::from_secs(60);
/// How long the inbox keeps a PR once it merged or closed. The You tab lists a week of them.
const INBOX_CLOSED_FOR: Duration = Duration::from_secs(30 * 24 * 60 * 60);

pub struct Ctx {
    pub config: Config,
    /// The hosted GitHub App. `None` self-hosted: the App is in the vault.
    pub hosted_app: Option<AppCredentials>,
    pub vault: Arc<Vault>,
    pub store: Arc<dyn Store>,
    pub executor: Arc<dyn Executor>,
    /// Holds the workspaces' model credentials for the jobs, which get tokens.
    pub proxy: Arc<ApiProxy>,
    pub chatgpt: ChatGpt,
    /// Sandboxed jobs running at once, across repos. A PR waiting for its checks holds none.
    pub jobs: Semaphore,
    pub wakers: Arc<Wakers>,
    pub analytics: Analytics,
    pub prs: Prs,
    pub job_errors: JobErrors,
    /// When the queue first saw each fork PR it has no pin for yet.
    pub unpinned: Unpinned,
    /// Hosted: holds PRs over the plan's limits, and bills private repos. `None` self-hosted.
    pub billing: Option<Arc<Billing>>,
}

/// Fork PRs waiting for their pin: the `labeled` webhook, or for their pull ref to show it.
#[derive(Default)]
pub struct Unpinned(std::sync::Mutex<HashMap<(u64, RepoName, PrNumber), Instant>>);

impl Unpinned {
    /// How long ago the queue first saw the PR without its pin.
    fn waited(&self, key: &LaneKey, number: PrNumber) -> Duration {
        let mut map = self.0.lock().unwrap();
        let k = (key.installation_id, key.repo.clone(), number);
        map.entry(k).or_insert_with(Instant::now).elapsed()
    }

    fn forget(&self, key: &LaneKey, number: PrNumber) {
        let k = (key.installation_id, key.repo.clone(), number);
        self.0.lock().unwrap().remove(&k);
    }

    /// Forgets the repo's PRs that lost the label: a new labeling waits afresh.
    fn keep_labeled(&self, installation: u64, repo: &RepoName, labeled: &[PullRequest]) {
        let mut map = self.0.lock().unwrap();
        map.retain(|(i, r, number), _| {
            *i != installation || r != repo || labeled.iter().any(|pr| pr.number == *number)
        });
    }
}

/// Since when each PR's job has failed on every try, on errors outside the job.
#[derive(Default)]
pub struct JobErrors(std::sync::Mutex<HashMap<(u64, RepoName, PrNumber), Instant>>);

impl JobErrors {
    /// How long the PR's job has failed, this time included.
    fn failed(&self, key: &LaneKey, number: PrNumber) -> Duration {
        let mut map = self.0.lock().unwrap();
        let key = (key.installation_id, key.repo.clone(), number);
        map.entry(key).or_insert_with(Instant::now).elapsed()
    }

    fn clear(&self, key: &LaneKey, number: PrNumber) {
        let mut map = self.0.lock().unwrap();
        map.remove(&(key.installation_id, key.repo.clone(), number));
    }
}

/// Each PR in a repo's queue as of its last tick: whose queue events they are, and what the
/// history keeps of it once it leaves.
#[derive(Default)]
pub struct Prs(std::sync::Mutex<HashMap<(u64, RepoName), HashMap<PrNumber, PrSummary>>>);

impl Prs {
    /// The PRs of the repo's lanes before they sync, so a PR that just left is still here, and
    /// the labeled PRs, so one that just joined is.
    fn update(&self, installation: u64, repo: &RepoName, lanes: &[Lane], labeled: &[PullRequest]) {
        let left = lanes.iter().flat_map(|lane| &lane.entries);
        let left = left.map(|e| (e.number, e.summary.clone()));
        let labeled = labeled.iter().map(|pr| (pr.number, pr.summary()));
        let prs = left.chain(labeled).collect();
        let mut map = self.0.lock().unwrap();
        map.insert((installation, repo.clone()), prs);
    }

    fn get(&self, key: &LaneKey, number: PrNumber) -> Option<PrSummary> {
        let map = self.0.lock().unwrap();
        map.get(&(key.installation_id, key.repo.clone()))?
            .get(&number)
            .cloned()
    }

    /// The PR's author, unless an App opened it: an App is no person to count events for.
    fn author(&self, key: &LaneKey, number: PrNumber) -> Option<String> {
        let author = self.get(key, number)?.author?;
        (!author.bot).then_some(author.login)
    }
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

/// Runs the queues while this instance holds the queue lock, and stops them if it loses it, or
/// once `stop` is set. Every instance serves the dashboard and the webhooks, but only one runs
/// the queues.
pub async fn lead(ctx: Arc<Ctx>, store: Arc<PgStore>, mut stop: watch::Receiver<bool>) {
    let mut waiting = false;
    loop {
        match store.try_queue_lock().await {
            Ok(Some(mut lock)) => {
                tracing::info!("holding the queue lock: running the queues");
                waiting = false;
                let (alive, mut stopped) = mpsc::channel::<()>(1);
                let queues = (
                    Spawned::spawn(supervise(ctx.clone(), alive.clone()), &alive),
                    Spawned::spawn(discover(ctx.clone()), &alive),
                    Spawned::spawn(prune(ctx.clone()), &alive),
                    Spawned::spawn(sync_seats(ctx.clone()), &alive),
                    Spawned::spawn(crate::redeliver::redeliver(ctx.clone()), &alive),
                    Spawned::spawn(crate::inbox::read_changed(ctx.clone()), &alive),
                    Spawned::spawn(crate::inbox::index(ctx.clone()), &alive),
                );
                let stopping = loop {
                    tokio::select! {
                        _ = tokio::time::sleep(LOCK_EVERY) => {}
                        _ = stop.wait_for(|s| *s) => break true,
                    }
                    let checked = tokio::time::timeout(LOCK_EVERY, lock.check()).await;
                    if let Err(e) = checked.map_err(anyhow::Error::from).and_then(|r| r) {
                        let error = format!("{e:#}");
                        tracing::error!(%error, "lost the queue lock: stopping the queues");
                        break false;
                    }
                };
                // The lock goes only once every queue task has stopped, so the queues never run
                // in two instances at once.
                drop((queues, alive));
                let _ = stopped.recv().await;
                drop(lock);
                if stopping {
                    return;
                }
            }
            Ok(None) if !waiting => {
                tracing::info!("another instance holds the queue lock: waiting for it");
                waiting = true;
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!(error = %format!("{e:#}"), "could not take the queue lock");
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(LOCK_EVERY) => {}
            _ = stop.wait_for(|s| *s) => return,
        }
    }
}

/// Deletes what nothing reads anymore, every `PRUNE_EVERY`: expired sessions, and job outputs
/// older than `JOB_OUTPUTS_FOR`.
async fn prune(ctx: Arc<Ctx>) {
    loop {
        let now = unix_now();
        match ctx.store.delete_expired_sessions(now).await {
            Ok(n) => tracing::info!(sessions = n, "deleted expired sessions"),
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "could not delete expired sessions")
            }
        }
        match ctx.store.prune_github_reads(GITHUB_READS_FOR).await {
            Ok(n) => tracing::info!(rows = n, "deleted old GitHub reads"),
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "could not delete old GitHub reads")
            }
        }
        match ctx.store.prune_inbox(INBOX_CLOSED_FOR).await {
            Ok(n) => tracing::info!(prs = n, "deleted PRs closed long ago from the inbox"),
            Err(e) => tracing::warn!(error = %format!("{e:#}"), "could not prune the inbox"),
        }
        let jobs = ctx.config.data_dir.join("jobs");
        match prune_job_outputs(&jobs, JOB_OUTPUTS_FOR).await {
            Ok(n) => tracing::info!(jobs = n, "deleted old job outputs"),
            Err(e) => tracing::warn!(error = %format!("{e:#}"), "could not delete old job outputs"),
        }
        if let Some(billing) = &ctx.billing {
            match billing.prune(now).await {
                Ok(n) => tracing::info!(rows = n, "deleted old fair-use counts"),
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "could not delete fair-use counts")
                }
            }
        }
        tokio::time::sleep(PRUNE_EVERY).await;
    }
}

/// Bills each subscription for its seats, every `SYNC_SEATS_EVERY`. One that fails goes again
/// next time.
async fn sync_seats(ctx: Arc<Ctx>) {
    let Some(billing) = &ctx.billing else {
        return;
    };
    loop {
        if let Err(e) = billing.sync().await {
            tracing::warn!(error = %format!("{e:#}"), "could not bill seats in Stripe");
        }
        tokio::time::sleep(SYNC_SEATS_EVERY).await;
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Deletes each job's output under `jobs` that has not changed for `age`: how many.
async fn prune_job_outputs(jobs: &Path, age: Duration) -> Result<usize> {
    let jobs = jobs.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut deleted = 0;
        let entries = match std::fs::read_dir(&jobs) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e.into()),
        };
        for entry in entries {
            let entry = entry?;
            let modified = entry.metadata()?.modified()?;
            if modified.elapsed().unwrap_or_default() >= age {
                std::fs::remove_dir_all(entry.path())?;
                deleted += 1;
            }
        }
        anyhow::Ok(deleted)
    })
    .await?
}

/// Held by every queue task until it has stopped: the channel closes once all have.
type Alive = mpsc::Sender<()>;

/// A spawned queue task, which is stopped when it is dropped: stopping a task stops the ones it
/// runs.
struct Spawned(JoinHandle<()>);

impl Spawned {
    fn spawn(future: impl Future<Output = ()> + Send + 'static, alive: &Alive) -> Self {
        let alive = alive.clone();
        Self(tokio::spawn(async move {
            let _alive = alive;
            future.await
        }))
    }

    fn is_finished(&self) -> bool {
        self.0.is_finished()
    }
}

impl Drop for Spawned {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Runs a queue for each repo that is on, and stops the queues of repos that are turned off.
/// Waits until the GitHub App exists.
async fn supervise(ctx: Arc<Ctx>, alive: Alive) {
    let mut running = Running::new();
    loop {
        if let Err(e) = reconcile(&ctx, &mut running, &alive).await {
            tracing::error!(error = %format!("{e:#}"), "could not update the running queues");
        }
        tokio::time::sleep(SUPERVISE_EVERY).await;
    }
}

/// Keeps track of which repos have the config file on their default branch, so that a repo
/// queues once it has one.
async fn discover(ctx: Arc<Ctx>) {
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
pub(crate) async fn app_credentials(ctx: &Ctx) -> Result<Option<AppCredentials>> {
    if let Some(app) = &ctx.hosted_app {
        return Ok(Some(app.clone()));
    }
    match ctx.vault.get(Scope::Instance, GITHUB_APP).await? {
        Some(json) => Ok(Some(serde_json::from_str(json.expose_secret())?)),
        None => Ok(None),
    }
}

/// Keyed by (installation, repo). The value keeps the App ID the task started with.
type Running = HashMap<(u64, RepoName), (u64, Spawned)>;

async fn reconcile(ctx: &Arc<Ctx>, running: &mut Running, alive: &Alive) -> Result<()> {
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
        }
        keep
    });
    for key in wanted {
        if let std::collections::hash_map::Entry::Vacant(slot) = running.entry(key) {
            let (installation, repo) = slot.key().clone();
            tracing::info!(installation, %repo, "watching");
            let gh = App::from_credentials(&creds)?.installation(installation);
            slot.insert((
                creds.id,
                Spawned::spawn(run_repo(ctx.clone(), gh, repo), alive),
            ));
        }
    }
    Ok(())
}

/// The least time between two ticks that a webhook starts. Webhooks come in bursts, a CI run's
/// checks most of all, and every tick spends the installation's rate limit: one tick takes in a
/// burst.
const TICK_GAP: Duration = Duration::from_secs(10);

async fn run_repo(ctx: Arc<Ctx>, gh: Installation, repo: RepoName) {
    let sandbox = JobSandbox { ctx: &ctx, gh: &gh };
    let cached = Cached {
        gh: &gh,
        reader: Reader {
            installation: gh.id,
            reads: &gh,
            store: ctx.store.as_ref(),
        },
    };
    loop {
        let started = tokio::time::Instant::now();
        // Without webhooks, only a read tells what changed.
        let webhooks = webhooks(&ctx).await.unwrap_or(false);
        let github: &dyn GitHub = if webhooks { &cached } else { &gh };
        let ran = tick(&ctx, github, &sandbox, &repo).await;
        // A failed tick may have written part of its work.
        if webhooks && !matches!(ran, Ok(Ran::Idle)) {
            for change in [Change::Repo, Change::ValuesUnder(String::new())] {
                if let Err(e) = cached.reader.changed(&repo, change).await {
                    tracing::error!(installation = gh.id, %repo, error = %format!("{e:#}"), "could not mark the repo changed");
                }
            }
        }
        match ran {
            // The lane may have a next PR: start it now, not a poll later.
            Ok(Ran::Job) => continue,
            Ok(Ran::Idle) => {}
            Err(e) => match e.downcast_ref::<RateLimited>() {
                // A webhook would only fail the same way: wait for the limit to lift.
                Some(limited) => {
                    tracing::warn!(installation = gh.id, %repo, wait_secs = limited.remaining().as_secs(), "GitHub's rate limit is used up, waiting");
                    tokio::time::sleep(limited.remaining()).await;
                    continue;
                }
                None => {
                    tracing::error!(installation = gh.id, %repo, error = %format!("{e:#}"), "tick failed")
                }
            },
        }
        ctx.wakers.wait(gh.id, &repo, ctx.config.poll).await;
        tokio::time::sleep_until(started + TICK_GAP).await;
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
    /// A stack gained or lost a PR while the job ran: it runs again, in its place.
    Restarted(String),
    /// A fork PR's author pushed after it was queued: it leaves the queue, for a maintainer to
    /// queue the new commits.
    Pushed(String),
}

/// One turn of each of the repo's lanes, one per base branch: the default branch first, then
/// each other branch a queued PR merges into, or a lane still holds. A lane runs at most one job
/// a turn, so a busy branch does not starve another. The lanes run one after the other: they
/// share the repo's git cache.
async fn tick(ctx: &Ctx, gh: &dyn GitHub, sandbox: &dyn Sandbox, repo: &RepoName) -> Result<Ran> {
    let Some(mut read) = read_repo(ctx, gh, repo).await? else {
        return Ok(Ran::Idle);
    };
    let mut ran = Ran::Idle;
    let mut failed = None;
    let mut visited = BTreeSet::new();
    while let Some(trunk) = read.trunks().into_iter().find(|t| !visited.contains(t)) {
        visited.insert(trunk.clone());
        match tick_lane(ctx, gh, sandbox, repo, &read, &trunk).await {
            // The lane may have let a retargeted PR go, for the next lane to take.
            Ok(Ran::Idle) => {
                read.lanes = ctx.store.repo_lanes(gh.installation_id(), repo).await?;
                continue;
            }
            Ok(Ran::Job) => ran = Ran::Job,
            // The other lanes still get their turn.
            Err(e) if failed.is_none() => failed = Some(e),
            Err(e) => {
                let error = format!("{e:#}");
                tracing::error!(installation = gh.installation_id(), %repo, %trunk, %error, "tick failed");
            }
        }
        // A job takes a while: the next lane reads GitHub again.
        match read_repo(ctx, gh, repo).await? {
            Some(r) => read = r,
            None => break,
        }
    }
    match failed {
        Some(e) => Err(e),
        None => Ok(ran),
    }
}

/// What a tick reads of a repo once, for all its lanes.
struct RepoRead {
    settings: RepoSettings,
    snap: RepoSnapshot,
    units: Vec<Unit>,
    /// The repo's lanes as saved, each with its trunk.
    lanes: Vec<(String, Lane)>,
    gate: Option<Hold>,
}

impl RepoRead {
    /// The branches with a lane to run: the default branch, each listed branch a queued unit
    /// merges into, and each with a saved lane: its PRs may have left, as GitHub closes the PRs
    /// of a deleted branch, or the branch left the list, and a drained lane goes.
    fn trunks(&self) -> Vec<String> {
        let others: BTreeSet<&String> = self
            .units
            .iter()
            .map(|u| self.lane_of(&u.base))
            .chain(self.lanes.iter().map(|(trunk, _)| trunk))
            .filter(|t| **t != self.snap.default_branch)
            .collect();
        let default = std::iter::once(self.snap.default_branch.clone());
        default.chain(others.into_iter().cloned()).collect()
    }

    /// The trunk of the lane a unit into `base` waits in. A branch without a queue is not
    /// listed in the settings, as a PR stacked by hand on another PR's branch: its units wait in
    /// the default branch's lane, held.
    fn lane_of<'a>(&'a self, base: &'a String) -> &'a String {
        match self.settings.queues(base, &self.snap.default_branch) {
            true => base,
            false => &self.snap.default_branch,
        }
    }

    fn lane(&self, trunk: &str) -> Lane {
        let saved = self.lanes.iter().find(|(t, _)| t == trunk);
        saved.map(|(_, lane)| lane.clone()).unwrap_or_default()
    }

    /// The units that merge into `trunk`. A PR retargeted from another branch joins only once
    /// that branch's lane has let it go, so its history reads in order.
    fn units_into(&self, trunk: &str) -> Vec<Unit> {
        let elsewhere = |number: PrNumber| {
            self.lanes
                .iter()
                .any(|(t, lane)| t != trunk && lane.entries.iter().any(|e| e.number == number))
        };
        self.units
            .iter()
            .filter(|u| self.lane_of(&u.base) == trunk && !elsewhere(u.number))
            .cloned()
            .collect()
    }
}

/// The repo's settings, labels, labeled PRs and lanes. `None` while the config file does not
/// parse: the queue is paused.
async fn read_repo(ctx: &Ctx, gh: &dyn GitHub, repo: &RepoName) -> Result<Option<RepoRead>> {
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
            return Ok(None);
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
    let lanes = ctx.store.repo_lanes(installation, repo).await?;
    ctx.unpinned.keep_labeled(installation, repo, &snap.labeled);
    let units = units(gh, repo, &snap.labeled).await?;
    let layers: Vec<PullRequest> = units.iter().flat_map(|u| u.layers.clone()).collect();
    let saved: Vec<Lane> = lanes.iter().map(|(_, lane)| lane.clone()).collect();
    ctx.prs.update(installation, repo, &saved, &layers);
    let gate = match &ctx.billing {
        Some(b) => {
            b.gate(installation, &repo.owner, snap.private, unix_now())
                .await?
        }
        None => None,
    };
    Ok(Some(RepoRead {
        settings,
        snap,
        units,
        lanes,
        gate,
    }))
}

/// Why a unit into `base` waits: the branch has no queue.
fn no_queue(base: &str, default_branch: &str, settings: &RepoSettings) -> String {
    let queues =
        std::iter::once(default_branch).chain(settings.branches.iter().map(String::as_str));
    let queues: Vec<String> = queues.map(|b| format!("`{b}`")).collect();
    format!(
        "targets `{base}`, which has no queue (queues: {}; `branches` in `{REPO_CONFIG_PATH}` adds one)",
        queues.join(", ")
    )
}

/// Syncs the lane of `trunk` with GitHub, and runs its next job, if it has one.
async fn tick_lane(
    ctx: &Ctx,
    gh: &dyn GitHub,
    sandbox: &dyn Sandbox,
    repo: &RepoName,
    read: &RepoRead,
    trunk: &str,
) -> Result<Ran> {
    let installation = gh.installation_id();
    let settings = read.settings.clone();
    let gate = read.gate.clone();
    let labels = settings.labels();
    let mut units = read.units_into(trunk);
    // The snapshot as this lane sees it: its trunk's head. A lane with no PR left reads none,
    // as its branch may be gone.
    let trunk_sha = if trunk == read.snap.default_branch {
        read.snap.trunk_sha.clone()
    } else if units.is_empty() {
        String::new()
    } else {
        gh.branch_sha(repo, trunk).await?
    };
    let snap = RepoSnapshot {
        trunk_sha,
        ..read.snap.clone()
    };
    let key = LaneKey {
        installation_id: installation,
        repo: repo.clone(),
        trunk: trunk.to_owned(),
    };
    let mut lane = read.lane(trunk);
    if let Some(merge) = lane.merging.clone() {
        let method = &snap.merge_method;
        let number = merge.number;
        if resume_merge(ctx, &settings, gh, &key, &mut lane, &units, merge, method).await? {
            units.retain(|u| u.number != number);
        }
    }

    let mut candidates = Vec::new();
    for unit in &units {
        if unit.stacked && unit.layers.iter().any(|pr| pr.is_cross_repository) {
            let why = "Stacks with a PR from a fork are not supported yet.";
            eject(ctx, &settings, gh, &key, &unit.prs(), why, "fork").await?;
            continue;
        }
        let mut pin = None;
        // A fork PR's head from its pull ref, which the PR's GraphQL lags behind.
        let mut top = unit.top().clone();
        if unit.fork() {
            top.head_ref_oid = gh.pull_head(repo, top.number).await?;
            match fork_pin(ctx, &key, &mut lane, &top, &labels.trigger).await? {
                Pin::Keep => {}
                Pin::Join(head) => pin = Some(head),
                Pin::Wait => continue,
                Pin::Drop { kind, why } => {
                    drop_fork(ctx, &settings, gh, &key, unit.number, kind, &why).await?;
                    continue;
                }
            }
        }
        let hold = if let Some(gate) = &gate {
            Some(gate.reason.clone())
        } else if unit.base != trunk {
            Some(no_queue(&unit.base, &read.snap.default_branch, &settings))
        } else if let Some(reason) = unit.hold_reason() {
            Some(reason)
        } else if unit.fork() {
            match fork_block(gh, &key, &snap.trunk_sha, &top).await? {
                Some(Block::Hold(reason)) => Some(reason),
                Some(Block::Eject(why)) => {
                    let why = format!("Ejected: {why}.");
                    eject(ctx, &settings, gh, &key, &unit.prs(), &why, "fork").await?;
                    continue;
                }
                None => None,
            }
        } else {
            None
        };
        candidates.push(Candidate {
            number: unit.number,
            priority: unit.layers.iter().any(|pr| pr.has_label(&labels.priority)),
            hold,
            summary: unit.layers[0].summary(),
            layers: if unit.stacked { unit.prs() } else { Vec::new() },
            pin,
        });
    }

    // The PRs of each entry, also of those that leave the lane now.
    let mut prs_of: HashMap<PrNumber, Vec<PrNumber>> =
        lane.entries.iter().map(|e| (e.number, e.prs())).collect();
    prs_of.extend(units.iter().map(|u| (u.number, u.prs())));
    for change in lane.sync(&candidates) {
        let (number, kind, message, comment) = match change {
            LaneChange::Enqueued(n) => {
                if let Some(b) = &ctx.billing {
                    let unit = units.iter().find(|u| u.number == n);
                    let authors = unit.map(Unit::authors).unwrap_or_default();
                    b.queued(installation, snap.private, &authors, unix_now())
                        .await?;
                }
                (n, "enqueued", "added to the queue".to_owned(), None)
            }
            LaneChange::Dequeued { number, .. } => {
                ctx.job_errors.clear(&key, number);
                (number, "dequeued", "left the queue".to_owned(), None)
            }
            LaneChange::Held { number, reason } => {
                let comment = match &gate {
                    Some(Hold { reason: r, comment }) if *r == reason => comment.clone(),
                    _ => on_hold(&reason),
                };
                (number, "held", reason, Some(comment))
            }
            LaneChange::Released(n) => (
                n,
                "released",
                "can merge again".to_owned(),
                Some("No longer on hold. Waiting in its place in the queue.".to_owned()),
            ),
        };
        let prs = prs_of.get(&number).cloned().unwrap_or(vec![number]);
        if kind == "dequeued" {
            exit_once(ctx, &key, &prs, kind, &message).await?;
        } else {
            record_all(ctx, &key, &prs, kind, &message).await?;
        }
        if let Some(comment) = comment {
            comment_all(gh, repo, &prs, &comment).await?;
        }
    }
    ctx.store.save_lane(&key, &lane).await?;

    // Jobs run inside the tick, so a job still active here was cut short by a restart. A single
    // PR runs again. A batch picks up where it was, unless what it tests changed: then it is
    // built again.
    if let Some(branch) = lane.batch_branch.clone() {
        if let Some(batch) = built_batch(&lane, &snap, &units, branch.clone()) {
            run_batch(ctx, &settings, gh, &key, &snap, &mut lane, batch).await?;
            return Ok(Ran::Job);
        }
        gh.delete_branch(repo, &branch).await?;
        lane.stop();
        ctx.store.save_lane(&key, &lane).await?;
    }
    let mut next = lane.active();
    if next.is_empty() {
        next = lane.next(settings.batch_size);
    }
    let Some(&number) = next.first() else {
        // The default branch's lane stays, empty, on the dashboard. Another goes once drained.
        if trunk != read.snap.default_branch && lane == Lane::default() {
            ctx.store.delete_lane(&key).await?;
        }
        return Ok(Ran::Idle);
    };
    let unit = units
        .iter()
        .find(|u| u.number == number)
        .context("the lane's next entry is not on GitHub")?
        .clone();
    let required = gh.required_checks(repo, &key.trunk).await?;

    // GitHub merges a PR behind the trunk, which a batch's PRs are, unless the trunk is strict.
    if next.len() > 1 && !required.strict {
        let ready = batchable(gh, repo, &units, &required.names, &next).await?;
        if let Some(batch) = build_batch(ctx, gh, &key, &snap, &mut lane, &ready).await? {
            run_batch(ctx, &settings, gh, &key, &snap, &mut lane, batch).await?;
            return Ok(Ran::Job);
        }
    }

    lane.activate(number);
    ctx.store.save_lane(&key, &lane).await?;

    let outcome = match run_job(ctx, &settings, gh, sandbox, &key, &snap, &mut lane, &unit).await {
        Ok(o) => o,
        // Not the PR's fault, and likely to pass: it stays active, so the next tick runs it again.
        Err(e) if ctx.job_errors.failed(&key, number) < JOB_ERRORS_FOR => {
            return Err(e.context(format!("the job of #{number}")));
        }
        Err(e) => Outcome::Ejected(EjectReason::RebaseFailed(format!("{e:#}"))),
    };
    ctx.job_errors.clear(&key, number);
    finish(ctx, &settings, gh, &key, &mut lane, &unit, outcome).await?;
    Ok(Ran::Job)
}

/// What the queue merges as one: a PR, or a stack of PRs, which GitHub merges all at once.
#[derive(Clone, Debug)]
struct Unit {
    /// The PR's number, or the stack's.
    number: PrNumber,
    /// The branch it merges into: the PR's base, or the stack's.
    base: String,
    /// A stack's open PRs, bottom to top, or the PR alone.
    layers: Vec<PullRequest>,
    stacked: bool,
}

impl Unit {
    /// The PR that GitHub merges: a stack's top layer, which merges every layer below it.
    fn top(&self) -> &PullRequest {
        self.layers.last().expect("a unit has a PR")
    }

    fn prs(&self) -> Vec<PrNumber> {
        self.layers.iter().map(|pr| pr.number).collect()
    }

    /// A PR from a fork, whose author is not trusted: the queue never batches it, runs no agent
    /// on it, and merges only a head it was queued at or published itself.
    fn fork(&self) -> bool {
        !self.stacked && self.top().is_cross_repository
    }

    /// How comments name it: `#7`, or a stack's layers, `#7, #8, #9`.
    fn refs(&self) -> String {
        let refs: Vec<String> = self
            .layers
            .iter()
            .map(|pr| format!("#{}", pr.number))
            .collect();
        refs.join(", ")
    }

    fn has_label(&self, label: &str) -> bool {
        self.layers.iter().any(|pr| pr.has_label(label))
    }

    /// The people who opened its PRs: no App, nor a deleted account.
    fn authors(&self) -> Vec<String> {
        let mut authors: Vec<String> = self
            .layers
            .iter()
            .filter_map(|pr| pr.summary().author)
            .filter(|a| !a.bot)
            .map(|a| a.login)
            .collect();
        authors.sort();
        authors.dedup();
        authors
    }

    /// Why GitHub would not merge it yet: a stack waits for every layer.
    fn hold_reason(&self) -> Option<String> {
        self.layers.iter().find_map(|pr| {
            let reason = pr.hold_reason()?;
            Some(if self.stacked {
                format!("#{} is not ready: {reason}", pr.number)
            } else {
                reason
            })
        })
    }

    /// The unit as GitHub shows it now.
    async fn refreshed(&self, gh: &dyn GitHub, repo: &RepoName) -> Result<Unit> {
        let mut layers = Vec::with_capacity(self.layers.len());
        for pr in &self.layers {
            layers.push(gh.pull_request(repo, pr.number).await?);
        }
        Ok(Unit {
            layers,
            ..self.clone()
        })
    }

    /// The branch a refreshed unit merges into now, if it is not `trunk`: it was retargeted,
    /// and leaves this lane for that branch's.
    fn retargeted(&self, trunk: &str) -> Option<String> {
        let base = self.layers[0].merges_into();
        (base != trunk).then(|| format!("{} now targets `{base}`", self.refs()))
    }

    /// How the stack changed since this unit was read: a PR joined or left it.
    async fn reshaped(&self, gh: &dyn GitHub, repo: &RepoName) -> Result<Option<String>> {
        if !self.stacked || gh.stack_layers(repo, self.number).await? == self.prs() {
            return Ok(None);
        }
        Ok(Some(format!("the stack of {} changed", self.refs())))
    }

    /// Each layer's branch head on GitHub, bottom to top.
    async fn branch_heads(&self, gh: &dyn GitHub, repo: &RepoName) -> Result<Vec<String>> {
        let mut heads = Vec::with_capacity(self.layers.len());
        for pr in &self.layers {
            heads.push(pr_head(gh, repo, pr).await?);
        }
        Ok(heads)
    }
}

/// A PR's head on GitHub now: its branch's, or for a fork PR, its pull ref's.
async fn pr_head(gh: &dyn GitHub, repo: &RepoName, pr: &PullRequest) -> Result<String> {
    match pr.is_cross_repository {
        true => gh.pull_head(repo, pr.number).await,
        false => gh.branch_sha(repo, &pr.head_ref_name).await,
    }
}

/// What the queue does with a fork PR's head, as the PR's pull ref shows it.
enum Pin {
    /// In the lane, at a head it may merge.
    Keep,
    /// Joins the lane at this head, the one its label went on at.
    Join(String),
    /// Not yet in the lane: its pin has not come, or the pull ref does not show it yet.
    Wait,
    /// Leaves the queue.
    Drop { kind: &'static str, why: String },
}

/// What a fork PR's author pushed after a maintainer queued it, nobody queued: the queue merges
/// only the head the label went on at, and the heads it published itself. The pin comes from
/// the `labeled` webhook; only without webhooks is it the head at first sight.
async fn fork_pin(
    ctx: &Ctx,
    key: &LaneKey,
    lane: &mut Lane,
    pr: &PullRequest,
    label: &str,
) -> Result<Pin> {
    let head = &pr.head_ref_oid;
    if let Some(entry) = lane.entries.iter().find(|e| e.number == pr.number) {
        return Ok(match entry.heads.contains(head) {
            true => Pin::Keep,
            // Queued before the queue pinned fork PRs.
            false if entry.heads.is_empty() => {
                lane.accept(pr.number, head);
                Pin::Keep
            }
            false => Pin::Drop {
                kind: "pushed",
                why: "new commits were pushed to it after it was queued".into(),
            },
        });
    }
    let labeled = ctx
        .store
        .label_head(key.installation_id, &key.repo, pr.number, label)
        .await?;
    let pin = match labeled {
        Some(pin) => Some(pin),
        None if !webhooks(ctx).await? => Some(head.clone()),
        None => None,
    };
    if pin.as_ref() == Some(head) {
        ctx.unpinned.forget(key, pr.number);
        return Ok(Pin::Join(head.clone()));
    }
    // The webhook may come after the poll, and the pull ref may lag behind the label.
    if ctx.unpinned.waited(key, pr.number) < PIN_WAIT {
        return Ok(Pin::Wait);
    }
    ctx.unpinned.forget(key, pr.number);
    Ok(match pin {
        Some(_) => Pin::Drop {
            kind: "pushed",
            why: "new commits were pushed to it after its label went on".into(),
        },
        None => Pin::Drop {
            kind: "unpinned",
            why: "GitHub sent no webhook for its label, so Mergepatrol cannot tell which commits were queued".into(),
        },
    })
}

/// Whether GitHub delivers webhooks to this Mergepatrol: it does only with a secret, and to https.
pub(crate) async fn webhooks(ctx: &Ctx) -> Result<bool> {
    let secret = app_credentials(ctx)
        .await?
        .is_some_and(|app| app.webhook_secret.is_some());
    Ok(secret && ctx.config.public_url.starts_with("https://"))
}

/// Takes a fork PR out of the queue, for a maintainer to queue it as it is now.
async fn drop_fork(
    ctx: &Ctx,
    settings: &RepoSettings,
    gh: &dyn GitHub,
    key: &LaneKey,
    number: PrNumber,
    kind: &str,
    why: &str,
) -> Result<()> {
    let labels = settings.labels();
    record(ctx, key, number, kind, why).await?;
    gh.remove_label(&key.repo, number, &labels.trigger).await?;
    gh.remove_label(&key.repo, number, &labels.priority).await?;
    let comment = format!(
        "Dequeued: {why}. Mergepatrol merges a PR from a fork only as it was when its label went on. Add `{}` again to queue it as it is now.",
        labels.trigger
    );
    gh.sticky_comment(&key.repo, number, &comment).await
}

/// What stops the queue from taking a fork PR on.
#[derive(Debug, PartialEq, Eq)]
enum Block {
    /// A maintainer, or the author, can lift it without a push.
    Hold(String),
    /// Only the author's push can mend it, which drops the PR from the queue anyway.
    Eject(String),
}

/// Why the queue cannot take fork PR `pr` on now, onto the trunk at `trunk_sha`. A PR already on
/// the trunk needs nothing pushed to its fork.
async fn fork_block(
    gh: &dyn GitHub,
    key: &LaneKey,
    trunk_sha: &str,
    pr: &PullRequest,
) -> Result<Option<Block>> {
    let repo = &key.repo;
    let trunk = &key.trunk;
    if gh.awaiting_approval(repo, &pr.head_ref_oid).await? {
        return Ok(Some(Block::Hold(AWAITING_APPROVAL.into())));
    }
    if !gh.behind(repo, trunk_sha, &pr.head_ref_oid).await? {
        return Ok(None);
    }
    let rebase_it = format!("rebase it onto `{trunk}` yourself");
    let org = pr
        .head_repository
        .as_ref()
        .map(|r| r.owner.kind == "Organization");
    Ok(match org {
        None => Some(Block::Eject(format!(
            "it is behind `{trunk}`, and its fork is gone, so Mergepatrol cannot push a rebase"
        ))),
        Some(true) => Some(Block::Eject(format!(
            "it is behind `{trunk}`, and Mergepatrol cannot push to a fork that an organization owns: {rebase_it}"
        ))),
        Some(false)
            if gh
                .changes_under(repo, pr.number, ".github/workflows/")
                .await? =>
        {
            Some(Block::Eject(format!(
                "it is behind `{trunk}`, and it changes workflow files, which GitHub does not let Mergepatrol push to a fork: {rebase_it}"
            )))
        }
        Some(false) if !pr.maintainer_can_modify => Some(Block::Hold(format!(
            "it is behind `{trunk}`, and Mergepatrol can push the rebase to its fork only once its author allows edits by maintainers"
        ))),
        Some(false) => None,
    })
}

/// The queue's units: each labeled PR, and each stack with a labeled layer, with all its open
/// layers. In the order of `labeled`, a stack where its first labeled layer is.
async fn units(gh: &dyn GitHub, repo: &RepoName, labeled: &[PullRequest]) -> Result<Vec<Unit>> {
    let mut units: Vec<Unit> = Vec::new();
    for pr in labeled {
        let Some(stack) = &pr.stack else {
            units.push(Unit {
                number: pr.number,
                base: pr.base_ref_name.clone(),
                layers: vec![pr.clone()],
                stacked: false,
            });
            continue;
        };
        if units.iter().any(|u| u.number == stack.number) {
            continue;
        }
        let mut layers = Vec::new();
        for n in gh.stack_layers(repo, stack.number).await? {
            layers.push(match labeled.iter().find(|p| p.number == n) {
                Some(p) => p.clone(),
                None => gh.pull_request(repo, n).await?,
            });
        }
        if layers.is_empty() {
            continue;
        }
        units.push(Unit {
            number: stack.number,
            base: stack.base_ref_name.clone(),
            layers,
            stacked: true,
        });
    }
    Ok(units)
}

async fn record_all(
    ctx: &Ctx,
    key: &LaneKey,
    prs: &[PrNumber],
    kind: &str,
    message: &str,
) -> Result<()> {
    for n in prs {
        record(ctx, key, *n, kind, message).await?;
    }
    Ok(())
}

/// Each of `prs` left the queue.
async fn exit_all(
    ctx: &Ctx,
    key: &LaneKey,
    prs: &[PrNumber],
    outcome: &str,
    detail: &str,
    reason: Option<&str>,
) -> Result<()> {
    for n in prs {
        exit(ctx, key, *n, outcome, detail, reason).await?;
    }
    Ok(())
}

/// `exit_all`, but for the PRs whose exit is not recorded yet. A tick cut short after an exit,
/// before it saved the lane, left the PR in it: the next tick sees it leave again, or sees its
/// merge through again.
async fn exit_once(
    ctx: &Ctx,
    key: &LaneKey,
    prs: &[PrNumber],
    outcome: &str,
    detail: &str,
) -> Result<()> {
    let mut fresh = Vec::new();
    for n in prs {
        match ctx.store.has_left(key, *n).await? {
            true => tracing::info!(repo = %key.repo, pr = n, outcome, "already left the queue"),
            false => fresh.push(*n),
        }
    }
    exit_all(ctx, key, &fresh, outcome, detail, None).await
}

async fn comment_all(gh: &dyn GitHub, repo: &RepoName, prs: &[PrNumber], body: &str) -> Result<()> {
    try_join_all(prs.iter().map(|n| gh.sticky_comment(repo, *n, body))).await?;
    Ok(())
}

/// The units of `next` that can join a batch as they are, in order, up to the first that cannot:
/// GitHub must merge each without a rebase, so the required checks of each of its PRs passed.
async fn batchable(
    gh: &dyn GitHub,
    repo: &RepoName,
    units: &[Unit],
    required: &[String],
    next: &[PrNumber],
) -> Result<Vec<Unit>> {
    let mut ready = Vec::new();
    'next: for number in next {
        let Some(unit) = units.iter().find(|u| u.number == *number) else {
            break;
        };
        // A batch's draft PR runs CI with the repo's secrets, which a fork's code must not get.
        if unit.fork() {
            break;
        }
        for pr in &unit.layers {
            let checks = gh.checks(repo, &pr.head_ref_oid).await?;
            if checks.verdict(required).0 != CheckState::Passed {
                break 'next;
            }
        }
        ready.push(unit.clone());
    }
    Ok(ready)
}

async fn finish(
    ctx: &Ctx,
    settings: &RepoSettings,
    gh: &dyn GitHub,
    key: &LaneKey,
    lane: &mut Lane,
    unit: &Unit,
    outcome: Outcome,
) -> Result<()> {
    let repo = &key.repo;
    let prs = unit.prs();
    lane.merging = None;
    match outcome {
        Outcome::Merged(sha) => {
            merged(ctx, settings, gh, key, lane, unit.number, &prs, &sha).await?
        }
        Outcome::Ejected(reason) => {
            lane.eject(unit.number);
            eject(
                ctx,
                settings,
                gh,
                key,
                &prs,
                &format!("Ejected: {reason}."),
                reason.kind(),
            )
            .await?;
        }
        Outcome::Held(reason) => {
            lane.hold(unit.number, reason.clone());
            record_all(ctx, key, &prs, "held", &reason).await?;
            comment_all(gh, repo, &prs, &on_hold(&reason)).await?;
        }
        Outcome::Cancelled => {
            lane.remove(unit.number);
            exit_all(ctx, key, &prs, "cancelled", "the label was removed", None).await?;
        }
        Outcome::Restarted(why) => {
            lane.stop();
            record_all(ctx, key, &prs, "restarted", &why).await?;
        }
        Outcome::Pushed(why) => {
            lane.remove(unit.number);
            drop_fork(ctx, settings, gh, key, unit.number, "pushed", &why).await?;
            exit(ctx, key, unit.number, "dequeued", &why, None).await?;
        }
    }
    ctx.store.save_lane(key, lane).await
}

/// The entry `number` merged as `sha`, with its PRs `prs`.
#[allow(clippy::too_many_arguments)]
async fn merged(
    ctx: &Ctx,
    settings: &RepoSettings,
    gh: &dyn GitHub,
    key: &LaneKey,
    lane: &mut Lane,
    number: PrNumber,
    prs: &[PrNumber],
    sha: &str,
) -> Result<()> {
    let repo = &key.repo;
    lane.remove(number);
    exit_once(ctx, key, prs, "merged", sha).await?;
    let labels = settings.labels();
    comment_all(gh, repo, prs, &format!("Merged as {sha}.")).await?;
    for n in prs {
        tokio::try_join!(
            gh.remove_label(repo, *n, &labels.trigger),
            gh.remove_label(repo, *n, &labels.priority),
        )?;
    }
    Ok(())
}

/// Saves in the lane the merge of `unit` at its top's `head`, before GitHub is asked for it: a
/// restart while GitHub merges leaves it to `resume_merge`, not to the sync, which takes merged
/// PRs for dequeued.
async fn start_merge(
    ctx: &Ctx,
    key: &LaneKey,
    lane: &mut Lane,
    unit: &Unit,
    head: &str,
) -> Result<()> {
    lane.merging = Some(PendingMerge {
        number: unit.number,
        prs: unit.prs(),
        top: unit.top().number,
        head: head.to_owned(),
    });
    ctx.store.save_lane(key, lane).await
}

/// Sees through the merge a restart cut short, and whether it merged. Asked again while it is
/// still open, GitHub waits for the merge pending, or merges it as the job would have. Otherwise
/// it merged, or it left the queue unmerged.
#[allow(clippy::too_many_arguments)]
async fn resume_merge(
    ctx: &Ctx,
    settings: &RepoSettings,
    gh: &dyn GitHub,
    key: &LaneKey,
    lane: &mut Lane,
    units: &[Unit],
    merge: PendingMerge,
    method: &str,
) -> Result<bool> {
    let repo = &key.repo;
    let open = units.iter().any(|u| {
        u.number == merge.number
            && u.top().number == merge.top
            && u.top().head_ref_oid == merge.head
            && u.hold_reason().is_none()
    });
    let sha = if open {
        match gh.merge(repo, merge.top, &merge.head, method).await? {
            MergeOutcome::Merged { sha } => Some(sha),
            MergeOutcome::Rejected(_) => None,
        }
    } else {
        gh.merged_as(repo, merge.top, &merge.head).await?
    };
    lane.merging = None;
    if let Some(sha) = &sha {
        merged(ctx, settings, gh, key, lane, merge.number, &merge.prs, sha).await?;
    }
    ctx.store.save_lane(key, lane).await?;
    Ok(sha.is_some())
}

fn on_hold(reason: &str) -> String {
    format!(
        "On hold: {reason}. It keeps its place in the queue, and goes on once GitHub would merge it."
    )
}

/// PRs and stacks merged together on a branch of their own, which a draft PR puts through CI.
struct Batch {
    branch: String,
    /// The trunk commit the branch starts from.
    trunk_sha: String,
    /// Each unit, as tested, with the merge commit its top head made on the branch.
    steps: Vec<(Unit, BranchMerge)>,
}

impl Batch {
    fn built(&self) -> BuiltBatch {
        BuiltBatch {
            trunk_sha: self.trunk_sha.clone(),
            steps: self
                .steps
                .iter()
                .map(|(unit, m)| BatchStep {
                    number: unit.number,
                    head_sha: unit.top().head_ref_oid.clone(),
                    sha: m.sha.clone(),
                    tree: m.tree.clone(),
                    layers: match unit.stacked {
                        true => unit
                            .layers
                            .iter()
                            .map(|pr| LayerHead {
                                number: pr.number,
                                head_sha: pr.head_ref_oid.clone(),
                            })
                            .collect(),
                        false => Vec::new(),
                    },
                })
                .collect(),
        }
    }

    fn members(&self) -> Vec<PrNumber> {
        self.steps.iter().map(|(u, _)| u.number).collect()
    }

    /// The other units of the batch, as comments name them.
    fn others(&self, unit: &Unit) -> String {
        let refs: Vec<String> = self
            .steps
            .iter()
            .filter(|(u, _)| u.number != unit.number)
            .map(|(u, _)| u.refs())
            .collect();
        refs.join(", ")
    }
}

/// The lane's batch, as it was built on `branch`, while it still tests what is queued now.
fn built_batch(lane: &Lane, snap: &RepoSnapshot, units: &[Unit], branch: String) -> Option<Batch> {
    let built = lane.batch_built.as_ref()?;
    let steps = built
        .steps
        .iter()
        .map(|s| {
            let mut unit = units.iter().find(|u| u.number == s.number)?.clone();
            // As tested: `stale` compares them with GitHub now.
            if unit.stacked {
                let tested: Vec<PrNumber> = s.layers.iter().map(|l| l.number).collect();
                if tested != unit.prs() {
                    return None;
                }
                for (pr, l) in unit.layers.iter_mut().zip(&s.layers) {
                    pr.head_ref_oid = l.head_sha.clone();
                }
            } else {
                unit.layers[0].head_ref_oid = s.head_sha.clone();
            }
            let merge = BranchMerge {
                sha: s.sha.clone(),
                tree: s.tree.clone(),
            };
            Some((unit, merge))
        })
        .collect::<Option<Vec<_>>>()?;
    let batch = Batch {
        branch,
        trunk_sha: built.trunk_sha.clone(),
        steps,
    };
    stale(&snap.trunk_sha, units, &batch)
        .is_none()
        .then_some(batch)
}

/// Merges the top head of each unit one after the other onto a new branch at the trunk, up to
/// the first that conflicts. `None` when fewer than two merged: the first unit then runs alone.
async fn build_batch(
    ctx: &Ctx,
    gh: &dyn GitHub,
    key: &LaneKey,
    snap: &RepoSnapshot,
    lane: &mut Lane,
    units: &[Unit],
) -> Result<Option<Batch>> {
    if units.len() < 2 {
        return Ok(None);
    }
    let repo = &key.repo;
    let numbers: Vec<String> = units.iter().map(|u| u.number.to_string()).collect();
    let branch = format!("mergepatrol/batch-{}", numbers.join("-"));
    lane.batch_branch = Some(branch.clone());
    ctx.store.save_lane(key, lane).await?;

    // A batch of the same PRs that stopped earlier left its branch.
    gh.delete_branch(repo, &branch).await?;
    gh.create_branch(repo, &branch, &snap.trunk_sha).await?;
    let mut steps = Vec::new();
    for unit in units {
        let message = format!("Merge {} into the batch", unit.refs());
        match gh
            .merge_into_branch(repo, &branch, &unit.top().head_ref_oid, &message)
            .await?
        {
            Some(m) => steps.push((unit.clone(), m)),
            None => break,
        }
    }
    if steps.len() < 2 {
        gh.delete_branch(repo, &branch).await?;
        lane.batch_branch = None;
        ctx.store.save_lane(key, lane).await?;
        return Ok(None);
    }
    let batch = Batch {
        branch,
        trunk_sha: snap.trunk_sha.clone(),
        steps,
    };
    lane.batch_built = Some(batch.built());
    ctx.store.save_lane(key, lane).await?;
    Ok(Some(batch))
}

/// How a batch's checks ended.
enum Tested {
    Passed,
    /// Why: the checks that failed, or those that did not finish in time.
    Failed(String),
    /// Something changed under the batch, so what it tested is stale.
    Stale(String),
}

/// Puts a batch through CI on a draft PR. When it passes, merges its units in order, each only
/// while the trunk comes out as the tree the batch tested. The units it does not finish go back
/// to their places.
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
    for n in batch.members() {
        lane.activate(n);
    }
    ctx.store.save_lane(key, lane).await?;

    let result = test_and_merge(ctx, settings, gh, key, snap, lane, &batch).await;
    // Not the batch's fault, and likely to pass: it stays, so the next tick picks it up where it
    // was, as after a restart, rather than building it and starting its CI again.
    let first = batch.members()[0];
    if result.is_err() && ctx.job_errors.failed(key, first) < JOB_ERRORS_FOR {
        return result;
    }
    ctx.job_errors.clear(key, first);
    // Deleting the branch closes the draft PR.
    gh.delete_branch(repo, &batch.branch).await?;
    lane.stop();
    ctx.store.save_lane(key, lane).await?;
    result
}

async fn test_and_merge(
    ctx: &Ctx,
    settings: &RepoSettings,
    gh: &dyn GitHub,
    key: &LaneKey,
    snap: &RepoSnapshot,
    lane: &mut Lane,
    batch: &Batch,
) -> Result<()> {
    let repo = &key.repo;
    match lane.batch_pr {
        // Picked up after a restart: the draft PR and the comments are there.
        Some(draft) => {
            for (unit, _) in &batch.steps {
                let why = format!("still tested in #{draft}");
                record_all(ctx, key, &unit.prs(), "batch-resumed", &why).await?;
            }
        }
        None => {
            // `#n` in code spans: plain references would add a timeline event to every PR, on
            // every batch.
            let list: Vec<String> = batch
                .steps
                .iter()
                .flat_map(|(u, _)| &u.layers)
                .map(|pr| format!("- `#{}` {}", pr.number, pr.title))
                .collect();
            let first = &batch.steps[0].0.layers[0];
            let title = format!(
                "{} (+{} in a Mergepatrol batch)",
                first.title,
                list.len() - 1
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
            try_join_all(batch.steps.iter().map(|(unit, _)| async move {
                let others = batch.others(unit);
                let why = format!("tested with {others} in #{draft}");
                record_all(ctx, key, &unit.prs(), "batch", &why).await?;
                let comment = format!("Testing together with {others} in #{draft}.");
                comment_all(gh, repo, &unit.prs(), &comment).await
            }))
            .await?;
        }
    }

    let head = &batch.steps.last().expect("a batch has PRs").1.sha;
    let opened = Instant::now();
    let tested = loop {
        let now = gh.snapshot(repo, &settings.labels()).await?;
        let now_units = units(gh, repo, &now.labeled).await?;
        let trunk_sha = match key.trunk == now.default_branch {
            true => now.trunk_sha,
            false => gh.branch_sha(repo, &key.trunk).await?,
        };
        if let Some(why) = stale(&trunk_sha, &now_units, batch) {
            break Tested::Stale(why);
        }
        match checks_verdict(gh, key, head, opened).await? {
            (CheckState::Passed, _) => break Tested::Passed,
            (CheckState::Failed, names) => {
                break Tested::Failed(format!("required checks failed: {}", names.join(", ")));
            }
            // As a failure: a smaller batch, then the PR alone, runs its checks again.
            (CheckState::Pending, names) if opened.elapsed() >= CHECKS_TIMEOUT => {
                break Tested::Failed(EjectReason::ChecksTimedOut(names).to_string());
            }
            (CheckState::Pending, _) => {}
        }
        ctx.wakers
            .wait(key.installation_id, repo, CHECKS_POLL)
            .await;
    };

    match tested {
        Tested::Stale(why) => {
            for (unit, _) in &batch.steps {
                record_all(ctx, key, &unit.prs(), "batch-stopped", &why).await?;
            }
            Ok(())
        }
        Tested::Failed(why) => {
            lane.batch_failed(&batch.members());
            try_join_all(batch.steps.iter().map(|(unit, _)| {
                let why = &why;
                async move {
                    record_all(ctx, key, &unit.prs(), "batch-failed", why).await?;
                    let comment = format!(
                        "The batch with {} failed: {why}. Mergepatrol now tests these PRs in smaller batches, to find the one that broke it.",
                        batch.others(unit)
                    );
                    comment_all(gh, repo, &unit.prs(), &comment).await
                }
            }))
            .await?;
            Ok(())
        }
        Tested::Passed => merge_batch(ctx, settings, gh, key, snap, lane, batch).await,
    }
}

/// Why what the batch tested no longer stands: the trunk moved, or one of its units changed.
fn stale(trunk_sha: &str, units: &[Unit], batch: &Batch) -> Option<String> {
    if trunk_sha != batch.trunk_sha {
        return Some("the trunk moved outside the queue".into());
    }
    for (tested, _) in &batch.steps {
        let Some(current) = units.iter().find(|u| u.number == tested.number) else {
            return Some(format!("{} left the queue", tested.refs()));
        };
        if current.prs() != tested.prs() {
            return Some(format!("the stack of {} changed", tested.refs()));
        }
        for (now, then) in current.layers.iter().zip(&tested.layers) {
            if now.head_ref_oid != then.head_ref_oid {
                return Some(format!("#{} was pushed to", now.number));
            }
        }
        if let Some(hold) = current.hold_reason() {
            return Some(format!("{} is on hold: {hold}", tested.refs()));
        }
    }
    None
}

/// Merges the units of a batch that passed, in its order: a stack all at once. Each merge must
/// leave the trunk at the tree of the batch's commit for that unit; otherwise the rest would
/// merge untested.
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
        let now = tested.refreshed(gh, repo).await?;
        if !now.has_label(&settings.label) || now.branch_heads(gh, repo).await? != head_shas(tested)
        {
            let why = "the PR changed before its merge";
            record_all(ctx, key, &tested.prs(), "batch-stopped", why).await?;
            return Ok(());
        }
        if let Some(why) = now.reshaped(gh, repo).await?.or(now.retargeted(&key.trunk)) {
            record_all(ctx, key, &tested.prs(), "batch-stopped", &why).await?;
            return Ok(());
        }
        if let Some(reason) = now.hold_reason() {
            return finish(ctx, settings, gh, key, lane, tested, Outcome::Held(reason)).await;
        }
        let top = tested.top();
        start_merge(ctx, key, lane, tested, &top.head_ref_oid).await?;
        match merge_retrying(gh, repo, top.number, &top.head_ref_oid, &snap.merge_method).await? {
            MergeOutcome::Merged { sha } => {
                let tree = gh.commit_tree(repo, &sha).await?;
                let merged = Outcome::Merged(sha.clone());
                finish(ctx, settings, gh, key, lane, tested, merged).await?;
                if tree != step.tree {
                    let why = format!(
                        "the trunk came out as tree {tree}, not the batch's {}",
                        step.tree
                    );
                    record_all(ctx, key, &tested.prs(), "diverged", &why).await?;
                    return Ok(());
                }
            }
            MergeOutcome::Rejected(r) => {
                let reason = Outcome::Ejected(EjectReason::MergeRejected(r));
                return finish(ctx, settings, gh, key, lane, tested, reason).await;
            }
        }
    }
    Ok(())
}

fn head_shas(unit: &Unit) -> Vec<String> {
    unit.layers
        .iter()
        .map(|pr| pr.head_ref_oid.clone())
        .collect()
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

/// A queue event: shown on the dashboard, logged, and counted for its PR's author and workspace.
async fn record(
    ctx: &Ctx,
    key: &LaneKey,
    number: PrNumber,
    kind: &str,
    message: &str,
) -> Result<()> {
    ctx.analytics.queue_event(
        key.installation_id,
        &key.repo.to_string(),
        &key.trunk,
        number,
        kind,
        ctx.prs.author(key, number).as_deref(),
    );
    log(ctx, key, number, kind, message).await
}

/// A queue event, shown on the dashboard and logged but not counted.
async fn log(ctx: &Ctx, key: &LaneKey, number: PrNumber, kind: &str, message: &str) -> Result<()> {
    tracing::info!(repo = %key.repo, pr = number, kind, detail = message, "queue");
    ctx.store.record(key, number, kind, message).await
}

/// A PR left the queue: the event, and the PR as it was then, for the history. `reason` is an
/// ejected PR's kind of reason.
async fn exit(
    ctx: &Ctx,
    key: &LaneKey,
    number: PrNumber,
    outcome: &str,
    detail: &str,
    reason: Option<&str>,
) -> Result<()> {
    log(ctx, key, number, outcome, detail).await?;
    forget_refs(ctx, key, number).await;
    let summary = ctx.prs.get(key, number).unwrap_or_default();
    let steps = ctx
        .store
        .record_exit(key, number, outcome, detail, &summary)
        .await?;
    ctx.analytics.queue_exit(
        key.installation_id,
        &key.repo.to_string(),
        &key.trunk,
        number,
        outcome,
        ctx.prs.author(key, number).as_deref(),
        &Exit {
            steps: steps.as_ref(),
            reason,
        },
    );
    Ok(())
}

/// Deletes the PR's refs in the repo's cache: once nothing points at its commits, git's
/// automatic gc lets them go.
async fn forget_refs(ctx: &Ctx, key: &LaneKey, number: PrNumber) {
    let cache = cache_path(&ctx.config.data_dir, &key.repo.owner, &key.repo.name);
    if !cache.join("HEAD").exists() {
        return;
    }
    let patterns =
        ["pr", "result", "publish"].map(|kind| format!("refs/mergepatrol/{kind}/{number}"));
    let patterns: Vec<&str> = patterns.iter().map(String::as_str).collect();
    if let Err(e) = Git.delete_refs(&cache, &patterns).await {
        tracing::warn!(repo = %key.repo, pr = number, error = %format!("{e:#}"), "could not delete the PR's refs");
    }
}

async fn eject(
    ctx: &Ctx,
    settings: &RepoSettings,
    gh: &dyn GitHub,
    key: &LaneKey,
    prs: &[PrNumber],
    why: &str,
    reason: &str,
) -> Result<()> {
    let labels = settings.labels();
    let comment = format!("{why}\n\nAdd `{}` again to retry.", labels.trigger);
    for n in prs {
        gh.remove_label(&key.repo, *n, &labels.trigger).await?;
        gh.add_label(&key.repo, *n, &labels.failed).await?;
        gh.sticky_comment(&key.repo, *n, &comment).await?;
        exit(ctx, key, *n, "ejected", why, Some(reason)).await?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_job(
    ctx: &Ctx,
    settings: &RepoSettings,
    gh: &dyn GitHub,
    sandbox: &dyn Sandbox,
    key: &LaneKey,
    snap: &RepoSnapshot,
    lane: &mut Lane,
    unit: &Unit,
) -> Result<Outcome> {
    let repo = &key.repo;
    let prs = unit.prs();
    // No agent fixes a fork PR: its code could read the agent's key.
    let fix_attempts = match unit.fork() {
        true => 0,
        false => settings.fix_ci.allowed(),
    };
    let mut job = Job::start(unit.number, fix_attempts);
    let mut published_at = Instant::now();
    // Each layer's head as published, bottom to top, and the layer whose checks failed.
    let mut heads: Vec<String> = Vec::new();
    let mut failing = 0;

    loop {
        let mut now = unit.refreshed(gh, repo).await?;
        if !now.has_label(&settings.label) {
            return Ok(Outcome::Cancelled);
        }
        if let Some(why) = now.reshaped(gh, repo).await? {
            return Ok(Outcome::Restarted(why));
        }
        if let Some(why) = now.retargeted(&key.trunk) {
            return Ok(Outcome::Restarted(why));
        }
        if now.fork() {
            let head = pr_head(gh, repo, now.top()).await?;
            let queued = lane.entries.iter().find(|e| e.number == unit.number);
            if !queued.is_some_and(|e| e.heads.contains(&head)) {
                let why = "new commits were pushed to it after it was queued";
                return Ok(Outcome::Pushed(why.into()));
            }
            // The head the job may work on: `Sandbox::rebase` refuses any other.
            now.layers[0].head_ref_oid = head;
        }

        let event = match &job.phase {
            Phase::Rebasing => 'rebase: {
                if now.fork() {
                    let trunk = gh.branch_sha(repo, &key.trunk).await?;
                    match fork_block(gh, key, &trunk, now.top()).await? {
                        Some(Block::Eject(why)) => {
                            return Ok(Outcome::Ejected(EjectReason::Fork(why)));
                        }
                        Some(Block::Hold(reason)) => break 'rebase Event::OnHold(reason),
                        None => {}
                    }
                }
                record_all(ctx, key, &prs, "rebasing", &format!("onto {}", key.trunk)).await?;
                let comment = format!("Rebasing onto `{}`.", key.trunk);
                comment_all(gh, repo, &prs, &comment).await?;
                let event = sandbox.rebase(key, snap, &now, settings).await?;
                published_at = Instant::now();
                if let Event::Rebased { head, .. } = &event {
                    if now.fork() {
                        lane.accept(unit.number, head);
                        ctx.store.save_lane(key, lane).await?;
                    }
                    heads = now.branch_heads(gh, repo).await?;
                    record_all(ctx, key, &prs, "published", head).await?;
                    let comment = format!("Rebased to {head}. Waiting for checks.");
                    comment_all(gh, repo, &prs, &comment).await?;
                }
                event
            }
            Phase::AwaitingChecks { base, .. } => {
                let trunk = gh.branch_sha(repo, &key.trunk).await?;
                // The refs, not `head_ref_oid`: the PR lags behind our own publish for a few
                // seconds.
                if now.branch_heads(gh, repo).await? != heads {
                    let why = "the PR branch was pushed to";
                    record_all(ctx, key, &prs, "head-moved", why).await?;
                    Event::HeadMoved
                } else if trunk != *base {
                    let why = "the trunk moved outside the queue";
                    record_all(ctx, key, &prs, "base-moved", why).await?;
                    Event::BaseMoved
                } else if now.fork() && gh.awaiting_approval(repo, &heads[0]).await? {
                    // Runs waiting for approval can show as failed checks.
                    Event::OnHold(AWAITING_APPROVAL.into())
                } else {
                    // GitHub merges a stack only when every layer's required checks passed.
                    let mut verdict = (CheckState::Passed, Vec::new());
                    for (i, head) in heads.iter().enumerate() {
                        match checks_verdict(gh, key, head, published_at).await? {
                            (CheckState::Failed, names) => {
                                failing = i;
                                verdict = (CheckState::Failed, names);
                                break;
                            }
                            (CheckState::Pending, names) => {
                                verdict.0 = CheckState::Pending;
                                verdict.1.extend(names);
                            }
                            (CheckState::Passed, _) => {}
                        }
                    }
                    match verdict {
                        (CheckState::Passed, _) => Event::ChecksPassed,
                        (CheckState::Failed, names) => Event::ChecksFailed(names),
                        (CheckState::Pending, names)
                            if published_at.elapsed() >= CHECKS_TIMEOUT =>
                        {
                            Event::ChecksTimedOut(names)
                        }
                        (CheckState::Pending, _) => {
                            ctx.wakers
                                .wait(key.installation_id, repo, CHECKS_POLL)
                                .await;
                            continue;
                        }
                    }
                }
            }
            Phase::Fixing { base, failed, .. } => {
                let attempt = format!("attempt {} of {}", job.fixes, job.fix_attempts);
                let layer = &now.layers[failing];
                let on = match now.stacked {
                    true => format!(" on #{}", layer.number),
                    false => String::new(),
                };
                let what = format!("{}{on}, {attempt}", failed.join(", "));
                record_all(ctx, key, &prs, "fixing", &what).await?;
                let comment = format!(
                    "Required checks failed{on}: {}. Fixing them, {attempt}.",
                    failed.join(", ")
                );
                comment_all(gh, repo, &prs, &comment).await?;
                let head = &heads[failing];
                let below = if failing == 0 {
                    base
                } else {
                    &heads[failing - 1]
                };
                let failures = gh.check_failures(repo, head, failed).await?;
                let event = sandbox
                    .fix(key, snap, &now, failing, settings, head, below, &failures)
                    .await?;
                published_at = Instant::now();
                if let Event::Fixed { head } = &event {
                    heads = now.branch_heads(gh, repo).await?;
                    record_all(ctx, key, &prs, "published", head).await?;
                    let comment = format!("Pushed a fix as {head}. Waiting for checks.");
                    comment_all(gh, repo, &prs, &comment).await?;
                }
                event
            }
            Phase::Merging { head } => {
                if let Some(reason) = now.hold_reason() {
                    Event::OnHold(reason)
                } else {
                    let top = now.top().number;
                    start_merge(ctx, key, lane, &now, head).await?;
                    match gh.merge(repo, top, head, &snap.merge_method).await? {
                        MergeOutcome::Merged { sha } => return Ok(Outcome::Merged(sha)),
                        MergeOutcome::Rejected(r) => {
                            lane.merging = None;
                            ctx.store.save_lane(key, lane).await?;
                            Event::MergeRejected(r)
                        }
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

/// The jobs that change a unit's branches, and publish the result to them.
#[async_trait]
trait Sandbox: Send + Sync {
    /// Rebases the unit onto the trunk, a stack layer by layer: `Event::Rebased` with the new
    /// head of the top layer, or why it could not.
    async fn rebase(
        &self,
        key: &LaneKey,
        snap: &RepoSnapshot,
        unit: &Unit,
        settings: &RepoSettings,
    ) -> Result<Event>;

    /// Commits a fix for `failures` on top of `head`, the head of `unit.layers[layer]`, which
    /// sits on `base`. A stack's layers above it are rebased onto the fix. `Event::Fixed` with
    /// the new head of the top layer, or why it could not.
    #[allow(clippy::too_many_arguments)]
    async fn fix(
        &self,
        key: &LaneKey,
        snap: &RepoSnapshot,
        unit: &Unit,
        layer: usize,
        settings: &RepoSettings,
        head: &str,
        base: &str,
        failures: &[CheckFailure],
    ) -> Result<Event>;
}

/// A sandboxed job, whose result is checked, then published through the Git Data API. A layer
/// already on its base needs no job.
struct JobSandbox<'a> {
    ctx: &'a Ctx,
    gh: &'a Installation,
}

impl JobSandbox<'_> {
    /// A job publishes past the cache: the branches and pull refs it may have moved read again.
    async fn published(&self, key: &LaneKey, event: Result<Event>) -> Result<Event> {
        for prefix in ["ref:", "pull:"] {
            let change = Change::ValuesUnder(prefix.into());
            self.ctx
                .store
                .github_changed(key.installation_id, &key.repo, &change)
                .await?;
        }
        event
    }

    #[allow(clippy::too_many_arguments)]
    async fn fix_and_restack(
        &self,
        key: &LaneKey,
        snap: &RepoSnapshot,
        unit: &Unit,
        layer: usize,
        settings: &RepoSettings,
        head: &str,
        base: &str,
        failures: &[CheckFailure],
    ) -> Result<Event> {
        let fixed = fix_and_publish(
            self.ctx, self.gh, key, snap, unit, layer, settings, head, base, failures,
        )
        .await?;
        if !matches!(fixed, Event::Fixed { .. }) || layer + 1 == unit.layers.len() {
            return Ok(fixed);
        }
        // The layers above the fix go onto it.
        Ok(
            match rebase_and_publish(self.ctx, self.gh, key, snap, unit, layer + 1, settings)
                .await?
            {
                Event::Rebased { head, .. } => Event::Fixed { head },
                Event::RebaseFailed(r) => Event::FixFailed(format!("restacking after it: {r}")),
                event => event,
            },
        )
    }
}

#[async_trait]
impl Sandbox for JobSandbox<'_> {
    async fn rebase(
        &self,
        key: &LaneKey,
        snap: &RepoSnapshot,
        unit: &Unit,
        settings: &RepoSettings,
    ) -> Result<Event> {
        let event = rebase_and_publish(self.ctx, self.gh, key, snap, unit, 0, settings).await;
        self.published(key, event).await
    }

    async fn fix(
        &self,
        key: &LaneKey,
        snap: &RepoSnapshot,
        unit: &Unit,
        layer: usize,
        settings: &RepoSettings,
        head: &str,
        base: &str,
        failures: &[CheckFailure],
    ) -> Result<Event> {
        let event = self
            .fix_and_restack(key, snap, unit, layer, settings, head, base, failures)
            .await;
        self.published(key, event).await
    }
}

/// The repo's object cache, with the trunk and each layer's branch fetched: the layers' heads,
/// bottom to top, and the trunk's.
async fn fetch_layers(
    ctx: &Ctx,
    gh: &Installation,
    git: &Git,
    key: &LaneKey,
    layers: &[PullRequest],
) -> Result<(std::path::PathBuf, Vec<String>, String)> {
    let repo = &key.repo;
    let cache = cache_path(&ctx.config.data_dir, &repo.owner, &repo.name);
    git.ensure_cache(&cache, &clone_url(repo)).await?;
    let trunk_ref = format!("refs/remotes/origin/{}", key.trunk);
    let mut refspecs = vec![format!("+refs/heads/{}:{trunk_ref}", key.trunk)];
    refspecs.extend(
        layers
            .iter()
            .map(|pr| format!("+{}:refs/mergepatrol/pr/{}", head_ref(pr), pr.number)),
    );
    let refspecs: Vec<&str> = refspecs.iter().map(String::as_str).collect();
    git.fetch(&cache, &gh.token().await?, &refspecs).await?;
    let mut heads = Vec::with_capacity(layers.len());
    for pr in layers {
        heads.push(
            git.rev_parse(&cache, &format!("refs/mergepatrol/pr/{}", pr.number))
                .await?,
        );
    }
    let trunk = git.rev_parse(&cache, &trunk_ref).await?;
    Ok((cache, heads, trunk))
}

/// The full ref of a PR's head in the repo: its branch, or for a fork PR, its pull ref.
fn head_ref(pr: &PullRequest) -> String {
    match pr.is_cross_repository {
        true => format!("refs/pull/{}/head", pr.number),
        false => format!("refs/heads/{}", pr.head_ref_name),
    }
}

fn clone_url(repo: &RepoName) -> String {
    format!("https://github.com/{repo}.git")
}

/// Where a job on a stack layer starts from: the layer below's branch, which holds what it goes
/// onto, and the commit the layer's own changes start from. `None` for a PR or a stack's bottom.
struct Below<'a> {
    branch: &'a str,
    upstream: &'a str,
}

/// The spec of a job on the PR at `head`, which sits on, or goes onto, `onto`. Its model API
/// token works until the `Granted` is dropped: keep it for as long as the job runs.
#[allow(clippy::too_many_arguments)]
async fn job_spec(
    ctx: &Ctx,
    gh: &Installation,
    key: &LaneKey,
    pr: &PullRequest,
    settings: &RepoSettings,
    head: &str,
    onto: &str,
    below: Option<Below<'_>>,
    task: Task,
) -> Result<(JobSpec, Option<Granted>)> {
    let resolver = &settings.resolver;
    // A fork PR's job gets no agent: without access, a conflict fails the job.
    let granted = match pr.is_cross_repository {
        true => None,
        false => ctx
            .chatgpt
            .credential(
                &ctx.vault,
                Scope::Installation(key.installation_id),
                resolver.provider,
            )
            .await?
            .map(|c| ctx.proxy.grant(resolver.provider, c, REQUESTS_PER_JOB)),
    };
    let spec = JobSpec {
        repo: key.repo.to_string(),
        clone_url: clone_url(&key.repo),
        token: gh.read_token(&key.repo).await?,
        head_ref: head_ref(pr),
        head_sha: head.to_owned(),
        trunk: key.trunk.clone(),
        trunk_sha: onto.to_owned(),
        base_ref: below.as_ref().map(|b| b.branch.to_owned()),
        upstream: below.as_ref().map(|b| b.upstream.to_owned()),
        pr_title: pr.title.clone(),
        pr_body: pr.body.clone(),
        verify: settings.verify.clone(),
        resolver: ResolverSpec {
            provider: resolver.provider,
            model: resolver.model.clone(),
            effort: resolver.effort.clone(),
            timeout_secs: RESOLVER_TIMEOUT.as_secs(),
            max_turns: RESOLVER_MAX_TURNS,
            api: granted.as_ref().map(|g| ApiAccess {
                base_url: Api::of(resolver.provider, &resolver.model)
                    .base_url(&ctx.config.job_proxy_url),
                token: g.token.clone(),
            }),
        },
        task,
    };
    Ok((spec, granted))
}

/// Runs a job in the sandbox, with its output in `out`. `Err` says why it failed.
async fn run_sandboxed(
    ctx: &Ctx,
    key: &LaneKey,
    number: PrNumber,
    job_id: &str,
    spec: &JobSpec,
    out: &Path,
    private: bool,
) -> Result<()> {
    let _ = tokio::fs::remove_dir_all(out).await;
    record(ctx, key, number, "job", job_id).await?;
    if let Some(b) = &ctx.billing {
        b.job(key.installation_id, private, unix_now()).await?;
    }
    let _slot = ctx.jobs.acquire().await?;
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
    unit: &Unit,
    layer: usize,
    settings: &RepoSettings,
    head: &str,
    base: &str,
    failures: &[CheckFailure],
) -> Result<Event> {
    let repo = &key.repo;
    let git = Git;
    let pr = &unit.layers[layer];
    let (cache, fetched, _) = fetch_layers(ctx, gh, &git, key, &unit.layers).await?;
    if fetched[layer] != head {
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
    let below = (layer > 0).then(|| Below {
        branch: &unit.layers[layer - 1].head_ref_name,
        upstream: base,
    });
    let (spec, _granted) = job_spec(ctx, gh, key, pr, settings, head, base, below, task).await?;
    let fixed = async {
        run_sandboxed(ctx, key, pr.number, &job_id, &spec, &out, snap.private).await?;
        let local_head = check_fix_output(&git, &cache, &out, pr.number, head).await?;
        publish_to_pr(gh, &git, &cache, key, snap, pr, head, head, &local_head).await
    };
    Ok(match fixed.await {
        Ok(Ok(sha)) => Event::Fixed { head: sha },
        Ok(Err(event)) => event,
        Err(e) => Event::FixFailed(format!("{e:#}")),
    })
}

/// Rebases the unit's layers from `from` up and publishes them: the bottom onto the trunk, each
/// other onto the layer below it as rebased. A layer already on what it goes onto is left as it
/// is.
///
/// The layers' branches move together, in one atomic update at the end. Until then a rebased
/// layer is on a branch of its own, for the next layer's job to fetch. So a restart, or an
/// author's push, never leaves a layer rebased under one that is not: each layer's own commits
/// still start at the layer below, as it is on GitHub.
async fn rebase_and_publish(
    ctx: &Ctx,
    gh: &Installation,
    key: &LaneKey,
    snap: &RepoSnapshot,
    unit: &Unit,
    from: usize,
    settings: &RepoSettings,
) -> Result<Event> {
    let git = Git;
    let (cache, heads, trunk) = fetch_layers(ctx, gh, &git, key, &unit.layers).await?;
    // A fork's author may have pushed since the queue checked the head against its pin.
    if unit.fork() && heads[0] != unit.top().head_ref_oid {
        return Ok(Event::HeadMoved);
    }
    let mut rebased: Vec<String> = heads[..from].to_vec();
    let mut moved: Vec<usize> = Vec::new();
    let result = rebase_layers(
        ctx,
        gh,
        &git,
        &cache,
        key,
        snap,
        unit,
        settings,
        &heads,
        &trunk,
        &mut rebased,
        &mut moved,
    )
    .await;
    for i in &moved {
        gh.delete_branch(&key.repo, &restack_branch(&unit.layers[*i]))
            .await?;
    }
    if let Err(e) = result {
        let failed = rebased.len();
        return Ok(Event::RebaseFailed(match (unit.stacked, unit.fork()) {
            (true, _) => format!("#{}: {e:#}", unit.layers[failed].number),
            (false, true) => format!(
                "{e:#}. Mergepatrol runs no agent on a PR from a fork, so rebase it onto `{}` yourself",
                key.trunk
            ),
            (false, false) => format!("{e:#}"),
        }));
    }
    if unit.fork() {
        let pr = unit.top();
        if moved.is_empty() {
            return Ok(Event::Rebased {
                head: heads[0].clone(),
                base: trunk,
            });
        }
        return publish_to_fork(gh, &git, &cache, key, pr, &heads[0], &rebased[0], trunk).await;
    }

    // Every layer, so that the update also fails if an author pushed to one left as it is: a
    // layer's update to the head it has is its lease.
    let moves: Vec<(&str, &str, &str)> = unit
        .layers
        .iter()
        .zip(heads.iter().zip(&rebased))
        .map(|(pr, (before, after))| (pr.head_ref_name.as_str(), before.as_str(), after.as_str()))
        .collect();
    if !moved.is_empty()
        && let Err(e) = gh.force_update_branches(&snap.id, &moves).await
    {
        // `updateRefs` refuses when a branch moved: an author pushed while the jobs ran.
        if unit.branch_heads(gh, &key.repo).await? != heads {
            return Ok(Event::HeadMoved);
        }
        bail!("publishing failed: {e:#}");
    }
    Ok(Event::Rebased {
        head: rebased.pop().expect("a unit has a PR"),
        base: trunk,
    })
}

/// Moves fork PR `pr`'s branch from `before` to `after`, a commit created in this repo: GitHub
/// lets the App push to a fork whose author allows edits by maintainers. The commit reaches the
/// cache through a ref outside `refs/heads/`, on which no workflow runs the fork's code with this
/// repo's secrets.
#[allow(clippy::too_many_arguments)]
async fn publish_to_fork(
    gh: &Installation,
    git: &Git,
    cache: &Path,
    key: &LaneKey,
    pr: &PullRequest,
    before: &str,
    after: &str,
    trunk: String,
) -> Result<Event> {
    let repo = &key.repo;
    let fork = pr.head_repository.as_ref().context("the fork is gone")?;
    let name = format!("refs/mergepatrol/publish/{}", pr.number);
    gh.delete_ref(repo, &name).await?;
    gh.create_ref(repo, &name, after).await?;
    let fetched = git
        .fetch(cache, &gh.token().await?, &[&format!("+{name}:{name}")])
        .await;
    gh.delete_ref(repo, &name).await?;
    fetched?;
    let url = format!("https://github.com/{}.git", fork.name_with_owner);
    let pushed = git
        .push_leased(
            cache,
            &gh.token().await?,
            &url,
            after,
            &pr.head_ref_name,
            before,
        )
        .await;
    if let Err(e) = pushed {
        if gh.pull_head(repo, pr.number).await? != before {
            return Ok(Event::HeadMoved);
        }
        bail!("publishing to the fork failed: {e:#}");
    }
    // The checks and the merge go by the pull ref, which follows the fork's branch moments later.
    let since = Instant::now();
    while gh.pull_head(repo, pr.number).await? != after {
        if since.elapsed() > PULL_REF_WAIT {
            bail!("the PR did not show the push to its fork");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Ok(Event::Rebased {
        head: after.to_owned(),
        base: trunk,
    })
}

/// The branch a rebased stack layer waits on, until the layers' branches move.
fn restack_branch(pr: &PullRequest) -> String {
    format!("mergepatrol/restack-{}", pr.number)
}

/// Rebases each layer from `rebased.len()` up, and creates its commits on GitHub: pushes its new
/// head to `rebased`, and its index to `moved` when it changed.
#[allow(clippy::too_many_arguments)]
async fn rebase_layers(
    ctx: &Ctx,
    gh: &Installation,
    git: &Git,
    cache: &Path,
    key: &LaneKey,
    snap: &RepoSnapshot,
    unit: &Unit,
    settings: &RepoSettings,
    heads: &[String],
    trunk: &str,
    rebased: &mut Vec<String>,
    moved: &mut Vec<usize>,
) -> Result<()> {
    let repo = &key.repo;
    for i in rebased.len()..unit.layers.len() {
        let pr = &unit.layers[i];
        let head = &heads[i];
        let onto = match i {
            0 => trunk.to_owned(),
            _ => rebased[i - 1].clone(),
        };
        if git.is_ancestor(cache, &onto, head).await? {
            rebased.push(head.clone());
            continue;
        }
        // The layer's own commits start at the layer below, as it is on GitHub. What they go
        // onto is that layer rebased, on its own branch, or as it is.
        let below_branch = match i {
            0 => None,
            _ if moved.contains(&(i - 1)) => Some(restack_branch(&unit.layers[i - 1])),
            _ => Some(unit.layers[i - 1].head_ref_name.clone()),
        };
        let below = below_branch.as_deref().map(|branch| Below {
            branch,
            upstream: &heads[i - 1],
        });
        let upstream = below.as_ref().map(|b| b.upstream.to_owned());
        let job_id = format!("{}-{}-{}-{}", repo.owner, repo.name, pr.number, &head[..8]);
        let out = ctx.config.data_dir.join("jobs").join(&job_id);
        let (spec, _granted) =
            job_spec(ctx, gh, key, pr, settings, head, &onto, below, Task::Rebase).await?;
        run_sandboxed(ctx, key, pr.number, &job_id, &spec, &out, snap.private).await?;
        let local_head = check_job_output(
            git,
            cache,
            &out,
            pr.number,
            &onto,
            head,
            upstream.as_deref(),
        )
        .await?;
        let publish = Publish {
            repo,
            repo_id: &snap.id,
            branch: &pr.head_ref_name,
            expected_head: head,
            base: &onto,
            local_head: &local_head,
        };
        let sha = create_commits(gh, git, cache, &publish).await?;
        rebased.push(sha.clone());
        moved.push(i);
        if i + 1 < unit.layers.len() {
            // For the next layer's job, and for this cache, which the next layer's output
            // must sit on.
            let branch = restack_branch(pr);
            gh.delete_branch(repo, &branch).await?;
            gh.create_branch(repo, &branch, &sha).await?;
            let refspec = format!("+refs/heads/{branch}:refs/mergepatrol/{branch}");
            git.fetch(cache, &gh.token().await?, &[&refspec]).await?;
        }
    }
    Ok(())
}

/// The job is untrusted, so its output is checked here, not believed. A rebase only replays
/// the PR's changes, and a conflict can only be in a file the PR changed: the result may touch
/// no file outside the PR's own. A stack layer's own changes start at `upstream`, the layer
/// below as it was.
async fn check_job_output(
    git: &Git,
    cache: &Path,
    out: &Path,
    number: PrNumber,
    onto: &str,
    head: &str,
    upstream: Option<&str>,
) -> Result<String> {
    let local_head = import_job_output(git, cache, out, number).await?;
    if !git.is_ancestor(cache, onto, &local_head).await? {
        bail!("the job's result does not sit on the base branch");
    }
    let start = match upstream {
        Some(upstream) => upstream.to_owned(),
        None => git.merge_base(cache, onto, head).await?,
    };
    let pr_files: BTreeSet<String> = git
        .changed_files(cache, &start, head)
        .await?
        .into_iter()
        .collect();
    let outside: Vec<String> = git
        .changed_files(cache, onto, &local_head)
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
        check_job_output(&Git, &cache, &out, 1, &trunk, &head, None).await
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

    #[tokio::test]
    async fn only_old_job_outputs_are_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let jobs = tmp.path().join("jobs");
        for job in ["old", "new"] {
            std::fs::create_dir_all(jobs.join(job)).unwrap();
            std::fs::write(jobs.join(job).join("transcript.jsonl"), "{}").unwrap();
        }
        // Eight days ago, by its modified time.
        let old = std::fs::File::open(jobs.join("old")).unwrap();
        let then = std::time::SystemTime::now() - Duration::from_secs(8 * 24 * 60 * 60);
        old.set_modified(then).unwrap();
        assert_eq!(prune_job_outputs(&jobs, JOB_OUTPUTS_FOR).await.unwrap(), 1);
        assert!(!jobs.join("old").exists());
        assert!(jobs.join("new/transcript.jsonl").exists());
        // No job yet: nothing to do.
        assert_eq!(
            prune_job_outputs(&tmp.path().join("none"), JOB_OUTPUTS_FOR)
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn a_task_keeps_the_channel_open_until_it_ends() {
        let (alive, mut stopped) = mpsc::channel::<()>(1);
        let (end, ended) = tokio::sync::oneshot::channel::<()>();
        let _task = Spawned::spawn(
            async move {
                let _ = ended.await;
            },
            &alive,
        );
        drop(alive);
        tokio::task::yield_now().await;
        assert!(matches!(
            stopped.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        end.send(()).unwrap();
        assert!(stopped.recv().await.is_none());
    }

    #[tokio::test]
    async fn stopping_a_task_stops_the_tasks_it_runs() {
        let (alive, mut stopped) = mpsc::channel::<()>(1);
        let child_alive = alive.clone();
        let parent = Spawned::spawn(
            async move {
                let _child = Spawned::spawn(std::future::pending(), &child_alive);
                std::future::pending::<()>().await;
            },
            &alive,
        );
        drop(alive);
        tokio::task::yield_now().await;
        drop(parent);
        let ended = tokio::time::timeout(Duration::from_secs(1), stopped.recv()).await;
        assert!(ended.is_ok(), "the child task still runs");
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
