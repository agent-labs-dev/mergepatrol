//! The agent: resolves the conflicts of one commit in a stopped rebase, fixes the failed
//! checks of a PR, or reviews a PR. It edits files only: the caller checks the result and
//! commits it. A review edits nothing: its answer is its last message.

mod chatgpt;
mod claude;
mod cli;
mod codex;
mod keys;

#[cfg(any(test, feature = "testing"))]
pub use chatgpt::testing;
pub use chatgpt::{ChatGptAuth, ChatGptLogin, DeviceLogin};
pub use claude::ClaudeResolver;
pub use codex::CodexResolver;
pub use keys::{KeyCheck, check_api_key};

use std::path::Path;

use anyhow::Result;
use async_trait::async_trait;
use mergepatrol_core::{Answer, Answered, CheckFailure, Finding, SentBack};

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

pub struct ReviewRequest<'a> {
    pub workdir: &'a Path,
    /// Where the PR left the trunk: its changes are `base..HEAD`.
    pub base: &'a str,
    pub pr_title: &'a str,
    pub pr_body: &'a str,
    pub trunk: &'a str,
    /// What humans said about findings on an earlier head.
    pub answered: &'a [Answered],
    /// What the agent sent back to the author on the last head, to check the fix of, with the
    /// author's replies.
    pub sent_back: &'a [SentBack],
    pub transcript: &'a Path,
}

pub struct QuestionRequest<'a> {
    pub workdir: &'a Path,
    pub base: &'a str,
    pub pr_title: &'a str,
    pub finding: &'a Finding,
    pub question: &'a str,
    /// The finding went back to the author, and the question is their reply.
    pub sent_back: bool,
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
    /// The agent's last message: a review's or a question's answer.
    pub text: String,
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

    /// Reads the PR and answers with a review, as `review_prompt` asks. Changes nothing.
    async fn review(&self, req: ReviewRequest<'_>) -> Result<Resolution>;

    /// Answers a human's question about a finding. Changes nothing.
    async fn question(&self, req: QuestionRequest<'_>) -> Result<Resolution>;
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

/// How an answer to the author's reply to a finding sent back ends: the agent withdraws it, or
/// it stands.
pub const WITHDRAWN: &str = "VERDICT: withdrawn";
pub const STANDS: &str = "VERDICT: stands";

/// Whether an answer to the author's reply withdraws the finding: its last line says so.
pub fn withdraws(answer: &str) -> bool {
    answer
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .is_some_and(|l| l.trim().trim_matches('`').eq_ignore_ascii_case(WITHDRAWN))
}

/// The JSON a review answers with, as `review_prompt` describes it.
pub(crate) const REVIEW_SHAPE: &str = r#"{
  "summary": "One or two sentences: what the PR does, and how it holds up.",
  "findings": [
    {
      "kind": "security | migrations | dependencies | public_api | other",
      "title": "One line, stated as a fact, e.g. \"The old secret keeps working for 24 hours\"",
      "file": "path/in/the/repo, or null for the whole PR",
      "line": 93,
      "confidence": 85,
      "lean": "fine | change",
      "finding": "What you saw, in one to three sentences.",
      "unsure": "What you could not tell from the code, and why. Empty when you are sure.",
      "checked": ["What you verified, so a human need not"],
      "would_clear": "What would have let you decide alone",
      "suggestion": "The change you would ask the author for. Empty when there is none.",
      "needs_human": false,
      "related": [{ "file": "path", "line": 41, "why": "What this code told you" }]
    }
  ]
}"#;

pub(crate) fn review_prompt(req: &ReviewRequest<'_>) -> String {
    let answered = if req.answered.is_empty() {
        String::new()
    } else {
        let items = req
            .answered
            .iter()
            .map(|a| {
                let what = match &a.answer {
                    Answer::Fine => "fine as it is".to_owned(),
                    Answer::Change { note } => format!("needs a change: {note}"),
                };
                let place = match (&a.finding.file, a.finding.line) {
                    (Some(f), Some(l)) => format!(" ({f}:{l})"),
                    (Some(f), None) => format!(" ({f})"),
                    _ => String::new(),
                };
                format!("- {}{place}: @{} said {what}", a.finding.title, a.by)
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "\nHumans already answered these findings on an earlier version of this PR. Do not \
raise them again unless the lines they are about changed since; if a change was asked for, \
check whether it was made.\n{items}\n"
        )
    };
    let sent_back = if req.sent_back.is_empty() {
        String::new()
    } else {
        let items = req
            .sent_back
            .iter()
            .map(|s| {
                let f = &s.finding;
                let place = match (&f.file, f.line) {
                    (Some(file), Some(l)) => format!(" ({file}:{l})"),
                    (Some(file), None) => format!(" ({file})"),
                    _ => String::new(),
                };
                let reply = match (s.resolved, s.reply.trim()) {
                    (true, "") => " The author resolved it.".to_owned(),
                    (true, r) => format!(" The author resolved it, replying: {r}"),
                    (false, "") => String::new(),
                    (false, r) => format!(" The author replied: {r}"),
                };
                format!("- {}{place}: {}{reply}", f.title, f.suggestion)
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "\nYou sent these findings back to the author on the last version of this PR. Check \
each against the code now. One that was fixed, report as a `fine` finding with the same title, \
saying how it was fixed. One the author answered instead of changing: if their reply holds up in \
the code, report it as `fine` with the same title; if it does not, report it again with the same \
title and say why the reply does not hold. One that was neither fixed nor answered, report again \
with the same title, so the author can tell it is the same one, and say what is still missing.\n{items}\n"
        )
    };
    format!(
        "You are reviewing a pull request before a merge queue merges it into `{trunk}`.

Pull request: {title}

{body}
{answered}{sent_back}
Its changes are `git diff {base}..HEAD`. Read them, and read the code around them that you need \
to judge them: callers, tests, config, deploy files, earlier migrations, docs. Use `git log`, \
`git show` and `git diff`, and read files. Change nothing.

Report what a careful senior reviewer would look at, each as a finding:
- What could be wrong: bugs, security holes, data loss, broken deploys, breaking changes to what \
others use, missing tests for risky code.
- What you checked and found sound, which a reviewer would otherwise check themselves. Keep these \
short.

For each finding give `lean`: `fine` if it can merge as it is, `change` if the author should \
change it. Give `confidence`, 50 to 100: how sure you are that your lean is right, such that a \
careful senior reviewer who knows this project would agree. Below 100 when it rests on something \
the code does not show you: a policy, how production is set up, what the author meant. Say what \
in `unsure`. Do not inflate it: a change you are sure enough of goes back to the author, often \
the agent that wrote the PR, who must fix it before the PR can merge.

Set `needs_human` to true only for a change the author cannot make from the PR and its \
description: a call on scope, product or design that the PR does not settle, access or \
credentials it does not have, or a change outside this repo. A human is asked about those. \
Anything the author can fix in the code, however hard, is not one.

`kind` says what the finding is about: `security` (auth, secrets, crypto, untrusted input), \
`migrations` (schema or data migrations), `dependencies` (new or bumped packages, lockfiles), \
`public_api` (routes, exported types, CLI flags, config keys others rely on), else `other`.

Give `file` and `line` for a finding about code: the line on the PR's side, which must be one \
of the lines it changed or close to them. At most 25 findings, the ones that matter most first.

Reply with the JSON alone, in this shape, and nothing else:
{REVIEW_SHAPE}",
        trunk = req.trunk,
        title = req.pr_title,
        body = req.pr_body,
        base = req.base,
    )
}

pub(crate) fn question_prompt(req: &QuestionRequest<'_>) -> String {
    let f = req.finding;
    let place = match (&f.file, f.line) {
        (Some(file), Some(line)) => format!("{file}:{line}"),
        (Some(file), None) => file.clone(),
        _ => "the whole PR".into(),
    };
    if req.sent_back {
        return format!(
            "You reviewed a pull request, {title}, and sent one finding back to its author to \
change. The author, or the agent that wrote the PR, replies instead of changing it.

Your finding, about {place}: {ftitle}
{finding}
The change you asked for: {suggestion}

Their reply: {question}

The PR's changes are `git diff {base}..HEAD`. Read whatever code helps you judge the reply: use \
`git log`, `git show`, `git diff`, and read files. Change nothing.

Stay adversarial: the reply wants the finding gone, and you hold it to the code, not to how \
sure it sounds. Answer in a short paragraph of plain text: what you found. Then end with one \
line, alone: `{WITHDRAWN}` if the reply shows the finding was wrong or does not apply, so it \
no longer holds the PR, else `{STANDS}`.",
            title = req.pr_title,
            ftitle = f.title,
            finding = f.finding,
            suggestion = f.suggestion,
            question = req.question,
            base = req.base,
        );
    }
    format!(
        "You reviewed a pull request, {title}, and were not sure enough about one finding to \
decide it alone. A human reviewing it has a question for you.

Your finding, about {place}: {ftitle}
{finding}
What you could not tell: {unsure}

Their question: {question}

The PR's changes are `git diff {base}..HEAD`. Read whatever code helps you answer: use `git log`, \
`git show`, `git diff`, and read files. Change nothing.

Answer in a short paragraph of plain text: what you found, and how sure you are. Say so if the \
code cannot tell.",
        title = req.pr_title,
        ftitle = f.title,
        finding = f.finding,
        unsure = f.unsure,
        question = req.question,
        base = req.base,
    )
}

#[cfg(test)]
mod withdraw_tests {
    use super::*;

    #[test]
    fn only_a_last_line_that_withdraws_withdraws() {
        assert!(withdraws(
            "The caller holds the lock.\n\nVERDICT: withdrawn\n"
        ));
        assert!(withdraws("Fine.\n`verdict: withdrawn`"));
        assert!(!withdraws("It still races.\nVERDICT: stands"));
        assert!(!withdraws(
            "VERDICT: withdrawn\nBut on second look it races."
        ));
        assert!(!withdraws(""));
    }
}
