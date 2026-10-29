use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use mergepatrol_core::Provider;
use serde_json::Value;

use crate::{FixRequest, Resolution, ResolveRequest, Resolver, cli, fix_prompt, gave_up, prompt};

/// The environment variable Codex reads the API key from.
const KEY_VAR: &str = "MERGEPATROL_API_KEY";

/// Runs Codex headless (`codex exec`) in the job's worktree, for models behind the Responses API.
pub struct CodexResolver {
    /// Whose key it needs, for the error when there is none.
    pub provider: Provider,
    /// An API key, or a job's token for the proxy at `base_url`.
    pub api_key: Option<String>,
    /// The Responses API's base URL, up to and including `/v1`.
    pub base_url: String,
    pub model: String,
    pub effort: String,
    pub timeout: Duration,
    /// Codex has no turn limit, so we count the agent's messages, commands and edits.
    pub max_turns: u32,
}

impl CodexResolver {
    fn command(&self, workdir: &Path, prompt: String) -> Result<tokio::process::Command> {
        let key = self.api_key.clone().with_context(|| {
            format!(
                "no {} API key: add one in the dashboard settings",
                self.provider.name()
            )
        })?;
        let toml = |v: &str| format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""));
        let mut cmd = tokio::process::Command::new("codex");
        cmd.current_dir(workdir).env(KEY_VAR, key);
        cmd.args(["exec", "--json", "--ephemeral", "--ignore-user-config"])
            // The job's container is the sandbox, and Codex's own does not run inside gVisor.
            // Nothing asks for approval: there is nobody to answer.
            .arg("--dangerously-bypass-approvals-and-sandbox")
            .args(["--model", &self.model])
            .args([
                "-c",
                &format!("model_reasoning_effort={}", toml(&self.effort)),
            ])
            .args(["-c", "model_provider=\"mergepatrol\""])
            .args(["-c", "model_providers.mergepatrol.name=\"Mergepatrol\""])
            .args([
                "-c",
                &format!(
                    "model_providers.mergepatrol.base_url={}",
                    toml(&self.base_url)
                ),
            ])
            .args([
                "-c",
                &format!("model_providers.mergepatrol.env_key=\"{KEY_VAR}\""),
            ])
            .args(["-c", "model_providers.mergepatrol.wire_api=\"responses\""])
            .arg(prompt);
        Ok(cmd)
    }

    async fn run(&self, workdir: &Path, prompt: String, transcript: &Path) -> Result<Resolution> {
        let cmd = self.command(workdir, prompt)?;
        let mut run = Run::default();
        let (status, stderr) = cli::stream(cmd, "codex", self.timeout, transcript, |event| {
            run.on(event, self.max_turns)
        })
        .await?;
        run.outcome(status, &stderr)
    }
}

/// What a `codex exec --json` run has said so far.
#[derive(Default)]
struct Run {
    turns: u32,
    /// The agent's last message: its answer, once the run is over.
    last_message: String,
    completed: bool,
    failed: Option<String>,
}

impl Run {
    fn on(&mut self, event: &Value, max_turns: u32) -> Result<()> {
        match event["type"].as_str() {
            Some("item.completed") => {
                let item = &event["item"];
                match item["type"].as_str() {
                    // Not the agent acting: its thinking, and Codex's own warnings.
                    Some("reasoning" | "error" | "todo_list") | None => return Ok(()),
                    Some("agent_message") => {
                        self.last_message = item["text"].as_str().unwrap_or_default().to_owned();
                    }
                    Some(_) => {}
                }
                self.turns += 1;
                if self.turns > max_turns {
                    bail!("Codex went over {max_turns} turns");
                }
            }
            Some("turn.completed") => self.completed = true,
            Some("turn.failed") => {
                self.failed = Some(
                    event["error"]["message"]
                        .as_str()
                        .unwrap_or("the turn failed")
                        .to_owned(),
                );
            }
            _ => {}
        }
        Ok(())
    }

    fn outcome(self, status: std::process::ExitStatus, stderr: &str) -> Result<Resolution> {
        if let Some(why) = self.failed {
            bail!("codex: {why}");
        }
        if !self.completed {
            bail!("codex exited ({status}) with no result: {}", stderr.trim());
        }
        if let Some(reason) = gave_up(&self.last_message) {
            return Ok(Resolution::GaveUp(reason));
        }
        Ok(Resolution::Resolved {
            turns: self.turns,
            // Codex reports tokens, not what they cost.
            cost_usd: None,
        })
    }
}

#[async_trait]
impl Resolver for CodexResolver {
    fn name(&self) -> String {
        format!("codex ({})", self.model)
    }

    async fn resolve(&self, req: ResolveRequest<'_>) -> Result<Resolution> {
        self.run(req.workdir, prompt(&req), req.transcript).await
    }

    async fn fix(&self, req: FixRequest<'_>) -> Result<Resolution> {
        self.run(req.workdir, fix_prompt(&req), req.transcript)
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt;

    use super::*;
    use serde_json::json;

    fn resolver() -> CodexResolver {
        CodexResolver {
            provider: Provider::OpenAi,
            api_key: Some("job-token".into()),
            base_url: "http://orchestrator/openai/v1".into(),
            model: "gpt-6.1-sol".into(),
            effort: "high".into(),
            timeout: Duration::from_secs(1),
            max_turns: 3,
        }
    }

    #[test]
    fn codex_calls_the_api_at_the_base_url_with_the_token() {
        let cmd = resolver().command(Path::new("."), "p".into()).unwrap();
        let cmd = cmd.as_std();
        let env = cmd
            .get_envs()
            .find(|(k, _)| *k == KEY_VAR)
            .and_then(|(_, v)| v);
        assert_eq!(env, Some("job-token".as_ref()));
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        for arg in [
            "model_providers.mergepatrol.base_url=\"http://orchestrator/openai/v1\"",
            "model_providers.mergepatrol.env_key=\"MERGEPATROL_API_KEY\"",
            "model_reasoning_effort=\"high\"",
            "gpt-6.1-sol",
        ] {
            assert!(args.iter().any(|a| a == arg), "{arg} in {args:?}");
        }
        assert_eq!(args.last().map(String::as_str), Some("p"));
    }

    #[test]
    fn no_key_says_whose() {
        let mut r = resolver();
        r.api_key = None;
        let err = r.command(Path::new("."), "p".into()).unwrap_err();
        assert!(err.to_string().contains("no OpenAI API key"), "{err}");
    }

    fn item(kind: &str, text: &str) -> Value {
        json!({ "type": "item.completed", "item": { "type": kind, "text": text } })
    }

    fn ok() -> std::process::ExitStatus {
        std::process::ExitStatus::from_raw(0)
    }

    #[test]
    fn a_completed_run_counts_what_the_agent_did() {
        let mut run = Run::default();
        for e in [
            item("error", "Model metadata not found"),
            item("reasoning", ""),
            item("command_execution", ""),
            item("file_change", ""),
            item("agent_message", "Done."),
            json!({ "type": "turn.completed", "usage": {} }),
        ] {
            run.on(&e, 10).unwrap();
        }
        match run.outcome(ok(), "").unwrap() {
            Resolution::Resolved { turns, cost_usd } => {
                assert_eq!(turns, 3);
                assert_eq!(cost_usd, None);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_last_message_can_give_up() {
        let mut run = Run::default();
        run.on(&item("agent_message", "Looking."), 10).unwrap();
        run.on(
            &item("agent_message", "MERGEPATROL_GIVE_UP: both sides rename it"),
            10,
        )
        .unwrap();
        run.on(&json!({ "type": "turn.completed" }), 10).unwrap();
        match run.outcome(ok(), "").unwrap() {
            Resolution::GaveUp(reason) => assert_eq!(reason, "both sides rename it"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn too_many_turns_stop_the_run() {
        let mut run = Run::default();
        for _ in 0..3 {
            run.on(&item("command_execution", ""), 3).unwrap();
        }
        assert!(run.on(&item("command_execution", ""), 3).is_err());
    }

    #[test]
    fn a_failed_or_unfinished_turn_is_an_error() {
        let mut run = Run::default();
        run.on(
            &json!({ "type": "turn.failed", "error": { "message": "401 Unauthorized" } }),
            10,
        )
        .unwrap();
        let err = run.outcome(ok(), "").unwrap_err();
        assert!(err.to_string().contains("401 Unauthorized"), "{err}");

        let err = Run::default().outcome(ok(), "boom").unwrap_err();
        assert!(err.to_string().contains("no result: boom"), "{err}");
    }
}
