//! `mergepatrol job`: the part that runs inside the sandbox. It reads a `JobSpec` from stdin (or
//! from its link, with `--remote`), and fetches the repo with a read-only token. Then it rebases
//! and resolves conflicts with the agent, or has the agent fix the PR's failed checks. It runs
//! the verify commands after the agent, and writes the result as a git bundle. Or it has the
//! agent review the PR, or answer a question about its review, and writes what the agent said. It holds no
//! credential that can write to GitHub: the orchestrator checks the bundle and publishes it.
//! Nor does it hold the Anthropic key: it calls the API through the orchestrator's proxy.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use mergepatrol_core::{
    Answered, CheckFailure, CodeLine, FIXED_BY, Finding, LineKind, Spend, SpendTask,
};
use mergepatrol_executor::protocol::{
    ANSWER_FILE, BUNDLE_FILE, JobEvent, JobOutcome, JobResult, JobSpec, RESULT_FILE, REVIEW_FILE,
    ResolverSpec, ReviewOutput, Task,
};
use mergepatrol_executor::proxy::Api;
use mergepatrol_executor::remote;
use mergepatrol_git::{Git, RebaseStep};
use mergepatrol_resolver::{
    ClaudeResolver, CodexResolver, FixRequest, Outcome, QuestionRequest, Resolution,
    ResolveRequest, Resolver, ReviewRequest,
};
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

/// `mergepatrol job --serve <port>`: a warm sandbox, started before its job. It waits for the
/// orchestrator to send the link, takes the first one, and stops listening.
pub async fn main_serve(port: u16) -> Result<()> {
    let link = receive_link(tokio::net::TcpListener::bind(("0.0.0.0", port)).await?).await?;
    main_remote(&link).await
}

/// Serves `POST /start`, with the link as the body, until it gets one.
async fn receive_link(listener: tokio::net::TcpListener) -> Result<String> {
    use axum::http::StatusCode;
    use std::sync::{Arc, Mutex};
    let (tx, rx) = tokio::sync::oneshot::channel::<String>();
    let tx = Arc::new(Mutex::new(Some(tx)));
    let app = axum::Router::new().route(
        "/start",
        axum::routing::post(move |link: String| async move {
            if link.is_empty() {
                return StatusCode::BAD_REQUEST;
            }
            match tx.lock().unwrap().take() {
                Some(tx) => {
                    let _ = tx.send(link);
                    StatusCode::NO_CONTENT
                }
                None => StatusCode::CONFLICT,
            }
        }),
    );
    let (got, link) = tokio::sync::oneshot::channel();
    let server = axum::serve(listener, app).with_graceful_shutdown(async move {
        if let Ok(l) = rx.await {
            let _ = got.send(l);
        }
    });
    server.await?;
    link.await.context("no link")
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

    let resolver = resolver(&spec.resolver);
    let resolver = resolver.as_ref();

    // The bundle holds the commits on top of what the orchestrator already has.
    let (exclude, done) = match &spec.task {
        Task::Rebase => {
            rebase(spec, &git, resolver, dir, out, events).await?;
            (&spec.trunk_sha, "rebased")
        }
        Task::FixChecks { failures } => {
            fix(spec, &git, resolver, failures, dir, out, events).await?;
            (&spec.head_sha, "fixed")
        }
        // These return what the agent said, not code.
        Task::Review { answered } => {
            return review(spec, &git, resolver, answered, dir, out, events).await;
        }
        Task::Question { finding, question } => {
            return answer(spec, &git, resolver, finding, question, dir, out, events).await;
        }
    };

    let head = git.rev_parse(dir, "HEAD").await?;
    git.create_bundle(dir, &out.join(BUNDLE_FILE), &head, exclude)
        .await?;
    event(events, done, head);
    Ok(())
}

/// The agent for the spec's model: the Claude CLI for the Messages API, Codex for the Responses
/// API.
fn resolver(spec: &ResolverSpec) -> Box<dyn Resolver> {
    let token = spec.api.as_ref().map(|a| a.token.clone());
    let timeout = Duration::from_secs(spec.timeout_secs);
    match Api::of(spec.provider, &spec.model) {
        Api::Messages => Box::new(ClaudeResolver {
            api_key: token,
            base_url: spec.api.as_ref().map(|a| a.base_url.clone()),
            model: spec.model.clone(),
            effort: spec.effort.clone(),
            timeout,
            max_turns: spec.max_turns,
        }),
        Api::Responses => Box::new(CodexResolver {
            provider: spec.provider,
            api_key: token,
            base_url: spec
                .api
                .as_ref()
                .map(|a| a.base_url.clone())
                .unwrap_or_default(),
            model: spec.model.clone(),
            effort: spec.effort.clone(),
            timeout,
            max_turns: spec.max_turns,
        }),
    }
}

async fn rebase(
    spec: &JobSpec,
    git: &Git,
    resolver: &dyn Resolver,
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
        if spec.resolver.api.is_none() {
            bail!(
                "it conflicts in {}, and the job has no {} API key for an agent to resolve that",
                files.join(", "),
                spec.resolver.provider.name()
            );
        }
        let commit_message = git.commit_message(dir, &commit).await?;
        let transcript = out
            .join("transcripts")
            .join(format!("{}.jsonl", &commit[..12]));
        let run = resolver
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
            .await
            .map_err(|e| anyhow!("the resolver failed: {e:#}"))?;
        let done = run.outcome == Outcome::Resolved;
        spent(
            events,
            &spec.resolver,
            SpendTask::Rebase,
            done,
            files.join(", "),
            &run,
        );
        match run.outcome {
            Outcome::Resolved => {}
            Outcome::GaveUp(reason) => bail!("the resolver gave up: {reason}"),
            Outcome::Failed(why) => bail!("the resolver failed: {why}"),
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
    resolver: &dyn Resolver,
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
    let run = resolver
        .fix(FixRequest {
            workdir: dir,
            failures,
            pr_title: &spec.pr_title,
            pr_body: &spec.pr_body,
            trunk: &spec.trunk,
            verify_commands: &spec.verify,
            transcript: &transcript,
        })
        .await
        .map_err(|e| anyhow!("the agent failed: {e:#}"))?;
    // A fix is done once it is committed: until then, any failure leaves it unfixed.
    let fixed = commit_fix(spec, git, resolver, &run.outcome, &names, dir, events).await;
    spent(
        events,
        &spec.resolver,
        SpendTask::Fix,
        fixed.is_ok(),
        String::new(),
        &run,
    );
    fixed
}

/// Checks the agent's fix, and commits it.
async fn commit_fix(
    spec: &JobSpec,
    git: &Git,
    resolver: &dyn Resolver,
    outcome: &Outcome,
    names: &[&str],
    dir: &Path,
    events: &mut Vec<JobEvent>,
) -> Result<()> {
    match outcome {
        Outcome::Resolved => {}
        Outcome::GaveUp(reason) => bail!("the agent gave up: {reason}"),
        Outcome::Failed(why) => bail!("the agent failed: {why}"),
    }
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
            "Fix {}\n\n{FIXED_BY}, {}",
            names.join(", "),
            resolver.name()
        ),
    )
    .await
}

/// How many lines of context a finding's code shows around what it is about.
const CODE_CONTEXT: u32 = 3;
/// The most lines of code a finding carries: a hunk can be long.
const CODE_LINES: usize = 24;

/// Has the agent review the PR, and writes what it found, each finding with its code.
async fn review(
    spec: &JobSpec,
    git: &Git,
    resolver: &dyn Resolver,
    answered: &[Answered],
    dir: &Path,
    out: &Path,
    events: &mut Vec<JobEvent>,
) -> Result<()> {
    let base = git.merge_base(dir, &spec.trunk_sha, &spec.head_sha).await?;
    event(
        events,
        "reviewing",
        format!("{}..{}", &base[..12], &spec.head_sha[..12]),
    );
    let transcript = out
        .join("transcripts")
        .join(format!("review-{}.jsonl", &spec.head_sha[..12]));
    let run = resolver
        .review(ReviewRequest {
            workdir: dir,
            base: &base,
            pr_title: &spec.pr_title,
            pr_body: &spec.pr_body,
            trunk: &spec.trunk,
            answered,
            transcript: &transcript,
        })
        .await
        .map_err(|e| anyhow!("the agent failed: {e:#}"))?;
    let parsed = match &run.outcome {
        Outcome::Failed(why) => Err(anyhow!("the agent failed: {why}")),
        Outcome::GaveUp(why) => Err(anyhow!("the agent gave up: {why}")),
        Outcome::Resolved => parse_review(&run.text),
    };
    spent(
        events,
        &spec.resolver,
        SpendTask::Review,
        parsed.is_ok(),
        String::new(),
        &run,
    );
    let mut output = parsed?;
    for (i, f) in output.findings.iter_mut().enumerate() {
        f.id = format!("f{}", i + 1);
        f.confidence = f.confidence.clamp(0, 100);
        if let Some(file) = f.file.clone() {
            f.code = diff_lines(git, dir, &base, &spec.head_sha, &file, f.line).await?;
        }
        for r in &mut f.related {
            r.code = file_lines(git, dir, &spec.head_sha, &r.file, r.line).await?;
        }
    }
    tokio::fs::write(out.join(REVIEW_FILE), serde_json::to_vec_pretty(&output)?).await?;
    Ok(())
}

/// Has the agent answer a human's question about a finding, and writes the answer.
#[allow(clippy::too_many_arguments)]
async fn answer(
    spec: &JobSpec,
    git: &Git,
    resolver: &dyn Resolver,
    finding: &Finding,
    question: &str,
    dir: &Path,
    out: &Path,
    events: &mut Vec<JobEvent>,
) -> Result<()> {
    let base = git.merge_base(dir, &spec.trunk_sha, &spec.head_sha).await?;
    let transcript = out
        .join("transcripts")
        .join(format!("question-{}.jsonl", &spec.head_sha[..12]));
    let run = resolver
        .question(QuestionRequest {
            workdir: dir,
            base: &base,
            pr_title: &spec.pr_title,
            finding,
            question,
            transcript: &transcript,
        })
        .await
        .map_err(|e| anyhow!("the agent failed: {e:#}"))?;
    let done = run.outcome == Outcome::Resolved && !run.text.trim().is_empty();
    spent(
        events,
        &spec.resolver,
        SpendTask::Review,
        done,
        String::new(),
        &run,
    );
    match &run.outcome {
        Outcome::Failed(why) => bail!("the agent failed: {why}"),
        Outcome::GaveUp(why) => bail!("the agent gave up: {why}"),
        Outcome::Resolved if !done => bail!("the agent answered nothing"),
        Outcome::Resolved => {}
    }
    tokio::fs::write(out.join(ANSWER_FILE), run.text.trim()).await?;
    Ok(())
}

/// The review in the agent's answer: its JSON, alone or in a code block.
fn parse_review(text: &str) -> Result<ReviewOutput> {
    let json = match (text.find('{'), text.rfind('}')) {
        (Some(start), Some(end)) if start < end => &text[start..=end],
        _ => bail!("the agent's review is not JSON"),
    };
    let mut output: ReviewOutput =
        serde_json::from_str(json).context("the agent's review does not parse")?;
    output.findings.retain(|f| !f.title.trim().is_empty());
    output.findings.truncate(25);
    Ok(output)
}

/// The PR's changes to `file` around `line`: the hunk that holds it, or the first hunk.
async fn diff_lines(
    git: &Git,
    dir: &Path,
    base: &str,
    head: &str,
    file: &str,
    line: Option<u32>,
) -> Result<Vec<CodeLine>> {
    let diff = git.diff_file(dir, base, head, file, CODE_CONTEXT).await?;
    let hunks = hunks(&diff);
    let at = |h: &&Vec<CodeLine>| {
        line.is_some_and(|l| {
            h.iter()
                .any(|c| c.kind != LineKind::Removed && c.number == Some(l))
        })
    };
    let Some(hunk) = hunks.iter().find(at).or(hunks.first()) else {
        // Not changed by the PR: the lines as they are.
        return file_lines(git, dir, head, file, line).await;
    };
    let mut lines = hunk.clone();
    if let Some(l) = line {
        // The lines next to the one it is about, when the hunk is long.
        if lines.len() > CODE_LINES {
            let i = lines
                .iter()
                .position(|c| c.kind != LineKind::Removed && c.number == Some(l))
                .unwrap_or(0);
            let start = i
                .saturating_sub(CODE_LINES / 2)
                .min(lines.len() - CODE_LINES);
            lines = lines[start..start + CODE_LINES].to_vec();
        }
        let near = |n: u32| n.abs_diff(l) <= 2;
        for c in &mut lines {
            c.hot = c.kind == LineKind::Added && c.number.is_some_and(near);
        }
    } else {
        lines.truncate(CODE_LINES);
    }
    Ok(lines)
}

/// A unified diff's hunks, each line numbered on its own side: an added or kept line by the
/// new file, a removed one by the old.
fn hunks(diff: &str) -> Vec<Vec<CodeLine>> {
    let mut hunks: Vec<Vec<CodeLine>> = Vec::new();
    let (mut old, mut new) = (0u32, 0u32);
    for text in diff.lines() {
        if let Some(header) = text.strip_prefix("@@ ") {
            let mut parts = header.split_whitespace();
            let start = |p: Option<&str>, sign: char| {
                p.and_then(|p| p.strip_prefix(sign))
                    .and_then(|p| p.split(',').next())
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0)
            };
            old = start(parts.next(), '-');
            new = start(parts.next(), '+');
            hunks.push(Vec::new());
            continue;
        }
        let Some(hunk) = hunks.last_mut() else {
            continue;
        };
        let (kind, number, rest) = match text.chars().next() {
            Some('+') => {
                new += 1;
                (LineKind::Added, new - 1, &text[1..])
            }
            Some('-') => {
                old += 1;
                (LineKind::Removed, old - 1, &text[1..])
            }
            Some(' ') => {
                old += 1;
                new += 1;
                (LineKind::Context, new - 1, &text[1..])
            }
            // `\ No newline at end of file`
            _ => continue,
        };
        hunk.push(CodeLine {
            number: Some(number),
            kind,
            text: rest.to_owned(),
            hot: false,
        });
    }
    hunks
}

/// A file's lines around `line` at `rev`, or its first lines.
async fn file_lines(
    git: &Git,
    dir: &Path,
    rev: &str,
    file: &str,
    line: Option<u32>,
) -> Result<Vec<CodeLine>> {
    let Some(text) = git.show_file(dir, rev, file).await? else {
        return Ok(Vec::new());
    };
    let all: Vec<&str> = text.lines().collect();
    let at = line.unwrap_or(1).max(1) as usize;
    let start = at.saturating_sub(CODE_CONTEXT as usize + 1);
    let end = (at + CODE_CONTEXT as usize).min(all.len());
    Ok((start..end)
        .map(|i| CodeLine {
            number: Some(i as u32 + 1),
            kind: LineKind::Context,
            text: all[i].to_owned(),
            hot: line == Some(i as u32 + 1),
        })
        .collect())
}

/// The event of an agent run, which says what it spent however it ended.
fn spent(
    events: &mut Vec<JobEvent>,
    resolver: &ResolverSpec,
    task: SpendTask,
    done: bool,
    files: String,
    run: &Resolution,
) {
    let spend = Spend {
        task,
        done,
        files,
        turns: run.turns,
        cost_usd: run.cost_usd,
        model: Some(resolver.model.clone()),
        effort: Some(resolver.effort.clone()),
    };
    event(events, spend.kind(), spend.message());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_review_reads_from_the_agents_json_in_or_out_of_a_code_block() {
        let text = "Here it is:\n```json\n{\"summary\": \"s\", \"findings\": [{\"kind\": \"security\", \"title\": \"t\", \"file\": \"a.rs\", \"line\": 3, \"confidence\": 70, \"lean\": \"fine\", \"finding\": \"f\"}, {\"kind\": \"other\", \"title\": \" \", \"confidence\": 90, \"lean\": \"fine\", \"finding\": \"\"}]}\n```";
        let r = parse_review(text).unwrap();
        assert_eq!(r.summary, "s");
        assert_eq!(r.findings.len(), 1, "a finding without a title is dropped");
        assert_eq!(r.findings[0].kind, mergepatrol_core::Kind::Security);
        assert_eq!(r.findings[0].line, Some(3));
        assert!(parse_review("I could not review it.").is_err());
    }

    #[test]
    fn hunks_number_each_line_on_its_side() {
        let diff = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -10,3 +10,4 @@ fn x\n keep\n-old\n+new\n+more\n keep2\n\\ No newline at end of file\n";
        let h = hunks(diff);
        assert_eq!(h.len(), 1);
        let got: Vec<(LineKind, Option<u32>, &str)> = h[0]
            .iter()
            .map(|c| (c.kind, c.number, c.text.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                (LineKind::Context, Some(10), "keep"),
                (LineKind::Removed, Some(11), "old"),
                (LineKind::Added, Some(11), "new"),
                (LineKind::Added, Some(12), "more"),
                (LineKind::Context, Some(13), "keep2"),
            ]
        );
    }

    #[tokio::test]
    async fn a_warm_sandbox_takes_the_first_link_and_stops_listening() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/start", listener.local_addr().unwrap());
        let received = tokio::spawn(receive_link(listener));
        let http = reqwest::Client::new();
        let empty = http.post(&url).body("").send().await.unwrap();
        assert_eq!(empty.status(), 400, "an empty link is refused");
        let res = http
            .post(&url)
            .body("http://orch/jobs/t")
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 204);
        assert_eq!(received.await.unwrap().unwrap(), "http://orch/jobs/t");
        assert!(
            http.post(&url).body("x").send().await.is_err(),
            "it stopped listening"
        );
    }
}
