//! Resolves the conflicts of one commit in a stopped rebase. The agent edits files only:
//! the caller checks the result, commits it, and goes on with the rebase.

mod claude;

pub use claude::ClaudeResolver;

use std::path::Path;

use anyhow::Result;
use async_trait::async_trait;

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

#[derive(Debug)]
pub enum Resolution {
    Resolved {
        turns: u32,
        cost_usd: Option<f64>,
    },
    /// The agent was not sure, and said why. Mergepatrol ejects the PR rather than guess.
    GaveUp(String),
}

#[async_trait]
pub trait Resolver: Send + Sync {
    /// Goes in the `Resolved-by:` trailer, for example `claude (claude-opus-5-5)`.
    fn name(&self) -> String;

    async fn resolve(&self, req: ResolveRequest<'_>) -> Result<Resolution>;
}

pub(crate) const GIVE_UP: &str = "MERGEPATROL_GIVE_UP:";

pub(crate) fn prompt(req: &ResolveRequest<'_>) -> String {
    let files = req
        .conflicted_files
        .iter()
        .map(|f| format!("- {f}"))
        .collect::<Vec<_>>()
        .join("\n");
    let verify = if req.verify_commands.is_empty() {
        "This repo has no verify commands.".to_owned()
    } else {
        let cmds = req
            .verify_commands
            .iter()
            .map(|c| format!("- `{c}`"))
            .collect::<Vec<_>>()
            .join("\n");
        format!("After you resolve, run these commands. They must pass:\n{cmds}")
    };
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
