use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use std::path::Path;

use crate::{FixRequest, GIVE_UP, Resolution, ResolveRequest, Resolver, fix_prompt, prompt};

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
    fn allowed_tools(verify: &[String]) -> String {
        let mut tools: Vec<String> = ["Read", "Edit", "Write", "Glob", "Grep"]
            .map(String::from)
            .into();
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
        verify: &[String],
    ) -> Result<tokio::process::Command> {
        let key = self
            .api_key
            .clone()
            .context("no Anthropic API key: add one in the dashboard settings")?;
        let mut cmd = tokio::process::Command::new("claude");
        cmd.current_dir(workdir).kill_on_drop(true);
        // `--bare` reads only ANTHROPIC_API_KEY, never a login on the machine.
        cmd.env("ANTHROPIC_API_KEY", key);
        if let Some(url) = &self.base_url {
            cmd.env("ANTHROPIC_BASE_URL", url);
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
            .arg(Self::allowed_tools(verify))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Ok(cmd)
    }

    async fn run(
        &self,
        workdir: &Path,
        prompt: String,
        verify: &[String],
        transcript_path: &Path,
    ) -> Result<Resolution> {
        let mut child = self
            .command(workdir, prompt, verify)?
            .spawn()
            .context("spawn claude")?;
        let stdout = child.stdout.take().context("claude stdout")?;
        // Drain stderr as it comes, so a full pipe never blocks the CLI.
        let mut stderr = child.stderr.take().context("claude stderr")?;
        let stderr = tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut err = String::new();
            let _ = stderr.read_to_string(&mut err).await;
            err
        });
        if let Some(dir) = transcript_path.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        let mut transcript = tokio::fs::File::create(transcript_path).await?;

        let read = async {
            let mut lines = BufReader::new(stdout).lines();
            let mut turns = 0;
            let mut result = None;
            while let Some(line) = lines.next_line().await? {
                transcript.write_all(line.as_bytes()).await?;
                transcript.write_all(b"\n").await?;
                let Ok(event) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                match event["type"].as_str() {
                    Some("assistant") => {
                        turns += 1;
                        if turns > self.max_turns {
                            bail!("Claude went over {} turns", self.max_turns);
                        }
                    }
                    Some("result") => result = Some(event),
                    _ => {}
                }
            }
            anyhow::Ok((turns, result))
        };

        let (turns, result) = match tokio::time::timeout(self.timeout, read).await {
            Ok(r) => r?,
            Err(_) => bail!("Claude did not finish in {:?}", self.timeout),
        };
        let status = child.wait().await?;
        let result = match result {
            Some(r) => r,
            None => bail!(
                "claude exited ({status}) with no result: {}",
                stderr.await?.trim()
            ),
        };

        if result["is_error"].as_bool() == Some(true) {
            bail!(
                "claude: {}",
                result["result"]
                    .as_str()
                    .or(result["subtype"].as_str())
                    .unwrap_or("error")
            );
        }
        let text = result["result"].as_str().unwrap_or_default();
        if let Some(reason) = text.lines().find_map(|l| l.trim().strip_prefix(GIVE_UP)) {
            return Ok(Resolution::GaveUp(reason.trim().to_owned()));
        }
        Ok(Resolution::Resolved {
            turns,
            cost_usd: result["total_cost_usd"].as_f64(),
        })
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
            req.verify_commands,
            req.transcript,
        )
        .await
    }

    async fn fix(&self, req: FixRequest<'_>) -> Result<Resolution> {
        self.run(
            req.workdir,
            fix_prompt(&req),
            req.verify_commands,
            req.transcript,
        )
        .await
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum KeyCheck {
    Valid,
    Invalid(String),
}

/// Asks the Anthropic API whether `key` works, with the cheapest call there is: list one model.
pub async fn check_anthropic_key(key: &str) -> Result<KeyCheck> {
    let res = reqwest::Client::new()
        .get("https://api.anthropic.com/v1/models?limit=1")
        .header("x-api-key", key)
        .header("anthropic-version", "2023-06-01")
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .context("could not reach the Anthropic API")?;
    let status = res.status();
    if status.is_success() {
        return Ok(KeyCheck::Valid);
    }
    let body: Value = res.json().await.unwrap_or_default();
    let message = body["error"]["message"]
        .as_str()
        .unwrap_or("rejected")
        .to_owned();
    match status.as_u16() {
        401 | 403 => Ok(KeyCheck::Invalid(message)),
        _ => bail!("the Anthropic API answered {status}: {message}"),
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
            .command(Path::new("."), "p".into(), &["cargo check".into()])
            .unwrap();
        assert_eq!(env(&cmd, "ANTHROPIC_API_KEY").as_deref(), Some("job-token"));
        assert_eq!(
            env(&cmd, "ANTHROPIC_BASE_URL").as_deref(),
            Some("http://orchestrator/anthropic")
        );
    }
}
