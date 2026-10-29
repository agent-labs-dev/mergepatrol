//! `mergepatrol job`: the part that runs inside the sandbox. It reads a `JobSpec` from stdin (or
//! from its link, with `--remote`),
//! fetches the repo with a read-only token, rebases, resolves conflicts with the agent, runs the
//! verify commands, and writes the result as a git bundle. It holds no credential that can
//! write to GitHub: the orchestrator checks the bundle and publishes it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use mergepatrol_executor::protocol::{
    BUNDLE_FILE, JobEvent, JobOutcome, JobResult, JobSpec, RESULT_FILE,
};
use mergepatrol_executor::remote;
use mergepatrol_git::{Git, RebaseStep};
use mergepatrol_resolver::{ClaudeResolver, Resolution, ResolveRequest, Resolver};
use tokio::io::AsyncReadExt;

use crate::config::{REPO_CONFIG_PATH, RepoConfig};

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
        Ok(()) => JobOutcome::Rebased,
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
    git.fetch(
        dir,
        &spec.token,
        &[
            &format!(
                "+refs/heads/{}:refs/remotes/origin/{}",
                spec.trunk, spec.trunk
            ),
            &format!("+refs/heads/{}:refs/mergepatrol/head", spec.head_ref),
        ],
    )
    .await?;
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

    let config = match git.file_at(dir, &spec.trunk_sha, REPO_CONFIG_PATH).await? {
        Some(yaml) => RepoConfig::parse(&yaml)?,
        None => RepoConfig::default(),
    };
    let resolver = ClaudeResolver {
        api_key: spec.resolver.anthropic_api_key.clone(),
        model: spec.resolver.model.clone(),
        effort: spec.resolver.effort.clone(),
        timeout: Duration::from_secs(spec.resolver.timeout_secs),
        max_turns: spec.resolver.max_turns,
    };

    let mut resolved_any = false;
    let mut step = git.rebase(dir, &spec.trunk_sha).await?;
    while let RebaseStep::Conflict { commit, files } = step {
        event(events, "conflict", files.join(", "));
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
                verify_commands: &config.verify,
                transcript: &transcript,
            })
            .await;
        match outcome {
            Ok(Resolution::Resolved { turns, cost_usd }) => {
                let cost = cost_usd.map(|c| format!(", ${c:.2}")).unwrap_or_default();
                event(
                    events,
                    "resolved",
                    format!("{} in {turns} turns{cost}", files.join(", ")),
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
        git.commit_resolution(
            dir,
            &format!("Resolved-by: mergepatrol, {}", resolver.name()),
        )
        .await?;
        resolved_any = true;
        step = git.rebase_continue(dir).await?;
    }

    if resolved_any {
        for cmd in &config.verify {
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
                bail!("`{cmd}` failed after the resolution:\n```\n{tail}\n```");
            }
        }
    }

    let head = git.rev_parse(dir, "HEAD").await?;
    git.create_bundle(dir, &out.join(BUNDLE_FILE), &head, &spec.trunk_sha)
        .await?;
    event(events, "rebased", head);
    Ok(())
}
