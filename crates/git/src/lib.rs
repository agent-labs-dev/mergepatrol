//! Local git, through the `git` CLI. Mergepatrol never pushes: `fetch` is the only network call,
//! and the `github` crate publishes the commits.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use mergepatrol_executor::Executor;
use tokio::io::AsyncWriteExt;

/// Committer of the local rebase. The published commits get GitHub's bot identity instead.
const COMMITTER: (&str, &str) = ("mergepatrol", "mergepatrol@users.noreply.github.com");

pub struct Git<'a> {
    exec: &'a dyn Executor,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RebaseStep {
    Done,
    /// The rebase stopped. `commit` is the original commit that did not apply.
    Conflict {
        commit: String,
        files: Vec<String>,
    },
}

#[derive(Debug, Clone)]
pub struct LocalCommit {
    pub sha: String,
    pub tree: String,
    pub message: String,
    pub author: String,
    pub changes: Vec<TreeChange>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeChange {
    pub path: String,
    pub mode: String,
    /// `None` deletes the path.
    pub sha: Option<String>,
}

impl TreeChange {
    pub fn is_submodule(&self) -> bool {
        self.mode == "160000"
    }
}

impl<'a> Git<'a> {
    pub fn new(exec: &'a dyn Executor) -> Self {
        Self { exec }
    }

    /// A bare repo at `path` that caches the objects of one GitHub repo.
    pub async fn ensure_cache(&self, path: &Path, url: &str) -> Result<()> {
        if path.join("HEAD").exists() {
            return Ok(());
        }
        tokio::fs::create_dir_all(path).await?;
        self.run(path, &["init", "--bare", "-q"]).await?;
        self.run(path, &["remote", "add", "origin", url]).await?;
        Ok(())
    }

    /// Fetches `refspecs`. The token goes in through the environment, not the arguments,
    /// so it does not show in `ps`.
    pub async fn fetch(&self, cache: &Path, token: &str, refspecs: &[&str]) -> Result<()> {
        use base64::Engine;
        let basic =
            base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{token}"));
        let mut args = vec!["fetch", "-q", "--no-tags", "--force", "origin"];
        args.extend(refspecs);
        let mut cmd = self.git(cache, &args);
        cmd.env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "http.extraHeader")
            .env(
                "GIT_CONFIG_VALUE_0",
                format!("Authorization: Basic {basic}"),
            );
        output(cmd, "git fetch").await?;
        Ok(())
    }

    pub async fn add_worktree(&self, cache: &Path, dir: &Path, rev: &str) -> Result<()> {
        let _ = self.remove_worktree(cache, dir).await;
        self.run(
            cache,
            &["worktree", "add", "-q", "--detach", path_str(dir)?, rev],
        )
        .await?;
        Ok(())
    }

    pub async fn remove_worktree(&self, cache: &Path, dir: &Path) -> Result<()> {
        self.run(cache, &["worktree", "remove", "--force", path_str(dir)?])
            .await?;
        Ok(())
    }

    pub async fn rev_parse(&self, wt: &Path, rev: &str) -> Result<String> {
        Ok(self
            .run(wt, &["rev-parse", "--verify", rev])
            .await?
            .trim()
            .to_owned())
    }

    pub async fn is_ancestor(&self, wt: &Path, ancestor: &str, of: &str) -> Result<bool> {
        let status = self
            .git(wt, &["merge-base", "--is-ancestor", ancestor, of])
            .output()
            .await?
            .status;
        Ok(status.success())
    }

    pub async fn merge_base(&self, wt: &Path, a: &str, b: &str) -> Result<String> {
        Ok(self.run(wt, &["merge-base", a, b]).await?.trim().to_owned())
    }

    pub async fn changed_files(&self, wt: &Path, from: &str, to: &str) -> Result<Vec<String>> {
        Ok(lines(
            &self
                .run(wt, &["diff", "--name-only", "--no-renames", from, to])
                .await?,
        ))
    }

    pub async fn rebase(&self, wt: &Path, onto: &str) -> Result<RebaseStep> {
        let ok = self
            .git(wt, &["rebase", "-q", onto])
            .output()
            .await?
            .status
            .success();
        self.rebase_step(wt, ok).await
    }

    pub async fn rebase_continue(&self, wt: &Path) -> Result<RebaseStep> {
        let mut cmd = self.git(wt, &["rebase", "--continue"]);
        cmd.env("GIT_EDITOR", "true");
        let ok = cmd.output().await?.status.success();
        self.rebase_step(wt, ok).await
    }

    pub async fn rebase_abort(&self, wt: &Path) -> Result<()> {
        self.run(wt, &["rebase", "--abort"]).await?;
        Ok(())
    }

    async fn rebase_step(&self, wt: &Path, ok: bool) -> Result<RebaseStep> {
        let files = self.unmerged_files(wt).await?;
        match (ok, files.is_empty()) {
            (true, true) => Ok(RebaseStep::Done),
            (false, false) => {
                let commit = self.rev_parse(wt, "REBASE_HEAD").await?;
                Ok(RebaseStep::Conflict { commit, files })
            }
            _ => {
                bail!("git rebase stopped without a conflict (exit ok: {ok}, unmerged: {files:?})")
            }
        }
    }

    pub async fn unmerged_files(&self, wt: &Path) -> Result<Vec<String>> {
        Ok(lines(
            &self
                .run(wt, &["diff", "--name-only", "--diff-filter=U"])
                .await?,
        ))
    }

    /// Commits the resolved conflict as the commit that did not apply, with its author and
    /// message, plus `trailer`. `rebase_continue` then goes on with the next commit.
    pub async fn commit_resolution(&self, wt: &Path, trailer: &str) -> Result<()> {
        let author = self
            .run(wt, &["log", "-1", "--format=%an <%ae>", "REBASE_HEAD"])
            .await?;
        let message = self
            .run(wt, &["log", "-1", "--format=%B", "REBASE_HEAD"])
            .await?;
        let message = self.add_trailer(wt, &message, trailer).await?;
        self.run(wt, &["add", "-A"]).await?;
        let mut cmd = self.git(
            wt,
            &[
                "commit",
                "-q",
                "--allow-empty",
                "--author",
                author.trim(),
                "-F",
                "-",
            ],
        );
        input(cmd.stdin(Stdio::piped()), message.as_bytes(), "git commit").await?;
        Ok(())
    }

    pub async fn add_trailer(&self, wt: &Path, message: &str, trailer: &str) -> Result<String> {
        let mut cmd = self.git(
            wt,
            &[
                "interpret-trailers",
                "--if-exists",
                "addIfDifferent",
                "--trailer",
                trailer,
            ],
        );
        let out = input(
            cmd.stdin(Stdio::piped()),
            message.as_bytes(),
            "git interpret-trailers",
        )
        .await?;
        Ok(String::from_utf8(out)?)
    }

    /// The commits in `base..head`, oldest first, with the tree changes of each.
    pub async fn commits(&self, wt: &Path, base: &str, head: &str) -> Result<Vec<LocalCommit>> {
        let range = format!("{base}..{head}");
        let shas = lines(
            &self
                .run(wt, &["rev-list", "--reverse", "--first-parent", &range])
                .await?,
        );
        let mut commits = Vec::with_capacity(shas.len());
        for sha in shas {
            let tree = self.rev_parse(wt, &format!("{sha}^{{tree}}")).await?;
            let message = self.run(wt, &["log", "-1", "--format=%B", &sha]).await?;
            let author = self
                .run(wt, &["log", "-1", "--format=%an <%ae>", &sha])
                .await?;
            let raw = self
                .output(
                    wt,
                    &[
                        "diff-tree",
                        "-r",
                        "-z",
                        "--raw",
                        "--no-renames",
                        &format!("{sha}^"),
                        &sha,
                    ],
                )
                .await?;
            commits.push(LocalCommit {
                sha,
                tree,
                message: message.trim_end().to_owned(),
                author: author.trim().to_owned(),
                changes: parse_raw_diff(&raw)?,
            });
        }
        Ok(commits)
    }

    pub async fn commit_message(&self, wt: &Path, rev: &str) -> Result<String> {
        Ok(self
            .run(wt, &["log", "-1", "--format=%B", rev])
            .await?
            .trim_end()
            .to_owned())
    }

    /// The content of `path` at `rev`, or `None` if it does not exist there.
    pub async fn file_at(&self, wt: &Path, rev: &str, path: &str) -> Result<Option<String>> {
        let out = self
            .git(wt, &["show", &format!("{rev}:{path}")])
            .output()
            .await?;
        Ok(out
            .status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned()))
    }

    pub async fn read_blob(&self, wt: &Path, sha: &str) -> Result<Vec<u8>> {
        self.output(wt, &["cat-file", "blob", sha]).await
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> tokio::process::Command {
        let mut cmd = self.exec.command("git", cwd);
        cmd.args(args)
            .env("GIT_COMMITTER_NAME", COMMITTER.0)
            .env("GIT_COMMITTER_EMAIL", COMMITTER.1)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null());
        cmd
    }

    async fn output(&self, cwd: &Path, args: &[&str]) -> Result<Vec<u8>> {
        output(self.git(cwd, args), &format!("git {}", args.join(" "))).await
    }

    async fn run(&self, cwd: &Path, args: &[&str]) -> Result<String> {
        Ok(String::from_utf8(self.output(cwd, args).await?)?)
    }
}

async fn output(mut cmd: tokio::process::Command, what: &str) -> Result<Vec<u8>> {
    let out = cmd
        .output()
        .await
        .with_context(|| format!("{what}: spawn"))?;
    if !out.status.success() {
        bail!("{what}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(out.stdout)
}

async fn input(cmd: &mut tokio::process::Command, stdin: &[u8], what: &str) -> Result<Vec<u8>> {
    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    child
        .stdin
        .take()
        .context("stdin")?
        .write_all(stdin)
        .await?;
    let out = child.wait_with_output().await?;
    if !out.status.success() {
        bail!("{what}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(out.stdout)
}

fn lines(s: &str) -> Vec<String> {
    s.lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

fn path_str(p: &Path) -> Result<&str> {
    p.to_str().context("non-UTF-8 path")
}

/// Parses `git diff-tree -r -z --raw`: `:old_mode new_mode old_sha new_sha status\0path\0`.
fn parse_raw_diff(raw: &[u8]) -> Result<Vec<TreeChange>> {
    let mut fields = raw.split(|b| *b == 0).filter(|f| !f.is_empty());
    let mut changes = Vec::new();
    while let Some(meta) = fields.next() {
        let meta = std::str::from_utf8(meta)?;
        let path = String::from_utf8(fields.next().context("diff-tree: missing path")?.to_vec())?;
        let parts: Vec<&str> = meta.trim_start_matches(':').split(' ').collect();
        let [_, new_mode, _, new_sha, status] = parts[..] else {
            bail!("diff-tree: bad line {meta:?}");
        };
        changes.push(TreeChange {
            path,
            mode: new_mode.to_owned(),
            sha: (status != "D").then(|| new_sha.to_owned()),
        });
    }
    Ok(changes)
}

pub fn cache_path(data_dir: &Path, owner: &str, name: &str) -> PathBuf {
    data_dir
        .join("repos")
        .join(owner)
        .join(format!("{name}.git"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_raw_diff() {
        let z = "0000000000000000000000000000000000000000";
        let a = "1111111111111111111111111111111111111111";
        let raw = format!(
            ":100644 100644 {a} {a} M\0src/a.rs\0:100644 000000 {a} {z} D\0old.txt\0:000000 100755 {z} {a} A\0run.sh\0"
        );
        let changes = parse_raw_diff(raw.as_bytes()).unwrap();
        assert_eq!(changes.len(), 3);
        assert_eq!(
            changes[1],
            TreeChange {
                path: "old.txt".into(),
                mode: "000000".into(),
                sha: None
            }
        );
        assert_eq!(changes[2].mode, "100755");
    }
}
