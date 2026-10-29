//! The agent: resolves the conflicts of one commit in a stopped rebase, or fixes the failed
//! checks of a PR. It edits files only: the caller checks the result and commits it.

mod claude;
mod cli;
mod codex;
mod keys;

pub use claude::ClaudeResolver;
pub use codex::CodexResolver;
pub use keys::{KeyCheck, check_api_key};

use std::path::Path;

use anyhow::Result;
use async_trait::async_trait;
use mergepatrol_core::CheckFailure;

pub struct ResolveRequest<'a> {
    pub workdir: &'a Path,
    pub conflicted_files: &'a [String],
    pub pr_title: &'a str,
    pub pr_body: &'a str,
    /// The message of the PR commit that did not apply.
    pub commit_message: &'a str,
    pub trunk: &'a str,
    pub verify_commands: &'a [String],
    /// Where to write the agent's transcript.
    pub transcript: &'a Path,
}

pub struct FixRequest<'a> {
    pub workdir: &'a Path,
    pub failures: &'a [CheckFailure],
    pub pr_title: &'a str,
    pub pr_body: &'a str,
    pub trunk: &'a str,
    pub verify_commands: &'a [String],
    pub transcript: &'a Path,
}

/// How an agent run ended, and what it spent: every run that started has one, however it ended.
#[derive(Debug)]
pub struct Resolution {
    pub turns: u32,
    /// `None` when the CLI did not say, as Codex never does, or when the run was cut short
    /// before the CLI could.
    pub cost_usd: Option<f64>,
    pub outcome: Outcome,
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Resolved,
    /// The agent was not sure, or the failure is not the PR's to fix, and said why. Mergepatrol
    /// ejects the PR rather than guess.
    GaveUp(String),
    /// The run broke: it went over its turns or its time, or the CLI failed.
    Failed(String),
}

#[async_trait]
pub trait Resolver: Send + Sync {
    /// Goes in the `Resolved-by:` trailer, for example `claude (claude-opus-5-5)`.
    fn name(&self) -> String;

    /// An error when the agent never started: no key, or no CLI.
    async fn resolve(&self, req: ResolveRequest<'_>) -> Result<Resolution>;

    async fn fix(&self, req: FixRequest<'_>) -> Result<Resolution>;
}

pub(crate) const GIVE_UP: &str = "MERGEPATROL_GIVE_UP:";

/// The reason the agent gave up, if its last message says it did.
pub(crate) fn gave_up(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.trim().strip_prefix(GIVE_UP))
        .map(|r| r.trim().to_owned())
}

/// The `verify` paragraph of a prompt.
fn verify_section(commands: &[String], after: &str) -> String {
    if commands.is_empty() {
        return "This repo has no verify commands.".to_owned();
    }
    let cmds = commands
        .iter()
        .map(|c| format!("- `{c}`"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("After you {after}, run these commands. They must pass:\n{cmds}")
}

pub(crate) fn prompt(req: &ResolveRequest<'_>) -> String {
    let files = req
        .conflicted_files
        .iter()
        .map(|f| format!("- {f}"))
        .collect::<Vec<_>>()
        .join("\n");
    let verify = verify_section(req.verify_commands, "resolve");
    format!(
        "You are resolving a rebase conflict for a merge queue.

A pull request is being rebased onto `{trunk}`. One of its commits did not apply cleanly.

Pull request: {title}

{body}

The commit that did not apply:

{commit}

These files have conflict markers:
{files}

Resolve the conflicts so that the result keeps the intent of the pull request AND the changes \
that landed on `{trunk}`. Use `git log`, `git show` and `git diff` to understand both sides.

Rules:
- Edit only the files listed above.
- Remove every conflict marker.
- Do not run git commands that change state (no add, commit, rebase, reset, checkout, stash).
- Do not refactor or improve unrelated code.

{verify}

If you cannot resolve the conflict with confidence, stop and reply with one line: \
`{GIVE_UP} <the reason>`. Do not guess.",
        trunk = req.trunk,
        title = req.pr_title,
        body = req.pr_body,
        commit = req.commit_message,
    )
}

pub(crate) fn fix_prompt(req: &FixRequest<'_>) -> String {
    let failures = req
        .failures
        .iter()
        .map(|f| format!("## {}\n\n{}", f.name, f.details.trim()))
        .collect::<Vec<_>>()
        .join("\n\n");
    let verify = verify_section(req.verify_commands, "fix");
    format!(
        "You are fixing the failed CI checks of a pull request for a merge queue.

The pull request sits on top of `{trunk}`, and the checks that must pass before it merges failed.

Pull request: {title}

{body}

What GitHub shows of the failures:

{failures}

Find out why they fail, and change the code so that they pass. Use `git log`, `git show` and \
`git diff {trunk}...HEAD` to see what the pull request changes.

Rules:
- Make the smallest change that fixes the failure, in the spirit of the pull request.
- Do not delete, skip or weaken tests or checks to make them pass.
- Do not edit `.github/` or `.mergepatrol/`: Mergepatrol rejects the fix if you do.
- Do not run git commands that change state (no add, commit, rebase, reset, checkout, stash).
- Do not refactor or improve unrelated code.

{verify}

Give up rather than guess when the failure is not the pull request's to fix: a flaky test, an \
outage, a missing secret, a failure that the logs do not explain, or a fix that changes what the \
pull request means. Then stop and reply with one line: `{GIVE_UP} <the reason>`.",
        trunk = req.trunk,
        title = req.pr_title,
        body = req.pr_body,
    )
}
