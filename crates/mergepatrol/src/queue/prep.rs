//! Getting a PR ready before it takes the lane: its conflicts resolved, its checks green, its
//! failures fixed, each on its own, so that no PR holds up another. A tick reads where a PR is
//! from GitHub and starts what it needs, and nothing here waits. What this cannot settle goes on
//! to the lane, which ejects as it does for any PR. A stack gets ready as one, and its fix goes on
//! the layer that failed. A fork goes straight to the lane: no agent runs on it, and as it never
//! joins a batch, the lane rebases it and runs its checks again whatever they said here.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use mergepatrol_core::{CHECKS_TIMEOUT, Event, Hold, PrNumber, RepoName, RepoSettings};
use mergepatrol_executor::protocol::Task;
use mergepatrol_github::{CheckState, Installation, RepoSnapshot};
use mergepatrol_store::LaneKey;
use tokio::task::JoinSet;
use tokio::time::Instant;

use super::{Ctx, JobSandbox, Unit, rebase_and_publish, record_all, rerun_failed, still_queued};
use crate::github::GitHub;

/// A job's hold on a PR, which ends with the job, also if it panics or is aborted, and which
/// leaves a later job's hold alone.
pub struct Claim {
    running: Running,
    id: PrId,
    token: u64,
}

impl Drop for Claim {
    fn drop(&mut self) {
        let mut running = self.running.lock().unwrap();
        if running.get(&self.id).is_some_and(|(t, _)| *t == self.token) {
            running.remove(&self.id);
        }
    }
}

/// How soon a job the tick asks for starts, also while the lane's job runs.
const DRAIN_EVERY: Duration = Duration::from_secs(2);

type PrId = (u64, RepoName, PrNumber);
/// Shared, so a claim can outlive the borrow of `Prep`, as a job's task does.
type Running = Arc<Mutex<HashMap<PrId, (u64, Hold)>>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Work {
    Rebase,
    /// GitHub Actions runs the failed jobs of the workflow runs `runs` again, once, for the
    /// checks `failed` on the layer `layer`.
    Rerun {
        layer: usize,
        failed: Vec<String>,
        runs: Vec<u64>,
    },
    /// `layer`: the index in the unit of the lowest layer that failed.
    Fix {
        layer: usize,
        failed: Vec<String>,
    },
}

/// A job the tick asked for.
pub(super) struct Wanted {
    pub key: LaneKey,
    pub unit: Unit,
    pub snap: RepoSnapshot,
    pub settings: RepoSettings,
    pub work: Work,
    /// Its claim on the PR.
    token: u64,
}

/// What of it lives only in memory: a restart reads it all from GitHub again.
#[derive(Default)]
pub struct Prep {
    wanted: Mutex<Vec<Wanted>>,
    /// The PRs with a job asked for or running: its claim, and what that holds them for.
    running: Running,
    next_claim: AtomicU64,
    /// The head of each PR left to the lane: its job could not settle it.
    left: Mutex<HashMap<PrId, String>>,
    /// The head of each PR the lane found a conflict on, which GitHub may not say yet.
    conflicts: Mutex<HashMap<PrId, String>>,
    /// Each PR's head that its checks hold, and since when.
    since: Mutex<HashMap<PrId, (String, Instant)>>,
    /// The commits of each PR whose failed jobs GitHub would not run again.
    refused_reruns: Mutex<HashMap<PrId, BTreeSet<String>>>,
}

impl Prep {
    fn holding(&self, id: &PrId) -> Option<Hold> {
        let running = self.running.lock().unwrap();
        running.get(id).map(|(_, hold)| hold.clone())
    }

    fn left_to_lane(&self, id: &PrId, head: &str) -> bool {
        self.left.lock().unwrap().get(id).is_some_and(|h| h == head)
    }

    /// Whether what `unit` needs of an agent is for here to do, and not its job in the lane.
    pub(super) fn takes_agent_work(&self, ctx: &Ctx, key: &LaneKey, unit: &Unit) -> bool {
        let id = (key.installation_id, key.repo.clone(), unit.number);
        ctx.config.prep && !unit.fork() && !self.left_to_lane(&id, &unit.top().head_ref_oid)
    }

    /// GitHub would not run the failed jobs of the commit `sha` of PR `number` again: its
    /// checks' failure stands.
    pub(super) fn note_refused_rerun(&self, key: &LaneKey, number: PrNumber, sha: &str) {
        let id = (key.installation_id, key.repo.clone(), number);
        let mut refused = self.refused_reruns.lock().unwrap();
        refused.entry(id).or_default().insert(sha.to_owned());
    }

    /// Whether GitHub would not run the failed jobs of `sha` again.
    pub(super) fn rerun_refused(&self, key: &LaneKey, number: PrNumber, sha: &str) -> bool {
        let id = (key.installation_id, key.repo.clone(), number);
        let refused = self.refused_reruns.lock().unwrap();
        refused.get(&id).is_some_and(|shas| shas.contains(sha))
    }

    /// The lane found a conflict on the PR's `head`.
    pub(super) fn conflicted(&self, key: &LaneKey, number: PrNumber, head: &str) {
        let id = (key.installation_id, key.repo.clone(), number);
        self.conflicts.lock().unwrap().insert(id, head.to_owned());
    }

    /// Whether the lane found a conflict on `head`, which it asks only once.
    fn take_conflict(&self, id: &PrId, head: &str) -> bool {
        let mut conflicts = self.conflicts.lock().unwrap();
        conflicts.remove_entry(id).is_some_and(|(_, h)| h == head)
    }

    /// Asks for `wanted`, or, if a job has the PR since the tick looked, holds it for that one.
    fn want(&self, id: PrId, hold: Hold, mut wanted: Wanted) -> Hold {
        let mut running = self.running.lock().unwrap();
        if let Some((_, theirs)) = running.get(&id) {
            return theirs.clone();
        }
        wanted.token = self.next_claim.fetch_add(1, Ordering::Relaxed);
        running.insert(id, (wanted.token, hold.clone()));
        self.wanted.lock().unwrap().push(wanted);
        hold
    }

    /// The claim a job the tick asked for holds.
    fn claim_of(&self, w: &Wanted) -> Claim {
        Claim {
            running: self.running.clone(),
            id: (w.key.installation_id, w.key.repo.clone(), w.unit.number),
            token: w.token,
        }
    }

    /// Holds the unit `number` as a job would, until the claim drops: `None` if a job has it.
    #[cfg(test)]
    pub(crate) fn claim(&self, key: &LaneKey, number: PrNumber) -> Option<Claim> {
        let id = (key.installation_id, key.repo.clone(), number);
        let mut running = self.running.lock().unwrap();
        if running.contains_key(&id) {
            return None;
        }
        let token = self.next_claim.fetch_add(1, Ordering::Relaxed);
        running.insert(id.clone(), (token, Hold::PrepFixing));
        Some(Claim {
            running: self.running.clone(),
            id,
            token,
        })
    }

    /// A queue starts on `repo`: the jobs its last run asked for and never started are gone. One
    /// that started ended with that queue, and took its claim along.
    fn reset(&self, repo: &RepoName) {
        let mut running = self.running.lock().unwrap();
        let mut wanted = self.wanted.lock().unwrap();
        wanted.retain(|w| {
            if w.key.repo != *repo {
                return true;
            }
            running.retain(|_, (token, _)| *token != w.token);
            false
        });
    }

    /// The jobs asked for on `repo`.
    pub(super) fn take(&self, repo: &RepoName) -> Vec<Wanted> {
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

    /// The PR's job ended, for its claim to stop holding it. `left`: the head it could not settle.
    pub(super) fn finish(&self, id: &PrId, left: Option<String>) {
        if let Some(head) = left {
            self.left.lock().unwrap().insert(id.clone(), head);
        }
    }

    /// As the claim of a job does as it ends.
    #[cfg(test)]
    pub(super) fn end_job(&self, id: &PrId) {
        self.running.lock().unwrap().remove(id);
    }

    /// Takes the jobs asked for on `repo`, as `drain` does, and aborts each before it runs.
    #[cfg(test)]
    pub(super) async fn abort_wanted(&self, repo: &RepoName) {
        let mut jobs = JoinSet::new();
        for wanted in self.take(repo) {
            let claim = self.claim_of(&wanted);
            jobs.spawn(async move { drop((wanted, claim)) });
        }
        jobs.abort_all();
        while jobs.join_next().await.is_some() {}
    }

    #[cfg(test)]
    pub(super) fn start_over(&self, repo: &RepoName) {
        self.reset(repo);
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

/// Why `unit`, unless it is a fork, waits off the lane, starting what it needs. `None`: it goes on
/// to the review.
pub(super) async fn hold(
    ctx: &Ctx,
    gh: &dyn GitHub,
    key: &LaneKey,
    snap: &RepoSnapshot,
    settings: &RepoSettings,
    unit: &Unit,
) -> Result<Option<Hold>> {
    if !ctx.config.prep || unit.fork() {
        return Ok(None);
    }
    let head = unit.top().head_ref_oid.as_str();
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
        token: 0,
    };
    if ctx.prep.take_conflict(&id, head) || unit.layers.iter().any(|pr| pr.conflicting) {
        let wanted = want(Work::Rebase);
        return Ok(Some(ctx.prep.want(id, Hold::PrepRebasing, wanted)));
    }
    let required = gh.branch_rules(&key.repo, &key.trunk).await?.checks;
    // Bottom to top: a fix moves the layers above it.
    let mut waiting = false;
    for (layer, pr) in unit.layers.iter().enumerate() {
        let sha = pr.head_ref_oid.as_str();
        let checks = gh.checks(&key.repo, sha).await?;
        let (state, names) = checks.verdict(&required);
        match state {
            CheckState::Passed => {}
            CheckState::Failed => {
                let runs: BTreeSet<u64> = names
                    .iter()
                    .filter_map(|n| checks.reruns.get(n).copied())
                    .collect();
                if !runs.is_empty() && !ctx.prep.rerun_refused(key, unit.number, sha) {
                    let runs = runs.into_iter().collect();
                    let failed = names;
                    let wanted = want(Work::Rerun {
                        layer,
                        failed,
                        runs,
                    });
                    return Ok(Some(ctx.prep.want(id, Hold::PrepChecks, wanted)));
                }
                let used = unit.fixes_used(gh, &key.repo).await?;
                return Ok((used < settings.fix_ci.allowed()).then(|| {
                    let failed = names;
                    let wanted = want(Work::Fix { layer, failed });
                    ctx.prep.want(id, Hold::PrepFixing, wanted)
                }));
            }
            CheckState::Pending => {
                // A check that never reports, or never ends, is the lane's to eject.
                let silent = !names
                    .iter()
                    .any(|n| checks.by_name.get(n) == Some(&CheckState::Pending));
                let running = match checks.workflows_running {
                    Some(running) => running,
                    None => silent && gh.workflows_running(&key.repo, sha).await?,
                };
                waiting |= (!silent || running) && ctx.prep.waited(&id, head) < CHECKS_TIMEOUT;
            }
        }
    }
    Ok(waiting.then_some(Hold::PrepChecks))
}

/// Starts what the ticks of `repo` ask for, until the queue stops, which stops the jobs too.
pub(super) async fn drain(ctx: std::sync::Arc<Ctx>, gh: Installation, repo: RepoName) {
    ctx.prep.reset(&repo);
    let mut jobs = JoinSet::new();
    loop {
        for wanted in ctx.prep.take(&repo) {
            let (ctx, gh) = (ctx.clone(), gh.clone());
            // Taken here: a task aborted before it first runs drops only what it captured.
            let claim = ctx.prep.claim_of(&wanted);
            jobs.spawn(async move { run(&ctx, &gh, wanted, claim).await });
        }
        while let Some(ended) = jobs.try_join_next() {
            if let Err(e) = ended {
                tracing::error!(%repo, error = %e, "a job off the lane broke");
            }
        }
        tokio::time::sleep(DRAIN_EVERY).await;
    }
}

async fn run(ctx: &Ctx, gh: &Installation, w: Wanted, claim: Claim) {
    let key = &w.key;
    let prs = w.unit.prs();
    let id = (key.installation_id, key.repo.clone(), w.unit.number);
    let head = w.unit.top().head_ref_oid.clone();
    let event = job(ctx, gh, &w).await;
    let why = match event {
        Ok(Event::Rebased { head, .. } | Event::Fixed { head }) => {
            ok(record_all(ctx, key, &prs, "published", &head).await);
            None
        }
        // The author pushed: the next tick reads the new head.
        Ok(Event::HeadMoved) => None,
        // The next tick reads how the checks ran again.
        Ok(Event::ChecksRerun) => None,
        Ok(Event::RerunFailed(why)) => {
            if let Work::Rerun { layer, .. } = &w.work {
                let sha = &w.unit.layers[*layer].head_ref_oid;
                ctx.prep.note_refused_rerun(key, w.unit.number, sha);
            }
            ok(record_all(ctx, key, &prs, "rerun-failed", &why).await);
            None
        }
        Ok(Event::RebaseFailed(why) | Event::FixFailed(why) | Event::FixGaveUp(why)) => Some(why),
        // A check reads the new title only on a new run: the lane's recheck, which it guards.
        Ok(Event::Retitled) => Some("the agent set a new title, for the lane to recheck".into()),
        Ok(event) => Some(format!("{event:?}")),
        Err(e) => Some(format!("{e:#}")),
    };
    if let Some(why) = &why {
        ok(record_all(ctx, key, &prs, "prep-left", why).await);
    }
    ctx.prep.finish(&id, why.map(|_| head));
    drop(claim);
    ctx.wakers.wake_repo(key.installation_id, &key.repo);
}

fn ok(recorded: Result<()>) {
    if let Err(e) = recorded {
        tracing::warn!(error = %format!("{e:#}"), "could not record what getting a PR ready did");
    }
}

async fn job(ctx: &Ctx, gh: &Installation, w: &Wanted) -> Result<Event> {
    let (key, unit, snap, settings) = (&w.key, &w.unit, &w.snap, &w.settings);
    let prs = unit.prs();
    let heads: Vec<String> = unit
        .layers
        .iter()
        .map(|pr| pr.head_ref_oid.clone())
        .collect();
    // It may have left the queue, or moved, since the tick asked: no agent for that.
    if !still_queued(gh, key, settings, unit, &heads).await? {
        return Ok(Event::HeadMoved);
    }
    let sandbox = JobSandbox { ctx, gh };
    let event = match &w.work {
        Work::Rerun {
            layer,
            failed,
            runs,
        } => {
            record_all(ctx, key, &prs, "rerunning", &failed.join(", ")).await?;
            let sha = &unit.layers[*layer].head_ref_oid;
            return rerun_failed(gh, &key.repo, sha, runs).await;
        }
        Work::Rebase => {
            let onto = format!("onto {}", key.trunk);
            record_all(ctx, key, &prs, "rebasing", &onto).await?;
            rebase_and_publish(ctx, gh, key, snap, unit, 0, settings, true, true).await
        }
        Work::Fix { layer, failed } => {
            let pr = &unit.layers[*layer];
            let head = &pr.head_ref_oid;
            let used = unit.fixes_used(gh, &key.repo).await?;
            let allowed = settings.fix_ci.allowed();
            if used >= allowed {
                return Ok(Event::FixFailed("no fixes left".into()));
            }
            let on = match unit.stacked {
                true => format!(" on #{}", pr.number),
                false => String::new(),
            };
            let what = format!(
                "{}{on}, attempt {} of {allowed}",
                failed.join(", "),
                used + 1
            );
            record_all(ctx, key, &prs, "fixing", &what).await?;
            let failures = GitHub::check_failures(gh, &key.repo, head, failed).await?;
            let base = unit.below(gh, key, *layer).await?;
            let task = Task::FixChecks { failures };
            sandbox
                .fix_and_restack(key, snap, unit, *layer, settings, head, &base, task, true)
                .await
        }
    };
    sandbox.published(key, event).await
}
