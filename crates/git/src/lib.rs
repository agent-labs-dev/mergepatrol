//! Local git, through the `git` CLI. Mergepatrol never pushes: `fetch` is the only network call,
//! and the `github` crate publishes the commits.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::io::AsyncWriteExt;

/// The longest a fetch may take. A first fetch of a large repo is slow; a stalled one is cut
/// short sooner, by `http.lowSpeedTime`.
const FETCH_TIMEOUT: Duration = Duration::from_secs(20 * 60);

/// Committer of the local rebase. The published commits get GitHub's bot identity instead.
const COMMITTER: (&str, &str) = ("mergepatrol", "mergepatrol@users.noreply.github.com");

#[derive(Clone, Copy, Default)]
pub struct Git;

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

impl Git {
    /// A bare repo at `path` that caches the objects of one GitHub repo, with `url` as `origin`.
    /// Heals one that a crash left half made, or that a killed git left locked: only the repo's
    /// queue uses its cache, one git at a time, so a lock file here is stale.
    pub async fn ensure_cache(&self, path: &Path, url: &str) -> Result<()> {
        tokio::fs::create_dir_all(path).await?;
        // On an existing repo, `init` only fills in what is missing.
        self.run(path, &["init", "--bare", "-q"]).await?;
        remove_locks(path).await?;
        match self.run(path, &["remote", "get-url", "origin"]).await {
            Ok(current) if current.trim() == url => {}
            Ok(_) => {
                _ = self
                    .run(path, &["remote", "set-url", "origin", url])
                    .await?
            }
            Err(_) => _ = self.run(path, &["remote", "add", "origin", url]).await?,
        }
        Ok(())
    }

    /// Fetches `refspecs`. The token goes in through the environment, not the arguments,
    /// so it does not show in `ps`. An empty token fetches without auth.
    pub async fn fetch(&self, repo: &Path, token: &str, refspecs: &[&str]) -> Result<()> {
        self.fetch_within(repo, token, refspecs, FETCH_TIMEOUT)
            .await
    }

    async fn fetch_within(
        &self,
        repo: &Path,
        token: &str,
        refspecs: &[&str],
        timeout: Duration,
    ) -> Result<()> {
        // A connection that stalls is dropped, rather than waited on.
        let mut args = vec![
            "-c",
            "http.lowSpeedLimit=1000",
            "-c",
            "http.lowSpeedTime=60",
        ];
        args.extend(["fetch", "-q", "--no-tags", "--force", "origin"]);
        args.extend(refspecs);
        let mut cmd = self.git(repo, &args);
        authorize(&mut cmd, token);
        // Dropping the command kills git.
        tokio::time::timeout(timeout, output(cmd, "git fetch"))
            .await
            .with_context(|| format!("git fetch: no result after {}s", timeout.as_secs()))??;
        Ok(())
    }

    /// Pushes `sha` to `branch` at `url`, only while the branch is at `expected`. The token goes
    /// in as `fetch`'s does.
    pub async fn push_leased(
        &self,
        repo: &Path,
        token: &str,
        url: &str,
        sha: &str,
        branch: &str,
        expected: &str,
    ) -> Result<()> {
        let lease = format!("--force-with-lease=refs/heads/{branch}:{expected}");
        let refspec = format!("{sha}:refs/heads/{branch}");
        let mut cmd = self.git(repo, &["push", "-q", "--no-verify", &lease, url, &refspec]);
        authorize(&mut cmd, token);
        output(cmd, "git push").await?;
        Ok(())
    }

    /// Writes the commits `exclude..rev` to `bundle`, under `refs/heads/result`.
    pub async fn create_bundle(
        &self,
        repo: &Path,
        bundle: &Path,
        rev: &str,
        exclude: &str,
    ) -> Result<()> {
        self.run(repo, &["update-ref", "refs/heads/result", rev])
            .await?;
        let not = format!("^{exclude}");
        self.run(
            repo,
            &[
                "bundle",
                "create",
                "-q",
                path_str(bundle)?,
                "refs/heads/result",
                &not,
            ],
        )
        .await?;
        Ok(())
    }

    /// Imports the `refs/heads/result` of `bundle` as `into`. Returns the commit it points to.
    pub async fn import_bundle(&self, repo: &Path, bundle: &Path, into: &str) -> Result<String> {
        let refspec = format!("+refs/heads/result:{into}");
        self.run(
            repo,
            &["fetch", "-q", "--no-tags", path_str(bundle)?, &refspec],
        )
        .await?;
        self.rev_parse(repo, into).await
    }

    /// A fresh repo at `dir` with `url` as `origin`.
    pub async fn init(&self, dir: &Path, url: &str) -> Result<()> {
        tokio::fs::create_dir_all(dir).await?;
        self.run(dir, &["init", "-q"]).await?;
        self.run(dir, &["remote", "add", "origin", url]).await?;
        Ok(())
    }

    pub async fn checkout_detached(&self, repo: &Path, rev: &str) -> Result<()> {
        self.run(repo, &["checkout", "-q", "--detach", rev]).await?;
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

    /// Rebases HEAD onto `onto`: the commits from `upstream`, or those not on `onto` when
    /// `upstream` is `None`.
    pub async fn rebase(
        &self,
        wt: &Path,
        onto: &str,
        upstream: Option<&str>,
    ) -> Result<RebaseStep> {
        let args = match upstream {
            Some(upstream) => vec!["rebase", "-q", "--onto", onto, upstream],
            None => vec!["rebase", "-q", onto],
        };
        let ok = self.git(wt, &args).output().await?.status.success();
        self.rebase_step(wt, ok).await
    }

    /// Commits the resolved conflict as the commit that did not apply, with its author and
    /// message, plus `trailer`, and goes on with the next commit. A resolution that leaves
    /// nothing to commit, as when the trunk has the change already, drops the commit, as git
    /// does for a commit that applies empty.
    pub async fn continue_resolution(&self, wt: &Path, trailer: &str) -> Result<RebaseStep> {
        self.run(wt, &["add", "-A"]).await?;
        let staged = !self
            .git(wt, &["diff", "--cached", "--quiet", "HEAD"])
            .status()
            .await?
            .success();
        let next = match staged {
            true => {
                self.commit_resolution(wt, trailer).await?;
                "--continue"
            }
            false => "--skip",
        };
        let mut cmd = self.git(wt, &["rebase", next]);
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

    async fn commit_resolution(&self, wt: &Path, trailer: &str) -> Result<()> {
        let author = self
            .run(wt, &["log", "-1", "--format=%an <%ae>", "REBASE_HEAD"])
            .await?;
        let message = self
            .run(wt, &["log", "-1", "--format=%B", "REBASE_HEAD"])
            .await?;
        let message = self.add_trailer(wt, &message, trailer).await?;
        let mut cmd = self.git(wt, &["commit", "-q", "--author", author.trim(), "-F", "-"]);
        input(cmd.stdin(Stdio::piped()), message.as_bytes(), "git commit").await?;
        Ok(())
    }

    /// Whether the worktree has changes to commit, untracked files included.
    pub async fn is_dirty(&self, wt: &Path) -> Result<bool> {
        Ok(!self
            .run(wt, &["status", "--porcelain"])
            .await?
            .trim()
            .is_empty())
    }

    /// Commits every change in the worktree, untracked files included, as Mergepatrol.
    pub async fn commit_all(&self, wt: &Path, message: &str) -> Result<()> {
        self.run(wt, &["add", "-A"]).await?;
        let author = format!("{} <{}>", COMMITTER.0, COMMITTER.1);
        let mut cmd = self.git(wt, &["commit", "-q", "--author", &author, "-F", "-"]);
        input(cmd.stdin(Stdio::piped()), message.as_bytes(), "git commit").await?;
        Ok(())
    }

    pub async fn add_trailer(&self, wt: &Path, message: &str, trailer: &str) -> Result<String> {
        // Without the newline, older git (2.39 in our bookworm images) puts the trailer right under the subject.
        let message = format!("{}\n", message.trim_end());
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

    /// The objects reachable from `head` but not from `base`: commits, trees, and blobs.
    pub async fn objects(&self, wt: &Path, base: &str, head: &str) -> Result<Vec<String>> {
        let range = format!("{base}..{head}");
        Ok(lines(
            &self
                .run(wt, &["rev-list", "--objects", "--no-object-names", &range])
                .await?,
        ))
    }

    pub async fn read_blob(&self, wt: &Path, sha: &str) -> Result<Vec<u8>> {
        self.output(wt, &["cat-file", "blob", sha]).await
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("git");
        cmd.current_dir(cwd)
            .kill_on_drop(true)
            .args(args)
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

/// Sends `token` as GitHub's basic auth, through the environment. An empty token sends none.
fn authorize(cmd: &mut tokio::process::Command, token: &str) {
    use base64::Engine;
    if token.is_empty() {
        return;
    }
    let basic = base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{token}"));
    cmd.env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "http.extraHeader")
        .env(
            "GIT_CONFIG_VALUE_0",
            format!("Authorization: Basic {basic}"),
        );
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

/// Removes the lock files that a git killed mid-write left: under `refs/`, and at the top of the
/// repo, such as `packed-refs.lock` and `shallow.lock`.
async fn remove_locks(repo: &Path) -> Result<()> {
    fn walk(dir: &Path, deep: bool) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                if deep || entry.file_name() == "refs" {
                    walk(&path, true)?;
                }
            } else if path.extension().is_some_and(|e| e == "lock") {
                std::fs::remove_file(&path)?;
            }
        }
        Ok(())
    }
    let repo = repo.to_owned();
    tokio::task::spawn_blocking(move || walk(&repo, false))
        .await?
        .context("remove stale lock files")
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

    #[tokio::test]
    async fn a_fetch_that_hangs_is_cut_short() {
        // A server that accepts the connection, then says nothing.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/r.git", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let mut open = Vec::new();
            while let Ok((conn, _)) = listener.accept().await {
                open.push(conn);
            }
        });
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache.git");
        Git.ensure_cache(&cache, &url).await.unwrap();
        let start = std::time::Instant::now();
        let err = Git
            .fetch_within(
                &cache,
                "",
                &["+refs/heads/main:refs/x"],
                Duration::from_secs(1),
            )
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("no result after 1s"), "{err:#}");
        assert!(start.elapsed() < Duration::from_secs(10));
    }
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

    fn sh(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    fn commit(dir: &Path, file: &str, content: &str) -> String {
        std::fs::write(dir.join(file), content).unwrap();
        sh(dir, &["add", file]);
        sh(dir, &["commit", "-q", "-m", file]);
        sh(dir, &["rev-parse", "HEAD"])
    }

    /// What `publish` relies on to skip uploads: a rebase makes a new blob only for a file both
    /// sides changed.
    #[tokio::test]
    async fn a_rebase_reuses_the_blobs_of_the_files_only_the_pr_changed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        sh(dir, &["init", "-q", "-b", "main"]);
        commit(dir, "both.txt", "1\n2\n3\n4\n5\n");
        let base = commit(dir, "base.txt", "base\n");
        commit(dir, "pr.txt", "pr\n");
        let head = commit(dir, "both.txt", "1\n2\n3\n4\npr\n");
        sh(dir, &["checkout", "-q", "--detach", &base]);
        let trunk = commit(dir, "both.txt", "trunk\n2\n3\n4\n5\n");
        sh(dir, &["checkout", "-q", "--detach", &head]);
        assert_eq!(
            Git.rebase(dir, &trunk, None).await.unwrap(),
            RebaseStep::Done
        );

        let known: Vec<String> = Git.objects(dir, &trunk, &head).await.unwrap();
        let commits = Git.commits(dir, &trunk, "HEAD").await.unwrap();
        let new: Vec<&str> = commits
            .iter()
            .flat_map(|c| &c.changes)
            .filter(|c| !known.contains(c.sha.as_ref().unwrap()))
            .map(|c| c.path.as_str())
            .collect();
        assert_eq!(new, ["both.txt"]);
    }
}
