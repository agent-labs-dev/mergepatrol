//! Runs an agent CLI that prints one JSON event per line, and keeps every line as its transcript.

use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

/// Runs `cmd`, handing each event to `on_event`, which stops the run with an error. Returns the
/// CLI's exit status and its stderr.
pub(crate) async fn stream(
    mut cmd: Command,
    name: &str,
    timeout: Duration,
    transcript_path: &Path,
    mut on_event: impl FnMut(&Value) -> Result<()>,
) -> Result<(ExitStatus, String)> {
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

    let read = async {
        let mut lines = BufReader::new(stdout).lines();
        while let Some(line) = lines.next_line().await? {
            transcript.write_all(line.as_bytes()).await?;
            transcript.write_all(b"\n").await?;
            if let Ok(event) = serde_json::from_str::<Value>(&line) {
                on_event(&event)?;
            }
        }
        anyhow::Ok(())
    };
    match tokio::time::timeout(timeout, read).await {
        Ok(r) => r?,
        Err(_) => bail!("{name} did not finish in {timeout:?}"),
    }
    let status = child.wait().await?;
    Ok((status, stderr.await?))
}
