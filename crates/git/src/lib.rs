//! Local git, through the `git` CLI. MergeLoop never pushes: `fetch` is the only network call,
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
const COMMITTER: (&str, &str) = ("MergeLoop", "mergepatrol@users.noreply.github.com");

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
        // First: a stale `config.lock` would fail `init` too.
        remove_locks(path).await?;
        // On an existing repo, `init` only fills in what is missing.
        self.run(path, &["init", "--bare", "-q"]).await?;
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

    /// Deletes the refs that match `patterns`, as `for-each-ref` takes them: none is fine.
    pub async fn delete_refs(&self, repo: &Path, patterns: &[&str]) -> Result<()> {
        let mut args = vec!["for-each-ref", "--format=delete %(refname)"];
        args.extend(patterns);
        let deletes = self.run(repo, &args).await?;
        if !deletes.is_empty() {
            let mut cmd = self.git(repo, &["update-ref", "--stdin"]);
            input(
                cmd.stdin(Stdio::piped()),
                deletes.as_bytes(),
                "git update-ref",
            )
            .await?;
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
        self.paths(wt, &["diff", "--name-only", "--no-renames", from, to])
            .await
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
    /// does for a commit that applies empty. Only tracked files are staged, unmerged ones among
    /// them: an untracked file is what the resolver's checks left, never part of a resolution.
    pub async fn continue_resolution(&self, wt: &Path, trailer: &str) -> Result<RebaseStep> {
        self.run(wt, &["add", "-u"]).await?;
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

    /// Whether `base..head` holds a merge commit: the branch keeps up with its base by merging.
    pub async fn has_merges(&self, wt: &Path, base: &str, head: &str) -> Result<bool> {
        let range = format!("{base}..{head}");
        let merges = self
            .run(wt, &["rev-list", "--merges", "-n1", &range])
            .await?;
        Ok(!merges.trim().is_empty())
    }

    /// Whether `a` and `b` merge with no conflict. Writes objects, and touches no worktree.
    pub async fn merges_cleanly(&self, wt: &Path, a: &str, b: &str) -> Result<bool> {
        let out = self
            .git(wt, &["merge-tree", "--write-tree", "--no-messages", a, b])
            .output()
            .await?;
        match out.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => bail!(
                "git merge-tree: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        }
    }

    /// Merges `rev` into HEAD as a merge commit with `message`, by MergeLoop. Returns the files
    /// it conflicts in, left for `commit_merge`: none once it is committed.
    pub async fn merge(&self, wt: &Path, rev: &str, message: &str) -> Result<Vec<String>> {
        let mut cmd = self.git(
            wt,
            &["merge", "-q", "--no-ff", "--no-edit", "-m", message, rev],
        );
        as_mergeloop(&mut cmd);
        let ok = cmd.output().await?.status.success();
        let files = self.unmerged_files(wt).await?;
        match (ok, files.is_empty()) {
            (true, true) => Ok(Vec::new()),
            (false, false) => Ok(files),
            _ => bail!("git merge stopped without a conflict (exit ok: {ok}, unmerged: {files:?})"),
        }
    }

    /// Commits the resolved conflict of a stopped `merge`, with its message plus `trailer`. Stages
    /// tracked files only: what the resolver's checks leave behind stays out.
    pub async fn commit_merge(&self, wt: &Path, message: &str, trailer: &str) -> Result<()> {
        self.run(wt, &["add", "-u"]).await?;
        let message = self.add_trailer(wt, message, trailer).await?;
        let mut cmd = self.git(wt, &["commit", "-q", "--no-verify", "-F", "-"]);
        as_mergeloop(&mut cmd);
        input(cmd.stdin(Stdio::piped()), message.as_bytes(), "git commit").await?;
        Ok(())
    }

    /// The parents of `rev`, the first parent first.
    pub async fn parents(&self, wt: &Path, rev: &str) -> Result<Vec<String>> {
        let line = self.run(wt, &["rev-list", "--parents", "-n1", rev]).await?;
        Ok(line.split_whitespace().skip(1).map(str::to_owned).collect())
    }

    /// What `to`'s tree changes of `from`'s, file by file.
    pub async fn tree_changes(&self, wt: &Path, from: &str, to: &str) -> Result<Vec<TreeChange>> {
        let raw = self
            .output(
                wt,
                &["diff-tree", "-r", "-z", "--raw", "--no-renames", from, to],
            )
            .await?;
        parse_raw_diff(&raw)
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
        self.paths(wt, &["diff", "--name-only", "--diff-filter=U"])
            .await
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

    /// The untracked files in the worktree that `.gitignore` does not ignore.
    pub async fn untracked_files(&self, wt: &Path) -> Result<Vec<String>> {
        self.paths(wt, &["ls-files", "--others", "--exclude-standard"])
            .await
    }

    /// Commits the changes to tracked files, and the untracked files in `new` still there, as
    /// MergeLoop.
    pub async fn commit(&self, wt: &Path, new: &[String], message: &str) -> Result<()> {
        self.run(wt, &["add", "-u"]).await?;
        let left = self.untracked_files(wt).await?;
        let new: Vec<&str> = new
            .iter()
            .filter(|f| left.contains(f))
            .map(String::as_str)
            .collect();
        if !new.is_empty() {
            let mut cmd = self.git(
                wt,
                &["add", "--pathspec-from-file=-", "--pathspec-file-nul"],
            );
            cmd.env("GIT_LITERAL_PATHSPECS", "1");
            input(
                cmd.stdin(Stdio::piped()),
                new.join("\0").as_bytes(),
                "git add",
            )
            .await?;
        }
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
            let changes = self.tree_changes(wt, &format!("{sha}^"), &sha).await?;
            commits.push(LocalCommit {
                sha,
                tree,
                message: message.trim_end().to_owned(),
                author: author.trim().to_owned(),
                changes,
            });
        }
        Ok(commits)
    }

    /// `base..head`'s commits with a line that starts with one of `trailers`, as "sha subject".
    pub async fn commits_with(
        &self,
        wt: &Path,
        base: &str,
        head: &str,
        trailers: &[&str],
    ) -> Result<Vec<String>> {
        let range = format!("{base}..{head}");
        let greps: Vec<String> = trailers.iter().map(|t| format!("--grep=^{t}")).collect();
        let mut args = vec!["log", "--format=%h %s"];
        args.extend(greps.iter().map(String::as_str));
        args.push(&range);
        Ok(lines(&self.run(wt, &args).await?))
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
        self.objects_beyond(wt, head, &[base]).await
    }

    /// The objects reachable from `head` but from none of `known`.
    pub async fn objects_beyond(
        &self,
        wt: &Path,
        head: &str,
        known: &[&str],
    ) -> Result<Vec<String>> {
        let known: Vec<String> = known.iter().map(|k| format!("^{k}")).collect();
        let mut args = vec!["rev-list", "--objects", "--no-object-names", head];
        args.extend(known.iter().map(String::as_str));
        Ok(lines(&self.run(wt, &args).await?))
    }

    /// One file's changes from `from` to `to`, with `context` lines around each hunk.
    pub async fn diff_file(
        &self,
        wt: &Path,
        from: &str,
        to: &str,
        path: &str,
        context: u32,
    ) -> Result<String> {
        let unified = format!("-U{context}");
        let out = self
            .output(
                wt,
                &["diff", "--no-renames", &unified, from, to, "--", path],
            )
            .await?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    /// A file's text at `rev`, or `None` when it has no such file.
    pub async fn show_file(&self, wt: &Path, rev: &str, path: &str) -> Result<Option<String>> {
        let spec = format!("{rev}:{path}");
        match self.output(wt, &["show", &spec]).await {
            Ok(out) => Ok(Some(String::from_utf8_lossy(&out).into_owned())),
            Err(_) => Ok(None),
        }
    }

    pub async fn read_blob(&self, wt: &Path, sha: &str) -> Result<Vec<u8>> {
        self.output(wt, &["cat-file", "blob", sha]).await
    }

    /// What the worktree changed in `files` since `rev`, new files included, as a patch that
    /// `apply` takes. It has no context lines, so that `holds` asks only after the lines it
    /// changed, not the code around them. A file that is not there is left out.
    pub async fn patch_of(&self, wt: &Path, rev: &str, files: &[String]) -> Result<String> {
        let files: Vec<&str> = files
            .iter()
            .map(String::as_str)
            .filter(|f| wt.join(f).exists())
            .collect();
        if files.is_empty() {
            return Ok(String::new());
        }
        let mut add = vec!["add", "-A", "--"];
        add.extend(&files);
        self.run(wt, &add).await?;
        let mut diff = vec![
            "diff",
            "--cached",
            "--binary",
            "--no-renames",
            "-U0",
            rev,
            "--",
        ];
        diff.extend(&files);
        self.run(wt, &diff).await
    }

    /// Puts the worktree back at `rev` as a fresh checkout has it: ignored files go too, as build
    /// output and installed packages left behind could change what a command shows.
    pub async fn reset_to(&self, wt: &Path, rev: &str) -> Result<()> {
        self.run(wt, &["reset", "-q", "--hard", rev]).await?;
        self.run(wt, &["clean", "-q", "-fdx"]).await?;
        Ok(())
    }

    /// Applies a patch from `patch_of` to the worktree.
    pub async fn apply(&self, wt: &Path, patch: &str) -> Result<()> {
        let mut cmd = self.git(wt, &["apply", "--unidiff-zero", "--whitespace=nowarn", "-"]);
        input(cmd.stdin(Stdio::piped()), patch.as_bytes(), "git apply").await?;
        Ok(())
    }

    /// Whether the worktree still holds `patch` as it was applied: its lines are all there.
    pub async fn holds(&self, wt: &Path, patch: &str) -> Result<bool> {
        let mut cmd = self.git(wt, &["apply", "--unidiff-zero", "--check", "-R", "-"]);
        Ok(
            input(cmd.stdin(Stdio::piped()), patch.as_bytes(), "git apply")
                .await
                .is_ok(),
        )
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

    /// The paths a listing of them prints, read with `-z`: as they are, where git would quote
    /// a name with unusual bytes.
    async fn paths(&self, cwd: &Path, args: &[&str]) -> Result<Vec<String>> {
        let args = [args, &["-z"]].concat();
        let raw = self.output(cwd, &args).await?;
        raw.split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .map(|p| String::from_utf8(p.to_vec()).context("non-UTF-8 path"))
            .collect()
    }
}

/// Makes MergeLoop the author of the commit `cmd` makes, as it is the committer.
fn as_mergeloop(cmd: &mut tokio::process::Command) {
    cmd.env("GIT_AUTHOR_NAME", COMMITTER.0)
        .env("GIT_AUTHOR_EMAIL", COMMITTER.1);
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
/// repo, such as `config.lock`, `packed-refs.lock` and `shallow.lock`.
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

    /// A finding's test, taken from the review's tree, goes on the head alone, and a fix that
    /// changes it no longer holds it.
    #[tokio::test]
    async fn a_patch_of_some_files_goes_back_on_the_head_and_tells_if_it_was_changed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        sh(dir, &["init", "-q", "-b", "main"]);
        let head = commit(dir, "lib.txt", "a\n");
        std::fs::write(dir.join("lib.txt"), "a\nb\n").unwrap();
        std::fs::write(dir.join("test.txt"), "t\n").unwrap();
        std::fs::write(dir.join("other.txt"), "o\n").unwrap();
        let patch = Git
            .patch_of(
                dir,
                &head,
                &["lib.txt".into(), "test.txt".into(), "gone.txt".into()],
            )
            .await
            .unwrap();
        assert!(patch.contains("+b") && patch.contains("+t") && !patch.contains("other"));

        std::fs::write(dir.join(".git/info/exclude"), "build\n").unwrap();
        std::fs::create_dir(dir.join("build")).unwrap();
        std::fs::write(dir.join("build/out"), "stale\n").unwrap();
        Git.reset_to(dir, &head).await.unwrap();
        assert!(!dir.join("test.txt").exists() && !dir.join("other.txt").exists());
        assert!(!dir.join("build").exists(), "ignored leftovers go too");
        assert!(!Git.holds(dir, &patch).await.unwrap());
        Git.apply(dir, &patch).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("test.txt")).unwrap(),
            "t\n"
        );
        assert!(Git.holds(dir, &patch).await.unwrap());
        // A fix next to the lines the patch added leaves them as they were.
        std::fs::write(dir.join("lib.txt"), "fixed\nA\nb\n").unwrap();
        assert!(Git.holds(dir, &patch).await.unwrap());
        std::fs::write(dir.join("test.txt"), "weakened\n").unwrap();
        assert!(!Git.holds(dir, &patch).await.unwrap());
        assert!(Git.patch_of(dir, &head, &[]).await.unwrap().is_empty());
    }
}
