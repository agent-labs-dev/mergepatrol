//! `mergepatrol job`: the part that runs inside the sandbox. It reads a `JobSpec` from stdin (or
//! from its link, with `--remote`), and fetches the repo with a read-only token. Then it rebases
//! and resolves conflicts with the agent, or has the agent fix the PR's failed checks. It runs
//! the verify commands after the agent, and writes the result as a git bundle. It holds no
//! credential that can write to GitHub: the orchestrator checks the bundle and publishes it.
//! Nor does it hold the Anthropic key: it calls the API through the orchestrator's proxy.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use mergepatrol_core::CheckFailure;
use mergepatrol_executor::protocol::{
    BUNDLE_FILE, JobEvent, JobOutcome, JobResult, JobSpec, RESULT_FILE, Task,
};
use mergepatrol_executor::remote;
use mergepatrol_git::{Git, RebaseStep};
use mergepatrol_resolver::{ClaudeResolver, FixRequest, Resolution, ResolveRequest, Resolver};
use tokio::io::AsyncReadExt;

const VERIFY_TIMEOUT: Duration = Duration::from_secs(600);

pub async fn main(out: PathBuf) -> Result<()> {
    let mut input = Vec::new();
    tokio::io::stdin().read_to_end(&mut input).await?;
    let spec: JobSpec = serde_json::from_slice(&input).context("bad job spec on stdin")?;
    run_to(&spec, &out).await
}

/// `mergepatrol job --remote <link>`: on a machine of its own, which reaches the orchestrator
/// only through the link.
pub async fn main_remote(link: &str) -> Result<()> {
    let spec = remote::fetch_spec(link).await?;
    let out = std::env::temp_dir().join("mergepatrol-out");
    run_to(&spec, &out).await?;
    remote::upload(link, &out, remote::CHUNK_BYTES).await
}

async fn run_to(spec: &JobSpec, out: &Path) -> Result<()> {
    let out = out.to_owned();
    tokio::fs::create_dir_all(&out).await?;

    let repo_dir = std::env::temp_dir().join(format!("mergepatrol-job-{}", std::process::id()));
    let mut events = Vec::new();
    let outcome = match run(spec, &repo_dir, &out, &mut events).await {
        Ok(()) => JobOutcome::Done,
        Err(e) => JobOutcome::Failed {
            reason: format!("{e:#}"),
        },
    };
    let _ = tokio::fs::remove_dir_all(&repo_dir).await;

    let result = JobResult { outcome, events };
    tokio::fs::write(out.join(RESULT_FILE), serde_json::to_vec_pretty(&result)?).await?;
    Ok(())
}

fn event(events: &mut Vec<JobEvent>, kind: &str, message: impl Into<String>) {
    let message = message.into();
    eprintln!("{kind}: {message}");
    events.push(JobEvent {
        kind: kind.into(),
        message,
    });
}

async fn run(spec: &JobSpec, dir: &Path, out: &Path, events: &mut Vec<JobEvent>) -> Result<()> {
    let git = Git;
    git.init(dir, &spec.clone_url).await?;
    let mut refspecs = vec![
        format!(
            "+refs/heads/{}:refs/remotes/origin/{}",
            spec.trunk, spec.trunk
        ),
        format!("+{}:refs/mergepatrol/head", spec.head_ref),
    ];
    if let Some(base_ref) = &spec.base_ref {
        refspecs.push(format!("+refs/heads/{base_ref}:refs/mergepatrol/base"));
    }
    let refspecs: Vec<&str> = refspecs.iter().map(String::as_str).collect();
    git.fetch(dir, &spec.token, &refspecs).await?;
    // The job needs the token for this fetch only. It ends before any of the repo's code runs.
    if !spec.token.is_empty() {
        mergepatrol_github::revoke_token(&spec.token).await?;
    }
    for sha in [&spec.head_sha, &spec.trunk_sha] {
        if git
            .rev_parse(dir, &format!("{sha}^{{commit}}"))
            .await
            .is_err()
        {
            bail!("{sha} is gone from GitHub: the branch moved");
        }
    }
    git.checkout_detached(dir, &spec.head_sha).await?;

    let anthropic = spec.resolver.anthropic.as_ref();
    let resolver = ClaudeResolver {
        api_key: anthropic.map(|a| a.token.clone()),
        base_url: anthropic.map(|a| a.base_url.clone()),
        model: spec.resolver.model.clone(),
        effort: spec.resolver.effort.clone(),
        timeout: Duration::from_secs(spec.resolver.timeout_secs),
        max_turns: spec.resolver.max_turns,
    };

    // The bundle holds the commits on top of what the orchestrator already has.
    let (exclude, done) = match &spec.task {
        Task::Rebase => {
            rebase(spec, &git, &resolver, dir, out, events).await?;
            (&spec.trunk_sha, "rebased")
        }
        Task::FixChecks { failures } => {
            fix(spec, &git, &resolver, failures, dir, out, events).await?;
            (&spec.head_sha, "fixed")
        }
    };

    let head = git.rev_parse(dir, "HEAD").await?;
    git.create_bundle(dir, &out.join(BUNDLE_FILE), &head, exclude)
        .await?;
    event(events, done, head);
    Ok(())
}

async fn rebase(
    spec: &JobSpec,
    git: &Git,
    resolver: &ClaudeResolver,
    dir: &Path,
    out: &Path,
    events: &mut Vec<JobEvent>,
) -> Result<()> {
    let mut resolved_any = false;
    let mut step = git
        .rebase(dir, &spec.trunk_sha, spec.upstream.as_deref())
        .await?;
    while let RebaseStep::Conflict { commit, files } = step {
        event(events, "conflict", files.join(", "));
        if resolver.api_key.is_none() {
            bail!(
                "it conflicts in {}, and the job has no Anthropic API key for an agent to resolve that",
                files.join(", ")
            );
        }
        let commit_message = git.commit_message(dir, &commit).await?;
        let transcript = out
            .join("transcripts")
            .join(format!("{}.jsonl", &commit[..12]));
        let outcome = resolver
            .resolve(ResolveRequest {
                workdir: dir,
                conflicted_files: &files,
                pr_title: &spec.pr_title,
                pr_body: &spec.pr_body,
                commit_message: &commit_message,
                trunk: &spec.trunk,
                verify_commands: &spec.verify,
                transcript: &transcript,
            })
            .await;
        match outcome {
            Ok(Resolution::Resolved { turns, cost_usd }) => {
                event(
                    events,
                    "resolved",
                    format!("{} in {}", files.join(", "), spent(turns, cost_usd)),
                );
            }
            Ok(Resolution::GaveUp(reason)) => bail!("the resolver gave up: {reason}"),
            Err(e) => bail!("the resolver failed: {e:#}"),
        }
        for f in &files {
            let content = tokio::fs::read_to_string(dir.join(f))
                .await
                .unwrap_or_default();
            if content
                .lines()
                .any(|l| l.starts_with("<<<<<<< ") || l.starts_with(">>>>>>> "))
            {
                bail!("conflict markers are left in {f}");
            }
        }
        resolved_any = true;
        step = git
            .continue_resolution(
                dir,
                &format!("Resolved-by: mergepatrol, {}", resolver.name()),
            )
            .await?;
    }

    if resolved_any {
        verify(spec, dir, events, "the resolution").await?;
    }
    Ok(())
}

/// Has the agent fix the failed checks, and commits its fix on top of the PR head.
async fn fix(
    spec: &JobSpec,
    git: &Git,
    resolver: &ClaudeResolver,
    failures: &[CheckFailure],
    dir: &Path,
    out: &Path,
    events: &mut Vec<JobEvent>,
) -> Result<()> {
    let names: Vec<&str> = failures.iter().map(|f| f.name.as_str()).collect();
    event(events, "fixing", names.join(", "));
    let transcript = out
        .join("transcripts")
        .join(format!("fix-{}.jsonl", &spec.head_sha[..12]));
    let outcome = resolver
        .fix(FixRequest {
            workdir: dir,
            failures,
            pr_title: &spec.pr_title,
            pr_body: &spec.pr_body,
            trunk: &spec.trunk,
            verify_commands: &spec.verify,
            transcript: &transcript,
        })
        .await;
    let spent = match outcome {
        Ok(Resolution::Resolved { turns, cost_usd }) => spent(turns, cost_usd),
        Ok(Resolution::GaveUp(reason)) => bail!("the agent gave up: {reason}"),
        Err(e) => bail!("the agent failed: {e:#}"),
    };
    if git.rev_parse(dir, "HEAD").await? != spec.head_sha {
        bail!("the agent moved HEAD");
    }
    if !git.is_dirty(dir).await? {
        bail!("the agent changed nothing");
    }
    verify(spec, dir, events, "the fix").await?;
    git.commit_all(
        dir,
        &format!(
            "Fix {}\n\nFixed-by: mergepatrol, {}",
            names.join(", "),
            resolver.name()
        ),
    )
    .await?;
    event(events, "fix", spent);
    Ok(())
}

fn spent(turns: u32, cost_usd: Option<f64>) -> String {
    let cost = cost_usd.map(|c| format!(", ${c:.2}")).unwrap_or_default();
    format!("{turns} turns{cost}")
}

/// Runs the repo's verify commands, which must pass after `what` the agent did.
async fn verify(spec: &JobSpec, dir: &Path, events: &mut Vec<JobEvent>, what: &str) -> Result<()> {
    for cmd in &spec.verify {
        event(events, "verify", cmd.as_str());
        let mut c = tokio::process::Command::new("sh");
        c.current_dir(dir).args(["-c", cmd]).kill_on_drop(true);
        let output = tokio::time::timeout(VERIFY_TIMEOUT, c.output())
            .await
            .with_context(|| format!("`{cmd}` timed out"))??;
        if !output.status.success() {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&output.stderr));
            let lines: Vec<&str> = text.lines().collect();
            let tail = lines[lines.len().saturating_sub(20)..].join("\n");
            bail!("`{cmd}` failed after {what}:\n```\n{tail}\n```");
        }
    }
    Ok(())
}
