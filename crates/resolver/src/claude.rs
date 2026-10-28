use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use mergepatrol_executor::Executor;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::{ApiKeys, GIVE_UP, Resolution, ResolveRequest, Resolver, prompt};

/// Runs Claude Code headless (`claude -p`) in the rebase worktree.
pub struct ClaudeResolver {
    pub exec: Arc<dyn Executor>,
    pub keys: Arc<dyn ApiKeys>,
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
}

#[async_trait]
impl Resolver for ClaudeResolver {
    fn name(&self) -> String {
        format!("claude ({})", self.model)
    }

    async fn resolve(&self, req: ResolveRequest<'_>) -> Result<Resolution> {
        let key = self
            .keys
            .anthropic()
            .await?
            .context("no Anthropic API key: add one in the dashboard settings")?;
        let mut cmd = self.exec.command("claude", req.workdir);
        // `--bare` reads only ANTHROPIC_API_KEY, never a login on the machine.
        cmd.env("ANTHROPIC_API_KEY", key);
        cmd.arg("-p")
            .arg(prompt(&req))
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
            .arg(Self::allowed_tools(req.verify_commands))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().context("spawn claude")?;
        let stdout = child.stdout.take().context("claude stdout")?;
        // Drain stderr as it comes, so a full pipe never blocks the CLI.
        let mut stderr = child.stderr.take().context("claude stderr")?;
        let stderr = tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut err = String::new();
            let _ = stderr.read_to_string(&mut err).await;
            err
        });
        if let Some(dir) = req.transcript.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        let mut transcript = tokio::fs::File::create(req.transcript).await?;

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
