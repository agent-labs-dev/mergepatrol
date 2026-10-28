//! Drives one repo: polls GitHub, keeps the lane in sync, and runs the PR at the head of the lane
//! through rebase, checks, and merge.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use mergepatrol_core::{
    Candidate, EjectReason, Event, Job, Lane, LaneChange, Phase, PrNumber, RepoName, Transition,
};
use mergepatrol_executor::Executor;
use mergepatrol_git::{Git, RebaseStep, cache_path};
use mergepatrol_github::{
    App, AppCredentials, CheckState, Installation, MergeOutcome, PullRequest, RepoSnapshot,
};
use mergepatrol_resolver::{Resolution, ResolveRequest, Resolver};
use mergepatrol_store::{GITHUB_APP, LaneKey, Store, Vault};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

use crate::config::{Config, REPO_CONFIG_PATH, RepoConfig};
use crate::publish::{Publish, publish};

const CHECKS_POLL: Duration = Duration::from_secs(15);
/// With no required checks, a commit that has no checks at all after this long counts as green.
const NO_CHECKS_GRACE: Duration = Duration::from_secs(120);
/// How soon a repo turned on in the dashboard starts its queue.
const SUPERVISE_EVERY: Duration = Duration::from_secs(10);
const VERIFY_TIMEOUT: Duration = Duration::from_secs(600);

pub struct Ctx {
    pub config: Config,
    pub vault: Arc<Vault>,
    pub store: Arc<dyn Store>,
    pub exec: Arc<dyn Executor>,
    pub resolver: Arc<dyn Resolver>,
    pub jobs: Semaphore,
    pub resolutions: Semaphore,
}

/// Runs a queue for each repo that is on in the dashboard, and stops the queues of repos that
/// are turned off. Waits until the GitHub App exists.
pub async fn supervise(ctx: Arc<Ctx>) {
    let mut running: HashMap<RepoName, (u64, JoinHandle<()>)> = HashMap::new();
    loop {
        if let Err(e) = reconcile(&ctx, &mut running).await {
            tracing::error!(error = %format!("{e:#}"), "could not update the running queues");
        }
        tokio::time::sleep(SUPERVISE_EVERY).await;
    }
}

async fn reconcile(
    ctx: &Arc<Ctx>,
    running: &mut HashMap<RepoName, (u64, JoinHandle<()>)>,
) -> Result<()> {
    let Some(json) = ctx.vault.get(GITHUB_APP).await? else {
        return Ok(());
    };
    let creds: AppCredentials = serde_json::from_str(&json)?;
    let wanted: HashMap<RepoName, ()> = ctx
        .store
        .repo_settings()
        .await?
        .into_iter()
        .filter(|s| s.enabled)
        .map(|s| (s.repo, ()))
        .collect();

    running.retain(|repo, (app_id, task)| {
        // A new App (set up again) means new credentials: restart.
        let keep = wanted.contains_key(repo) && *app_id == creds.id && !task.is_finished();
        if !keep {
            tracing::info!(%repo, "stopping");
            task.abort();
        }
        keep
    });
    for repo in wanted.into_keys() {
        if !running.contains_key(&repo) {
            tracing::info!(%repo, "watching");
            let app = App::from_credentials(&creds)?;
            running.insert(
                repo.clone(),
                (creds.id, tokio::spawn(run_repo(ctx.clone(), app, repo))),
            );
        }
    }
    Ok(())
}

async fn run_repo(ctx: Arc<Ctx>, app: App, repo: RepoName) {
    let gh = loop {
        match app.installation_for(&repo).await {
            Ok(gh) => break gh,
            Err(e) => {
                tracing::error!(%repo, error = %format!("{e:#}"), "no installation, retrying");
                tokio::time::sleep(ctx.config.poll).await;
            }
        }
    };
    loop {
        if let Err(e) = tick(&ctx, &gh, &repo).await {
            tracing::error!(%repo, error = %format!("{e:#}"), "tick failed");
        }
        tokio::time::sleep(ctx.config.poll).await;
    }
}

enum Outcome {
    Merged(String),
    Ejected(EjectReason),
    Held(String),
    /// The label was removed while the job ran.
    Cancelled,
}

async fn tick(ctx: &Ctx, gh: &Installation, repo: &RepoName) -> Result<()> {
    let labels = &ctx.config.labels;
    let snap = gh.snapshot(repo, &labels.trigger).await?;
    let key = LaneKey {
        installation_id: gh.id,
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
                ctx.store
                    .record(&key, n, "enqueued", "added to the queue")
                    .await?
            }
            LaneChange::Dequeued { number, .. } => {
                ctx.store
                    .record(&key, number, "dequeued", "left the queue")
                    .await?
            }
        }
    }
    ctx.store.save_lane(&key, &lane).await?;

    // Jobs run inside the tick, so an active entry here was cut short by a restart: run it again.
    let Some(number) = lane.active().or_else(|| lane.next()) else {
        return Ok(());
    };
    lane.activate(number);
    ctx.store.save_lane(&key, &lane).await?;

    let _slot = ctx.jobs.acquire().await?;
    let outcome = match run_job(ctx, gh, &key, &snap, number).await {
        Ok(o) => o,
        Err(e) => Outcome::Ejected(EjectReason::RebaseFailed(format!("{e:#}"))),
    };
    finish(ctx, gh, &key, &mut lane, number, outcome).await
}

async fn finish(
    ctx: &Ctx,
    gh: &Installation,
    key: &LaneKey,
    lane: &mut Lane,
    number: PrNumber,
    outcome: Outcome,
) -> Result<()> {
    match outcome {
        Outcome::Merged(sha) => {
            lane.remove(number);
            ctx.store.record(key, number, "merged", &sha).await?;
            gh.sticky_comment(&key.repo, number, &format!("Merged as {sha}."))
                .await?;
        }
        Outcome::Ejected(reason) => {
            lane.remove(number);
            eject(ctx, gh, key, number, &format!("Ejected: {reason}.")).await?;
        }
        Outcome::Held(reason) => {
            lane.hold(number, reason.clone());
            ctx.store.record(key, number, "held", &reason).await?;
            gh.sticky_comment(
                &key.repo,
                number,
                &format!("On hold: {reason}. It stays in its place in the queue."),
            )
            .await?;
        }
        Outcome::Cancelled => {
            lane.remove(number);
            ctx.store
                .record(key, number, "cancelled", "the label was removed")
                .await?;
        }
    }
    ctx.store.save_lane(key, lane).await
}

async fn eject(
    ctx: &Ctx,
    gh: &Installation,
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
    ctx.store.record(key, number, "ejected", why).await
}

async fn run_job(
    ctx: &Ctx,
    gh: &Installation,
    key: &LaneKey,
    snap: &RepoSnapshot,
    number: PrNumber,
) -> Result<Outcome> {
    let repo = &key.repo;
    let mut job = Job::start(number);
    let mut published_at = Instant::now();
    let required = gh.required_checks(repo, &key.trunk).await?;

    loop {
        let pr = gh.pull_request(repo, number).await?;
        if !pr.has_label(&ctx.config.labels.trigger) {
            return Ok(Outcome::Cancelled);
        }

        let event = match &job.phase {
            Phase::Rebasing => {
                ctx.store
                    .record(key, number, "rebasing", &format!("onto {}", key.trunk))
                    .await?;
                gh.sticky_comment(repo, number, &format!("Rebasing onto `{}`.", key.trunk))
                    .await?;
                let event = rebase_and_publish(ctx, gh, key, snap, &pr).await?;
                published_at = Instant::now();
                if let Event::Rebased { head, .. } = &event {
                    ctx.store.record(key, number, "published", head).await?;
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
                if pr.head_ref_oid != *head {
                    Event::HeadMoved
                } else if trunk != *base {
                    ctx.store
                        .record(
                            key,
                            number,
                            "base-moved",
                            "the trunk moved outside the queue",
                        )
                        .await?;
                    Event::BaseMoved
                } else {
                    let checks = gh.checks(repo, head).await?;
                    match checks.verdict(&required) {
                        (CheckState::Passed, _)
                            if required.is_empty() && checks.by_name.is_empty() =>
                        {
                            if published_at.elapsed() < NO_CHECKS_GRACE {
                                tokio::time::sleep(CHECKS_POLL).await;
                                continue;
                            }
                            Event::ChecksPassed
                        }
                        (CheckState::Passed, _) => Event::ChecksPassed,
                        (CheckState::Failed, names) => Event::ChecksFailed(names),
                        (CheckState::Pending, _) => {
                            tokio::time::sleep(CHECKS_POLL).await;
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

async fn rebase_and_publish(
    ctx: &Ctx,
    gh: &Installation,
    key: &LaneKey,
    snap: &RepoSnapshot,
    pr: &PullRequest,
) -> Result<Event> {
    let repo = &key.repo;
    let git = Git::new(ctx.exec.as_ref());
    let cache = cache_path(&ctx.config.data_dir, &repo.owner, &repo.name);
    git.ensure_cache(&cache, &format!("https://github.com/{repo}.git"))
        .await?;

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

    let job_id = format!("{}-{}-{}", repo.owner, repo.name, pr.number);
    let wt = ctx.exec.workspace(&job_id);
    let head = git.rev_parse(&cache, &pr_ref).await?;
    let trunk = git.rev_parse(&cache, &trunk_ref).await?;
    git.add_worktree(&cache, &wt, &head).await?;

    let result = rebase_in(ctx, gh, &git, &wt, key, snap, pr, &head, &trunk, &job_id).await;
    if let Err(e) = git.remove_worktree(&cache, &wt).await {
        tracing::warn!(error = %e, "could not remove worktree");
    }
    result
}

#[allow(clippy::too_many_arguments)]
async fn rebase_in(
    ctx: &Ctx,
    gh: &Installation,
    git: &Git<'_>,
    wt: &std::path::Path,
    key: &LaneKey,
    snap: &RepoSnapshot,
    pr: &PullRequest,
    head: &str,
    trunk: &str,
    job_id: &str,
) -> Result<Event> {
    if git.is_ancestor(wt, trunk, head).await? {
        return Ok(Event::Rebased {
            head: head.to_owned(),
            base: trunk.to_owned(),
        });
    }

    let pr_files: BTreeSet<String> = git
        .changed_files(wt, &git.merge_base(wt, trunk, head).await?, head)
        .await?
        .into_iter()
        .collect();
    let config = match git.file_at(wt, trunk, REPO_CONFIG_PATH).await? {
        Some(yaml) => RepoConfig::parse(&yaml)?,
        None => RepoConfig::default(),
    };

    let mut resolved: BTreeSet<String> = BTreeSet::new();
    let mut step = git.rebase(wt, trunk).await?;
    while let RebaseStep::Conflict { commit, files } = step {
        ctx.store
            .record(key, pr.number, "conflict", &files.join(", "))
            .await?;
        let _slot = ctx.resolutions.acquire().await?;
        let commit_message = git.commit_message(wt, &commit).await?;
        let transcript = ctx
            .config
            .data_dir
            .join("transcripts")
            .join(format!("{job_id}-{}.jsonl", &commit[..12]));
        let outcome = ctx
            .resolver
            .resolve(ResolveRequest {
                workdir: wt,
                conflicted_files: &files,
                pr_title: &pr.title,
                pr_body: &pr.body,
                commit_message: &commit_message,
                trunk: &key.trunk,
                verify_commands: &config.verify,
                transcript: &transcript,
            })
            .await;
        match outcome {
            Ok(Resolution::Resolved { turns, cost_usd }) => {
                let cost = cost_usd.map(|c| format!(", ${c:.2}")).unwrap_or_default();
                ctx.store
                    .record(
                        key,
                        pr.number,
                        "resolved",
                        &format!("{} in {turns} turns{cost}", files.join(", ")),
                    )
                    .await?;
            }
            Ok(Resolution::GaveUp(reason)) => {
                return abort(git, wt, format!("the resolver gave up: {reason}")).await;
            }
            Err(e) => return abort(git, wt, format!("the resolver failed: {e:#}")).await,
        }
        for f in &files {
            let content = tokio::fs::read_to_string(wt.join(f))
                .await
                .unwrap_or_default();
            if content
                .lines()
                .any(|l| l.starts_with("<<<<<<< ") || l.starts_with(">>>>>>> "))
            {
                return abort(git, wt, format!("conflict markers are left in {f}")).await;
            }
        }
        git.commit_resolution(
            wt,
            &format!("Resolved-by: mergepatrol, {}", ctx.resolver.name()),
        )
        .await?;
        resolved.extend(files);
        step = git.rebase_continue(wt).await?;
    }

    let local_head = git.rev_parse(wt, "HEAD").await?;
    if !resolved.is_empty() {
        let outside: Vec<String> = git
            .changed_files(wt, trunk, &local_head)
            .await?
            .into_iter()
            .filter(|f| !pr_files.contains(f) && !resolved.contains(f))
            .collect();
        if !outside.is_empty() {
            return Ok(Event::RebaseFailed(format!(
                "the resolution changed files outside the PR: {}",
                outside.join(", ")
            )));
        }
        for cmd in &config.verify {
            ctx.store.record(key, pr.number, "verify", cmd).await?;
            let mut c = ctx.exec.command("sh", wt);
            c.args(["-c", cmd]);
            let out = tokio::time::timeout(VERIFY_TIMEOUT, c.output())
                .await
                .context("verify timed out")??;
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let lines: Vec<&str> = stderr.lines().collect();
                let tail = lines[lines.len().saturating_sub(20)..].join("\n");
                return Ok(Event::RebaseFailed(format!(
                    "`{cmd}` failed after the resolution:\n```\n{tail}\n```"
                )));
            }
        }
    }

    let published = publish(
        gh,
        git,
        wt,
        Publish {
            repo: &key.repo,
            repo_id: &snap.id,
            branch: &pr.head_ref_name,
            expected_head: head,
            base: trunk,
            local_head: &local_head,
        },
    )
    .await;
    match published {
        Ok(sha) => Ok(Event::Rebased {
            head: sha,
            base: trunk.to_owned(),
        }),
        // `updateRefs` refuses when the branch moved: the author pushed while we rebased.
        Err(_) if gh.branch_sha(&key.repo, &pr.head_ref_name).await? != head => {
            Ok(Event::HeadMoved)
        }
        Err(e) => Ok(Event::RebaseFailed(format!("publishing failed: {e:#}"))),
    }
}

async fn abort(git: &Git<'_>, wt: &std::path::Path, reason: String) -> Result<Event> {
    let _ = git.rebase_abort(wt).await;
    Ok(Event::RebaseFailed(reason))
}
