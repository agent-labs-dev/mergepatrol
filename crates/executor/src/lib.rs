//! Where a job's commands run. `LocalExecutor` runs them on this machine.
//! A sandboxed executor (a container per job) replaces it for the hosted version.

use std::path::{Path, PathBuf};

use tokio::process::Command;

pub trait Executor: Send + Sync {
    /// The directory that holds the workspace of one job.
    fn workspace(&self, job_id: &str) -> PathBuf;

    /// A command for `program`, set to run in `cwd`. The caller adds the arguments.
    fn command(&self, program: &str, cwd: &Path) -> Command;
}

pub struct LocalExecutor {
    root: PathBuf,
}

impl LocalExecutor {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

impl Executor for LocalExecutor {
    fn workspace(&self, job_id: &str) -> PathBuf {
        self.root.join(job_id)
    }

    fn command(&self, program: &str, cwd: &Path) -> Command {
        let mut cmd = Command::new(program);
        cmd.current_dir(cwd).kill_on_drop(true);
        cmd
    }
}
