use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde_json::Value;

use crate::cli::{self, Ended};
use crate::{
    FixRequest, Outcome, QuestionRequest, Resolution, ResolveRequest, Resolver, ReviewRequest,
    fix_prompt, gave_up, prompt, question_prompt, review_prompt,
};

/// What the agent may use: to edit and verify, or only to read.
#[derive(Clone, Copy, PartialEq)]
enum Access {
    Edit,
    Read,
}

/// Runs Claude Code headless (`claude -p`) in the job's worktree.
pub struct ClaudeResolver {
    /// An Anthropic API key, or a job's token for the proxy at `base_url`.
    pub api_key: Option<String>,
    /// The Anthropic API's URL. `None` is the CLI's default.
    pub base_url: Option<String>,
    pub model: String,
    pub effort: String,
    pub timeout: Duration,
    /// The CLI has no turn limit, so we count assistant messages in the stream.
    pub max_turns: u32,
}

impl ClaudeResolver {
    fn allowed_tools(access: Access, verify: &[String]) -> String {
        let mut tools: Vec<String> = ["Read", "Glob", "Grep"].map(String::from).into();
        if access == Access::Edit {
            tools.extend(["Edit", "Write"].map(String::from));
        }
        for git in ["status", "diff", "log", "show"] {
            tools.push(format!("Bash(git {git}:*)"));
        }
        tools.extend(verify.iter().map(|c| format!("Bash({c})")));
        tools.join(",")
    }

    fn command(
        &self,
        workdir: &Path,
        prompt: String,
        access: Access,
        verify: &[String],
    ) -> Result<tokio::process::Command> {
        let key = self
            .api_key
            .clone()
            .context("no Anthropic API key: add one in the dashboard settings")?;
        let mut cmd = tokio::process::Command::new("claude");
        cmd.current_dir(workdir);
        // `--bare` reads only ANTHROPIC_API_KEY, never a login on the machine.
        cmd.env("ANTHROPIC_API_KEY", key);
        if let Some(url) = &self.base_url {
            cmd.env("ANTHROPIC_BASE_URL", url);
            cli::bypass_proxy(&mut cmd, url);
        }
        cmd.arg("-p")
            .arg(prompt)
            .args([
                "--bare",
                "--output-format",
                "stream-json",
                "--verbose",
                "--no-session-persistence",
            ])
            .args(["--model", &self.model, "--effort", &self.effort])
            // Anything not in the allowlist is denied, never prompted.
            .args(["--permission-mode", "dontAsk"])
            .arg("--allowedTools")
            .arg(Self::allowed_tools(access, verify));
        Ok(cmd)
    }

    async fn run(
        &self,
        workdir: &Path,
        prompt: String,
        access: Access,
        verify: &[String],
        transcript_path: &Path,
    ) -> Result<Resolution> {
        let cmd = self.command(workdir, prompt, access, verify)?;
        let mut turns = 0;
        let mut result = None;
        let ended = cli::stream(cmd, "claude", self.timeout, transcript_path, |event| {
            match event["type"].as_str() {
                Some("assistant") => {
                    turns += 1;
                    if turns > self.max_turns {
                        bail!("Claude went over {} turns", self.max_turns);
                    }
                }
                Some("result") => result = Some(event.clone()),
                _ => {}
            }
            Ok(())
        })
        .await?;
        Ok(Resolution {
            turns,
            cost_usd: result.as_ref().and_then(|r| r["total_cost_usd"].as_f64()),
            text: result
                .as_ref()
                .and_then(|r| r["result"].as_str())
                .unwrap_or_default()
                .to_owned(),
            outcome: outcome(ended, result.as_ref()),
        })
    }
}

/// How the run ended, from the CLI's `result` event.
fn outcome(ended: Ended, result: Option<&Value>) -> Outcome {
    let (status, stderr) = match ended {
        Ended::Stopped(why) => return Outcome::Failed(why),
        Ended::Exited(status, stderr) => (status, stderr),
    };
    let Some(result) = result else {
        return Outcome::Failed(format!(
            "claude exited ({status}) with no result: {}",
            stderr.trim()
        ));
    };
    if result["is_error"].as_bool() == Some(true) {
        return Outcome::Failed(format!(
            "claude: {}",
            result["result"]
                .as_str()
                .or(result["subtype"].as_str())
                .unwrap_or("error")
        ));
    }
    match gave_up(result["result"].as_str().unwrap_or_default()) {
        Some(reason) => Outcome::GaveUp(reason),
        None => Outcome::Resolved,
    }
}

#[async_trait]
impl Resolver for ClaudeResolver {
    fn name(&self) -> String {
        format!("claude ({})", self.model)
    }

    async fn resolve(&self, req: ResolveRequest<'_>) -> Result<Resolution> {
        self.run(
            req.workdir,
            prompt(&req),
            Access::Edit,
            req.verify_commands,
            req.transcript,
        )
        .await
    }

    async fn fix(&self, req: FixRequest<'_>) -> Result<Resolution> {
        self.run(
            req.workdir,
            fix_prompt(&req),
            Access::Edit,
            req.verify_commands,
            req.transcript,
        )
        .await
    }

    async fn review(&self, req: ReviewRequest<'_>) -> Result<Resolution> {
        let prompt = review_prompt(&req);
        self.run(req.workdir, prompt, Access::Read, &[], req.transcript)
            .await
    }

    async fn question(&self, req: QuestionRequest<'_>) -> Result<Resolution> {
        let prompt = question_prompt(&req);
        self.run(req.workdir, prompt, Access::Read, &[], req.transcript)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(cmd: &tokio::process::Command, name: &str) -> Option<String> {
        cmd.as_std()
            .get_envs()
            .find(|(k, _)| *k == name)
            .and_then(|(_, v)| v.map(|v| v.to_string_lossy().into_owned()))
    }

    fn exited() -> Ended {
        use std::os::unix::process::ExitStatusExt;
        Ended::Exited(std::process::ExitStatus::from_raw(0), String::new())
    }

    #[test]
    fn every_ending_is_an_outcome() {
        let result = |text: &str, is_error: bool| serde_json::json!({ "type": "result", "result": text, "is_error": is_error });
        assert_eq!(
            outcome(exited(), Some(&result("Done.", false))),
            Outcome::Resolved
        );
        assert_eq!(
            outcome(
                exited(),
                Some(&result("MERGEPATROL_GIVE_UP: a flaky test", false))
            ),
            Outcome::GaveUp("a flaky test".into())
        );
        assert_eq!(
            outcome(exited(), Some(&result("overloaded", true))),
            Outcome::Failed("claude: overloaded".into())
        );
        assert!(matches!(outcome(exited(), None), Outcome::Failed(_)));
        assert_eq!(
            outcome(Ended::Stopped("Claude went over 3 turns".into()), None),
            Outcome::Failed("Claude went over 3 turns".into())
        );
    }

    #[test]
    fn the_cli_calls_the_api_at_the_base_url_with_the_token() {
        let resolver = ClaudeResolver {
            api_key: Some("job-token".into()),
            base_url: Some("http://orchestrator/anthropic".into()),
            model: "m".into(),
            effort: "medium".into(),
            timeout: Duration::from_secs(1),
            max_turns: 1,
        };
        let cmd = resolver
            .command(
                Path::new("."),
                "p".into(),
                Access::Edit,
                &["cargo check".into()],
            )
            .unwrap();
        assert_eq!(env(&cmd, "ANTHROPIC_API_KEY").as_deref(), Some("job-token"));
        assert_eq!(
            env(&cmd, "ANTHROPIC_BASE_URL").as_deref(),
            Some("http://orchestrator/anthropic")
        );
        assert_eq!(env(&cmd, "NO_PROXY").as_deref(), Some("orchestrator"));
        assert_eq!(env(&cmd, "no_proxy").as_deref(), Some("orchestrator"));
    }

    #[test]
    fn a_review_may_read_but_not_edit() {
        let read = ClaudeResolver::allowed_tools(Access::Read, &[]);
        assert!(read.contains("Read") && read.contains("Bash(git diff:*)"));
        assert!(!read.contains("Edit") && !read.contains("Write"));
        let edit = ClaudeResolver::allowed_tools(Access::Edit, &["cargo check".into()]);
        assert!(edit.contains("Edit") && edit.contains("Bash(cargo check)"));
    }
}
