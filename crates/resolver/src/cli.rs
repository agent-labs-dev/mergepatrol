//! Runs an agent CLI that prints one JSON event per line, and keeps every line as its transcript.

use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

/// Sends `cmd`'s calls to `url` around the job's egress proxy. Claude Code sends plain HTTP
/// through `HTTPS_PROXY` too, and the proxy only lets out the hosts on its allowlist.
pub(crate) fn bypass_proxy(cmd: &mut Command, url: &str) {
    let Some(host) = reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(String::from))
    else {
        return;
    };
    let hosts = match std::env::var("NO_PROXY").or_else(|_| std::env::var("no_proxy")) {
        Ok(hosts) if !hosts.is_empty() => format!("{hosts},{host}"),
        _ => host,
    };
    cmd.env("NO_PROXY", &hosts).env("no_proxy", hosts);
}

/// How a CLI run ended, once it started.
pub(crate) enum Ended {
    /// The CLI exited by itself, with its stderr.
    Exited(ExitStatus, String),
    /// It was killed: `on_event` stopped it, or it ran out of time. Why.
    Stopped(String),
}

/// Runs `cmd`, handing each event to `on_event`, which stops the run with an error. An error
/// when the CLI could not start or be read.
pub(crate) async fn stream(
    mut cmd: Command,
    name: &str,
    timeout: Duration,
    transcript_path: &Path,
    mut on_event: impl FnMut(&Value) -> Result<()>,
) -> Result<Ended> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("spawn {name}"))?;
    let stdout = child.stdout.take().context("stdout")?;
    // Drain stderr as it comes, so a full pipe never blocks the CLI.
    let mut stderr = child.stderr.take().context("stderr")?;
    let stderr = tokio::spawn(async move {
        let mut err = String::new();
        let _ = stderr.read_to_string(&mut err).await;
        err
    });
    if let Some(dir) = transcript_path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    let mut transcript = tokio::fs::File::create(transcript_path).await?;

    // The outer error is reading the CLI, the inner one `on_event` stopping it.
    let read = async {
        let mut lines = BufReader::new(stdout).lines();
        while let Some(line) = lines.next_line().await? {
            transcript.write_all(line.as_bytes()).await?;
            transcript.write_all(b"\n").await?;
            if let Ok(event) = serde_json::from_str::<Value>(&line)
                && let Err(e) = on_event(&event)
            {
                return anyhow::Ok(Err(e));
            }
        }
        Ok(Ok(()))
    };
    match tokio::time::timeout(timeout, read).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(e))) => return Ok(Ended::Stopped(format!("{e:#}"))),
        Ok(Err(e)) => return Err(e),
        Err(_) => {
            return Ok(Ended::Stopped(format!(
                "{name} did not finish in {timeout:?}"
            )));
        }
    }
    let status = child.wait().await?;
    Ok(Ended::Exited(status, stderr.await?))
}
