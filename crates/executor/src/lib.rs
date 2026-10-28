//! Runs rebase jobs. A job runs the repo's code and an agent with a shell, so it is untrusted:
//! in production each job gets its own gVisor container (`DockerExecutor`). `LocalExecutor`
//! runs the job as a subprocess, for development.

pub mod protocol;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::protocol::{JobResult, JobSpec, RESULT_FILE};

#[async_trait]
pub trait Executor: Send + Sync {
    /// Runs one job and leaves its output (`result.json`, maybe `result.bundle`) in `out`.
    async fn run(&self, job_id: &str, spec: &JobSpec, out: &Path) -> Result<JobResult>;
}

/// Runs `<program> job --out <dir>` on this machine. No isolation.
pub struct LocalExecutor {
    pub program: PathBuf,
    pub timeout: Duration,
}

#[async_trait]
impl Executor for LocalExecutor {
    async fn run(&self, _job_id: &str, spec: &JobSpec, out: &Path) -> Result<JobResult> {
        tokio::fs::create_dir_all(out).await?;
        let mut cmd = Command::new(&self.program);
        cmd.arg("job").arg("--out").arg(out);
        let logs = run_with_spec(cmd, spec, self.timeout).await?;
        read_result(out, &logs).await
    }
}

/// One container per job: `docker create`, `docker start` with the spec on stdin, then
/// `docker cp` the output out. No bind mounts, so it also works when the orchestrator itself
/// runs in a container and talks to the host's Docker.
pub struct DockerExecutor {
    pub image: String,
    /// `runsc` for gVisor. `None` uses Docker's default runtime.
    pub runtime: Option<String>,
    /// A network whose host firewall blocks the metadata server and private ranges.
    pub network: Option<String>,
    /// DNS servers for the job. Needed on the sandbox network, whose firewall blocks Docker's
    /// embedded DNS. Empty uses Docker's default.
    pub dns: Vec<String>,
    pub memory: String,
    pub cpus: String,
    pub timeout: Duration,
}

#[async_trait]
impl Executor for DockerExecutor {
    async fn run(&self, job_id: &str, spec: &JobSpec, out: &Path) -> Result<JobResult> {
        tokio::fs::create_dir_all(out).await?;
        let name = format!("mergepatrol-job-{job_id}");
        // A container left over from a crash would block the name.
        let _ = Command::new("docker")
            .args(["rm", "-f", &name])
            .output()
            .await;
        let mut create = Command::new("docker");
        create.args(["create", "-i", "--name", &name]);
        if let Some(runtime) = &self.runtime {
            create.args(["--runtime", runtime]);
        }
        if let Some(network) = &self.network {
            create.args(["--network", network]);
        }
        for server in &self.dns {
            create.args(["--dns", server]);
        }
        create
            .args([
                "--memory",
                &self.memory,
                "--cpus",
                &self.cpus,
                "--pids-limit",
                "4096",
            ])
            .args(["--cap-drop", "ALL", "--security-opt", "no-new-privileges"])
            .arg(&self.image)
            .args(["job", "--out", "/out"]);
        let id = String::from_utf8(checked(create, "docker create").await?)?
            .trim()
            .to_owned();

        let result = async {
            let mut start = Command::new("docker");
            start.args(["start", "-a", "-i", &id]);
            let logs = run_with_spec(start, spec, self.timeout).await;
            if logs.is_err() {
                let _ = Command::new("docker").args(["kill", &id]).output().await;
            }
            let logs = logs?;
            let mut cp = Command::new("docker");
            cp.args(["cp", &format!("{id}:/out/."), path_str(out)?]);
            checked(cp, "docker cp").await?;
            read_result(out, &logs).await
        }
        .await;

        if let Err(e) = Command::new("docker")
            .args(["rm", "-f", &id])
            .output()
            .await
        {
            tracing::warn!(error = %e, container = %id, "could not remove the job container");
        }
        result
    }
}

/// Runs `cmd` with `spec` as JSON on stdin. Returns the end of its output, for errors.
async fn run_with_spec(mut cmd: Command, spec: &JobSpec, timeout: Duration) -> Result<String> {
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("start the job")?;
    let mut stdin = child.stdin.take().context("job stdin")?;
    stdin.write_all(&serde_json::to_vec(spec)?).await?;
    drop(stdin);
    let out = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(out) => out?,
        Err(_) => bail!("the job did not finish in {timeout:?}"),
    };
    let mut logs = String::from_utf8_lossy(&out.stdout).into_owned();
    logs.push_str(&String::from_utf8_lossy(&out.stderr));
    let tail = tail(&logs, 40);
    if !out.status.success() {
        bail!("the job crashed ({}):\n{tail}", out.status);
    }
    Ok(tail)
}

async fn read_result(out: &Path, logs: &str) -> Result<JobResult> {
    let json = tokio::fs::read(out.join(RESULT_FILE))
        .await
        .with_context(|| format!("the job wrote no result:\n{logs}"))?;
    serde_json::from_slice(&json).context("the job wrote a bad result")
}

async fn checked(mut cmd: Command, what: &str) -> Result<Vec<u8>> {
    let out = cmd
        .stdin(Stdio::null())
        .output()
        .await
        .with_context(|| format!("{what}: spawn"))?;
    if !out.status.success() {
        bail!("{what}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(out.stdout)
}

fn tail(s: &str, lines: usize) -> String {
    let all: Vec<&str> = s.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

fn path_str(p: &Path) -> Result<&str> {
    p.to_str().context("non-UTF-8 path")
}
