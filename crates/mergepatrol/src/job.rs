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
    Answered, CheckFailure, CodeLine, Evidence, FIXED_BY, Finding, Lean, LineKind, RESOLVED_BY,
    SentBack, Spend, SpendTask,
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
    ResolveRequest, Resolver, ReviewRequest, Verify, new_title,
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

/// The checks the agent runs: the repo's verify commands, or, for a repo that sets none, the
/// commands its own CI runs for a pull request, read from the checkout. MergeLoop runs only the
/// configured ones itself: a command picked from CI tells the agent what to run, and is never a
/// reason to eject a PR.
async fn checks_for(spec: &JobSpec, dir: &Path, events: &mut Vec<JobEvent>) -> Verify {
    if !spec.verify.is_empty() {
        return Verify::Configured(spec.verify.clone());
    }
    let ci = checks_from_ci(dir).await;
    if ci.is_empty() {
        return Verify::None;
    }
    event(
        events,
        "verify",
        format!("the repo sets no verify commands: {} from its CI", ci.len()),
    );
    Verify::Ci(ci)
}

/// The commands the repo's own workflows run for a pull request, read from the checkout.
async fn checks_from_ci(dir: &Path) -> Vec<String> {
    let mut files: Vec<(String, String)> = Vec::new();
    let Ok(mut entries) = tokio::fs::read_dir(dir.join(".github/workflows")).await else {
        return Vec::new();
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Ok(text) = tokio::fs::read_to_string(&path).await else {
            continue;
        };
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        files.push((name, text));
    }
    // The workflows in a fixed order, so a review of the same head sees the same commands.
    files.sort_by(|a, b| a.0.cmp(&b.0));
    mergepatrol_core::verify_from_ci(&files)
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
    let checks = checks_for(spec, dir, events).await;

    let resolver = resolver(&spec.resolver);
    let resolver = resolver.as_ref();

    // The bundle holds the commits on top of what the orchestrator already has.
    let (exclude, done) = match &spec.task {
        Task::Rebase => {
            rebase(spec, &git, resolver, &checks, dir, out, events).await?;
            if git.rev_parse(dir, "HEAD").await? == spec.trunk_sha {
                bail!(
                    "nothing is left to merge: its changes are already on {}",
                    spec.trunk
                );
            }
            (&spec.trunk_sha, "rebased")
        }
        Task::FixChecks { failures } => {
            let names: Vec<&str> = failures.iter().map(|f| f.name.as_str()).collect();
            let what = names.join(", ");
            fix(
                spec,
                &git,
                resolver,
                &checks,
                (failures, &[], &what),
                dir,
                out,
                events,
            )
            .await?;
            (&spec.head_sha, "fixed")
        }
        Task::FixReview { findings } => {
            let what = "the review's findings";
            fix(
                spec,
                &git,
                resolver,
                &checks,
                (&[], findings, what),
                dir,
                out,
                events,
            )
            .await?;
            (&spec.head_sha, "fixed")
        }
        // These return what the agent said, not code.
        Task::Review {
            answered,
            sent_back,
            since,
        } => {
            let earlier = (answered.as_slice(), sent_back.as_slice(), since.as_deref());
            return review(spec, &git, resolver, &checks, earlier, dir, out, events).await;
        }
        Task::Question {
            finding,
            question,
            sent_back,
        } => {
            let q = (finding.as_ref(), question.as_str(), *sent_back);
            return answer(spec, &git, resolver, q, dir, out, events).await;
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
    checks: &Verify,
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
        if spec.leave_conflicts {
            bail!("{LEFT_CONFLICT} in {}", files.join(", "));
        }
        if spec.resolver.api.is_none() {
            let files = files.join(", ");
            let credential = spec.resolver.provider.credential();
            match &spec.resolver.settings_url {
                Some(url) => bail!(
                    "it conflicts in {files}, and the workspace has no {credential} for an agent to resolve that: add one in the dashboard's settings, {url}"
                ),
                None => bail!(
                    "it conflicts in {files}, and the job has no {credential} for an agent to resolve that"
                ),
            }
        }
        let commit_message = git.commit_message(dir, &commit).await?;
        let stopped_at = git.rev_parse(dir, "HEAD").await?;
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
                verify: checks,
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
        if git.rev_parse(dir, "HEAD").await? != stopped_at {
            bail!("the agent moved HEAD");
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
            .continue_resolution(dir, &format!("{RESOLVED_BY}, {}", resolver.name()))
            .await?;
    }

    if resolved_any {
        verify(spec, dir, events, "the resolution").await?;
    }
    Ok(())
}

/// How a rebase's failure starts when it stopped at a conflict, as `leave_conflicts` asks.
pub(crate) const LEFT_CONFLICT: &str = "it conflicts";

/// How a fix's failure starts when the agent found nothing in the code to fix: the orchestrator
/// reads it from the job's reason.
pub(crate) const GAVE_UP: &str = "the agent gave up";

/// How a fix's failure starts when a check failed on the PR's title, and the rest of it is the
/// title the agent proposes: a job cannot set it.
pub(crate) const RETITLED: &str = "the agent proposes the title: ";

/// Has the agent fix the failed checks, and commits its fix on top of the PR head.
#[allow(clippy::too_many_arguments)]
async fn fix(
    spec: &JobSpec,
    git: &Git,
    resolver: &dyn Resolver,
    checks: &Verify,
    (failures, findings, what): (&[CheckFailure], &[Finding], &str),
    dir: &Path,
    out: &Path,
    events: &mut Vec<JobEvent>,
) -> Result<()> {
    let transcript = out
        .join("transcripts")
        .join(format!("fix-{}.jsonl", &spec.head_sha[..12]));
    apply_evidence(git, dir, findings).await?;
    let run = resolver
        .fix(FixRequest {
            workdir: dir,
            failures,
            findings,
            pr_title: &spec.pr_title,
            pr_body: &spec.pr_body,
            trunk: &spec.trunk,
            verify: checks,
            transcript: &transcript,
        })
        .await
        .map_err(|e| anyhow!("the agent failed: {e:#}"))?;
    let title = match run.outcome {
        Outcome::Resolved if !failures.is_empty() => new_title(&run.text),
        _ => None,
    };
    // A fix is done once it is committed: until then, any failure leaves it unfixed.
    let fixed = match &title {
        Some(title) => Err(anyhow!("{RETITLED}{title}")),
        None => {
            let ran = (&run.outcome, findings, what);
            commit_fix(spec, git, resolver, ran, dir, events).await
        }
    };
    spent(
        events,
        &spec.resolver,
        SpendTask::Fix,
        fixed.is_ok() || title.is_some(),
        String::new(),
        &run,
    );
    fixed
}

/// Checks the agent's fix, and commits it: with the review's tests for `findings`, which now pass.
async fn commit_fix(
    spec: &JobSpec,
    git: &Git,
    resolver: &dyn Resolver,
    (outcome, findings, what): (&Outcome, &[Finding], &str),
    dir: &Path,
    events: &mut Vec<JobEvent>,
) -> Result<()> {
    match outcome {
        Outcome::Resolved => {}
        Outcome::GaveUp(reason) => bail!("{GAVE_UP}: {reason}"),
        Outcome::Failed(why) => bail!("the agent failed: {why}"),
    }
    if git.rev_parse(dir, "HEAD").await? != spec.head_sha {
        bail!("the agent moved HEAD");
    }
    if !git.is_dirty(dir).await? {
        bail!("the agent changed nothing");
    }
    verify(spec, dir, events, "the fix").await?;
    evidence_passes(git, dir, findings, events).await?;
    git.commit_all(
        dir,
        &format!("Fix {what}\n\n{FIXED_BY}, {}", resolver.name()),
    )
    .await
}

/// How many lines of context a finding's code shows around what it is about.
const CODE_CONTEXT: u32 = 3;
/// The most lines of code a finding carries: a hunk can be long.
const CODE_LINES: usize = 24;

/// Has the agent review the PR, and writes what it found, each finding with its code.
#[allow(clippy::too_many_arguments)]
async fn review(
    spec: &JobSpec,
    git: &Git,
    resolver: &dyn Resolver,
    checks: &Verify,
    (answered, sent_back, since): (&[Answered], &[SentBack], Option<&str>),
    dir: &Path,
    out: &Path,
    events: &mut Vec<JobEvent>,
) -> Result<()> {
    let base = git.merge_base(dir, &spec.trunk_sha, &spec.head_sha).await?;
    let queue_commits = match since {
        Some(since) => {
            // A rebase replays the PR, and leaves the head it went on from behind.
            let on_it = git.is_ancestor(dir, since, &spec.head_sha).await;
            let from = if on_it.unwrap_or(false) { since } else { &base };
            let trailers = [FIXED_BY, RESOLVED_BY];
            git.commits_with(dir, from, &spec.head_sha, &trailers)
                .await?
        }
        None => Vec::new(),
    };
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
            sent_back,
            queue_commits: &queue_commits,
            verify: checks,
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
    check_evidence(&spec.head_sha, git, dir, &mut output.findings, events).await?;
    tokio::fs::write(out.join(REVIEW_FILE), serde_json::to_vec_pretty(&output)?).await?;
    Ok(())
}

/// Has the agent answer a human's question about a finding, and writes the answer.
#[allow(clippy::too_many_arguments)]
async fn answer(
    spec: &JobSpec,
    git: &Git,
    resolver: &dyn Resolver,
    (finding, question, sent_back): (&Finding, &str, bool),
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
            sent_back,
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

/// The most findings a review keeps, so its summary fits GitHub.
pub(crate) const FINDINGS_MAX: usize = 25;

/// The review in the agent's answer: its JSON, alone or in a code block.
fn parse_review(text: &str) -> Result<ReviewOutput> {
    let json = match (text.find('{'), text.rfind('}')) {
        (Some(start), Some(end)) if start < end => &text[start..=end],
        _ => bail!("the agent's review is not JSON"),
    };
    let mut output: ReviewOutput =
        serde_json::from_str(json).context("the agent's review does not parse")?;
    output.findings.retain(|f| !f.title.trim().is_empty());
    output.findings.truncate(FINDINGS_MAX);
    Ok(output)
}

/// The PR's changes to `file` around `line`: the hunk that holds it, or the first hunk when it
/// names no line. A line no hunk holds shows the file there.
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
    let hunk = match line {
        Some(_) => hunks.iter().find(at),
        None => hunks.first(),
    };
    let Some(hunk) = hunk else {
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

/// Runs each finding's evidence on the head alone, with only the files it names, and keeps what
/// that showed: the agent's word that a command fails counts for nothing. Only a change's evidence
/// runs, as only a change can go back to the author. The tree is left at the head.
async fn check_evidence(
    head: &str,
    git: &Git,
    dir: &Path,
    findings: &mut [Finding],
    events: &mut Vec<JobEvent>,
) -> Result<()> {
    // What the agent wrote for each, taken before the tree goes back to the head.
    let mut patches = Vec::new();
    for f in findings.iter_mut() {
        let Some(e) = f.evidence.as_mut() else {
            patches.push(None);
            continue;
        };
        (e.patch, e.output, e.failed) = (String::new(), String::new(), false);
        let runs = f.lean == Lean::Change && !e.command.trim().is_empty();
        let patch = match (runs, unsafe_file(&e.files)) {
            (false, _) => None,
            (true, Some(file)) => {
                e.output = format!("MergeLoop does not run evidence that writes {file}.");
                None
            }
            (true, None) => Some(git.patch_of(dir, head, &e.files).await?),
        };
        patches.push(patch);
    }
    for (f, patch) in findings.iter_mut().zip(patches) {
        let (Some(e), Some(patch)) = (f.evidence.as_mut(), patch) else {
            continue;
        };
        git.reset_to(dir, head).await?;
        if !patch.is_empty()
            && let Err(err) = git.apply(dir, &patch).await
        {
            e.output = format!("{err:#}");
            continue;
        }
        event(events, "evidence", format!("{}: {}", f.id, e.command));
        let ran = run_command(dir, &e.command).await?;
        // A command that never ends may be stuck on the machine, not on the PR. One the shell
        // could not find or start never ran: the checkout lacks a tool, the PR is not to blame.
        e.failed = !ran.passed && !ran.timed_out && !ran.not_run;
        (e.output, e.patch) = (ran.tail, patch);
    }
    git.reset_to(dir, head).await
}

/// A file evidence may not write: one outside the repo, or one a fix may not change, which the
/// fix would have to commit.
fn unsafe_file(files: &[String]) -> Option<&str> {
    files
        .iter()
        .find(|f| {
            let p = Path::new(f.as_str());
            p.is_absolute()
                || p.components()
                    .any(|c| !matches!(c, std::path::Component::Normal(_)))
                || p.starts_with(".github")
                || p.starts_with(".mergepatrol")
        })
        .map(String::as_str)
}

/// The fix starts from the review's tests for what it fixes, in place, as the review ran them.
async fn apply_evidence(git: &Git, dir: &Path, findings: &[Finding]) -> Result<()> {
    for e in shown(findings) {
        if !e.patch.is_empty() {
            git.apply(dir, &e.patch)
                .await
                .with_context(|| format!("the review's test for `{}` does not apply", e.command))?;
        }
    }
    Ok(())
}

/// After the fix, each command that showed a finding passes, and the review's tests are as they
/// were: checked once every command ran, as a command may write files too.
async fn evidence_passes(
    git: &Git,
    dir: &Path,
    findings: &[Finding],
    events: &mut Vec<JobEvent>,
) -> Result<()> {
    for e in shown(findings) {
        event(events, "evidence", e.command.as_str());
        let ran = run_command(dir, &e.command).await?;
        if !ran.passed {
            bail!(
                "`{}` still fails after the fix:\n```\n{}\n```",
                e.command,
                ran.tail
            );
        }
    }
    for e in shown(findings) {
        if !e.patch.is_empty() && !git.holds(dir, &e.patch).await? {
            bail!(
                "the fix changed the review's test in {}",
                e.files.join(", ")
            );
        }
    }
    Ok(())
}

/// The evidence MergeLoop saw fail, of the findings that have it.
fn shown(findings: &[Finding]) -> impl Iterator<Item = &Evidence> {
    findings
        .iter()
        .filter_map(|f| f.evidence.as_ref())
        .filter(|e| e.failed)
}

/// How a command ran: whether it passed, and the end of what it printed.
struct Ran {
    passed: bool,
    timed_out: bool,
    /// The shell could not find the command, or start it: exit 127 or 126.
    not_run: bool,
    tail: String,
}

/// How much of the end of what a command printed a failure keeps: its last lines, and at most
/// this many bytes of them, as a line may be long.
const TAIL_LINES: usize = 20;
const TAIL_BYTES: usize = 4_000;

/// Runs a command from the repo's root. One that runs past the timeout did not pass.
async fn run_command(dir: &Path, cmd: &str) -> Result<Ran> {
    use std::process::Stdio;
    let mut c = tokio::process::Command::new("sh");
    c.current_dir(dir)
        .args(["-c", cmd])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // A group of its own, so that what it started stops with it.
        .process_group(0)
        .kill_on_drop(true);
    let child = c.spawn()?;
    let group = child.id();
    let done = tokio::time::timeout(VERIFY_TIMEOUT, child.wait_with_output()).await;
    if let Some(group) = group {
        stop_group(group).await;
    }
    let Ok(output) = done else {
        return Ok(Ran {
            passed: false,
            timed_out: true,
            not_run: false,
            tail: format!("`{cmd}` timed out after {}s", VERIFY_TIMEOUT.as_secs()),
        });
    };
    let output = output?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    let lines: Vec<&str> = text.lines().collect();
    let tail = lines[lines.len().saturating_sub(TAIL_LINES)..].join("\n");
    let mut cut = tail.len().saturating_sub(TAIL_BYTES);
    while !tail.is_char_boundary(cut) {
        cut += 1;
    }
    Ok(Ran {
        passed: output.status.success(),
        timed_out: false,
        not_run: matches!(output.status.code(), Some(126 | 127)),
        tail: tail[cut..].to_owned(),
    })
}

/// Stops every process left in a command's group, and waits until none is: one left running, a
/// test runner or an install, could write into the tree of the next command.
async fn stop_group(group: u32) {
    let Ok(group) = i32::try_from(group) else {
        return;
    };
    // SAFETY: `kill` takes any pid and signal; a negative pid names the group.
    unsafe { libc::kill(-group, libc::SIGKILL) };
    for _ in 0..50 {
        // SAFETY: signal 0 only asks whether the group has a process left.
        if unsafe { libc::kill(-group, 0) } != 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Runs the repo's verify commands, which must pass after `what` the agent did.
async fn verify(spec: &JobSpec, dir: &Path, events: &mut Vec<JobEvent>, what: &str) -> Result<()> {
    for cmd in &spec.verify {
        event(events, "verify", cmd.as_str());
        let ran = run_command(dir, cmd).await?;
        if ran.timed_out {
            bail!("`{cmd}` timed out");
        }
        if !ran.passed {
            bail!("`{cmd}` failed after {what}:\n```\n{}\n```", ran.tail);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn what_a_command_starts_stops_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let ran = run_command(dir.path(), "(sleep 1; touch late) >/dev/null 2>&1 & exit 1")
            .await
            .unwrap();
        assert!(!ran.passed);
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(
            !dir.path().join("late").exists(),
            "the background job was stopped"
        );
    }

    #[tokio::test]
    async fn a_commands_output_is_kept_short_however_long_its_lines() {
        let dir = tempfile::tempdir().unwrap();
        let ran = run_command(dir.path(), "yes é | head -n 50000 | tr -d '\\n'; exit 1")
            .await
            .unwrap();
        assert!(!ran.passed);
        assert!(
            ran.tail.len() <= TAIL_BYTES && ran.tail.ends_with('é'),
            "{}",
            ran.tail.len()
        );
    }

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
    async fn a_line_the_pr_did_not_change_shows_the_file_there_not_another_hunk() {
        let dir = tempfile::tempdir().unwrap();
        let sh = |cmd: &str| {
            let ok = std::process::Command::new("sh")
                .args(["-c", cmd])
                .current_dir(dir.path())
                .status()
                .unwrap()
                .success();
            assert!(ok, "{cmd}");
        };
        sh(
            "git init -q && git config user.email t@t && git config user.name t \
            && seq 1 40 > a && git add a && git commit -qm base \
            && sed -i.bak 's/^2$/two/' a && rm a.bak && git commit -qam head",
        );
        let lines = diff_lines(&Git, dir.path(), "HEAD~1", "HEAD", "a", Some(35))
            .await
            .unwrap();
        let hot: Vec<&str> = lines
            .iter()
            .filter(|c| c.hot)
            .map(|c| c.text.as_str())
            .collect();
        assert_eq!(hot, ["35"]);
        assert!(lines.iter().all(|c| c.kind == LineKind::Context));
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
