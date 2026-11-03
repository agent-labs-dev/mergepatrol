//! Getting a PR ready before it takes the lane: its conflicts resolved, its checks green, its
//! failures fixed, each on its own, so that no PR holds up another. A tick reads where a PR is
//! from GitHub and starts what it needs, and nothing here waits. What this cannot settle goes on
//! to the lane, which ejects as it does for any PR. Stacks and forks go straight there.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Result;
use mergepatrol_core::{CHECKS_TIMEOUT, Event, Hold, PrNumber, RepoName, RepoSettings};
use mergepatrol_executor::protocol::Task;
use mergepatrol_github::{CheckState, Installation, RepoSnapshot};
use mergepatrol_store::LaneKey;
use tokio::task::JoinSet;
use tokio::time::Instant;

use super::{Ctx, JobSandbox, Unit, fix_and_publish, rebase_and_publish, record};
use crate::github::GitHub;

/// How soon a job the tick asks for starts, also while the lane's job runs.
const DRAIN_EVERY: Duration = Duration::from_secs(2);

type PrId = (u64, RepoName, PrNumber);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Work {
    Rebase,
    Fix { failed: Vec<String> },
}

/// A job the tick asked for.
pub(super) struct Wanted {
    pub key: LaneKey,
    pub unit: Unit,
    pub snap: RepoSnapshot,
    pub settings: RepoSettings,
    pub work: Work,
}

/// What of it lives only in memory: a restart reads it all from GitHub again.
#[derive(Default)]
pub struct Prep {
    wanted: Mutex<Vec<Wanted>>,
    /// The PRs with a job asked for or running, and what that holds them for.
    running: Mutex<HashMap<PrId, Hold>>,
    /// The head of each PR left to the lane: its job could not settle it.
    left: Mutex<HashMap<PrId, String>>,
    /// Each PR's head that its checks hold, and since when.
    since: Mutex<HashMap<PrId, (String, Instant)>>,
}

impl Prep {
    fn holding(&self, id: &PrId) -> Option<Hold> {
        self.running.lock().unwrap().get(id).cloned()
    }

    fn left_to_lane(&self, id: &PrId, head: &str) -> bool {
        self.left.lock().unwrap().get(id).is_some_and(|h| h == head)
    }

    fn want(&self, id: PrId, hold: Hold, wanted: Wanted) -> Hold {
        self.running.lock().unwrap().insert(id, hold.clone());
        self.wanted.lock().unwrap().push(wanted);
        hold
    }

    /// The jobs asked for on `repo`.
    fn take(&self, repo: &RepoName) -> Vec<Wanted> {
        let mut wanted = self.wanted.lock().unwrap();
        let (mine, rest) = std::mem::take(&mut *wanted)
            .into_iter()
            .partition(|w| w.key.repo == *repo);
        *wanted = rest;
        mine
    }

    fn waited(&self, id: &PrId, head: &str) -> Duration {
        let mut since = self.since.lock().unwrap();
        let entry = since
            .entry(id.clone())
            .or_insert_with(|| (head.to_owned(), Instant::now()));
        if entry.0 != head {
            *entry = (head.to_owned(), Instant::now());
        }
        entry.1.elapsed()
    }

    /// The PR's job ended. `left`: the head it could not settle.
    fn finish(&self, id: &PrId, left: Option<String>) {
        self.running.lock().unwrap().remove(id);
        if let Some(head) = left {
            self.left.lock().unwrap().insert(id.clone(), head);
        }
    }

    /// What was asked for, for tests of the tick.
    #[cfg(test)]
    pub(super) fn asked(&self) -> Vec<(PrNumber, Work)> {
        let wanted = self.wanted.lock().unwrap();
        wanted
            .iter()
            .map(|w| (w.unit.number, w.work.clone()))
            .collect()
    }

    #[cfg(test)]
    pub(super) fn leave_to_lane(&self, id: PrId, head: &str) {
        self.left.lock().unwrap().insert(id, head.to_owned());
    }
}

/// Why `unit`, which is no stack or fork, waits off the lane, starting what it needs. `None`: it
/// goes on to the review.
pub(super) async fn hold(
    ctx: &Ctx,
    gh: &dyn GitHub,
    key: &LaneKey,
    snap: &RepoSnapshot,
    settings: &RepoSettings,
    unit: &Unit,
) -> Result<Option<Hold>> {
    if !ctx.config.prep || unit.stacked || unit.fork() {
        return Ok(None);
    }
    let pr = unit.top();
    let head = pr.head_ref_oid.as_str();
    let id = (key.installation_id, key.repo.clone(), unit.number);
    if let Some(hold) = ctx.prep.holding(&id) {
        return Ok(Some(hold));
    }
    if ctx.prep.left_to_lane(&id, head) {
        return Ok(None);
    }
    let want = |work| Wanted {
        key: key.clone(),
        unit: unit.clone(),
        snap: snap.clone(),
        settings: settings.clone(),
        work,
    };
    if pr.conflicting {
        let wanted = want(Work::Rebase);
        return Ok(Some(ctx.prep.want(id, Hold::PrepRebasing, wanted)));
    }
    let required = gh.branch_rules(&key.repo, &key.trunk).await?.checks;
    let checks = gh.checks(&key.repo, head).await?;
    let (state, names) = checks.verdict(&required);
    Ok(match state {
        CheckState::Passed => None,
        CheckState::Failed => {
            let used = gh.fixes_used(&key.repo, pr.number).await?;
            (used < settings.fix_ci.allowed()).then(|| {
                let wanted = want(Work::Fix { failed: names });
                ctx.prep.want(id, Hold::PrepFixing, wanted)
            })
        }
        CheckState::Pending => {
            // A check that never reports, or never ends, is the lane's to eject.
            let silent = !names
                .iter()
                .any(|n| checks.by_name.get(n) == Some(&CheckState::Pending));
            let running = match checks.workflows_running {
                Some(running) => running,
                None => silent && gh.workflows_running(&key.repo, head).await?,
            };
            let holds = (!silent || running) && ctx.prep.waited(&id, head) < CHECKS_TIMEOUT;
            holds.then_some(Hold::PrepChecks)
        }
    })
}

/// Starts what the ticks of `repo` ask for, until the queue stops, which stops the jobs too.
pub(super) async fn drain(ctx: std::sync::Arc<Ctx>, gh: Installation, repo: RepoName) {
    let mut jobs = JoinSet::new();
    loop {
        for wanted in ctx.prep.take(&repo) {
            let (ctx, gh) = (ctx.clone(), gh.clone());
            jobs.spawn(async move { run(&ctx, &gh, wanted).await });
        }
        while jobs.try_join_next().is_some() {}
        tokio::time::sleep(DRAIN_EVERY).await;
    }
}

async fn run(ctx: &Ctx, gh: &Installation, w: Wanted) {
    let key = &w.key;
    let number = w.unit.number;
    let id = (key.installation_id, key.repo.clone(), number);
    let head = w.unit.top().head_ref_oid.clone();
    let event = job(ctx, gh, &w).await;
    let why = match event {
        Ok(Event::Rebased { head, .. } | Event::Fixed { head }) => {
            ok(record(ctx, key, number, "published", &head).await);
            None
        }
        // The author pushed: the next tick reads the new head.
        Ok(Event::HeadMoved) => None,
        Ok(Event::RebaseFailed(why) | Event::FixFailed(why) | Event::FixGaveUp(why)) => Some(why),
        Ok(event) => Some(format!("{event:?}")),
        Err(e) => Some(format!("{e:#}")),
    };
    if let Some(why) = &why {
        ok(record(ctx, key, number, "prep-left", why).await);
    }
    ctx.prep.finish(&id, why.map(|_| head));
    ctx.wakers.wake_repo(key.installation_id, &key.repo);
}

fn ok(recorded: Result<()>) {
    if let Err(e) = recorded {
        tracing::warn!(error = %format!("{e:#}"), "could not record what getting a PR ready did");
    }
}

async fn job(ctx: &Ctx, gh: &Installation, w: &Wanted) -> Result<Event> {
    let (key, unit, snap, settings) = (&w.key, &w.unit, &w.snap, &w.settings);
    let pr = unit.top();
    // It may have left the queue, or moved, since the tick asked: no agent for that.
    let now = GitHub::pull_request(gh, &key.repo, pr.number).await?;
    if !now.is_open() || !now.has_label(&settings.label) || now.head_ref_oid != pr.head_ref_oid {
        return Ok(Event::HeadMoved);
    }
    let event = match &w.work {
        Work::Rebase => {
            let onto = format!("onto {}", key.trunk);
            record(ctx, key, pr.number, "rebasing", &onto).await?;
            rebase_and_publish(ctx, gh, key, snap, unit, 0, settings).await
        }
        Work::Fix { failed } => {
            let head = &pr.head_ref_oid;
            let used = GitHub::fixes_used(gh, &key.repo, pr.number).await?;
            let allowed = settings.fix_ci.allowed();
            if used >= allowed {
                return Ok(Event::FixFailed("no fixes left".into()));
            }
            let what = format!("{}, attempt {} of {allowed}", failed.join(", "), used + 1);
            record(ctx, key, pr.number, "fixing", &what).await?;
            let failures = GitHub::check_failures(gh, &key.repo, head, failed).await?;
            let trunk = gh.branch_sha(&key.repo, &key.trunk).await?;
            let task = Task::FixChecks { failures };
            fix_and_publish(ctx, gh, key, snap, unit, 0, settings, head, &trunk, task).await
        }
    };
    JobSandbox { ctx, gh }.published(key, event).await
}
