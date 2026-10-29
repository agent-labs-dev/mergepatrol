use std::path::Path;
use std::process::Command;

use mergepatrol_git::Git;

fn sh(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Ada")
        .env("GIT_AUTHOR_EMAIL", "ada@example.com")
        .env("GIT_COMMITTER_NAME", "Ada")
        .env("GIT_COMMITTER_EMAIL", "ada@example.com")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// `n` empty commits on `branch`, from where it is, or from nothing: its head.
fn commits(dir: &Path, branch: &str, n: usize) -> String {
    use std::io::Write;
    let tip = Command::new("git")
        .args(["rev-parse", "--verify", "-q", branch])
        .current_dir(dir)
        .output()
        .unwrap();
    let tip = String::from_utf8(tip.stdout).unwrap();
    let mut stream = String::new();
    for i in 0..n {
        let msg = format!("{branch} {i}");
        stream.push_str(&format!(
            "commit refs/heads/{branch}\ncommitter Ada <ada@example.com> 0 +0000\ndata {}\n{msg}\n",
            msg.len()
        ));
        if i == 0 && !tip.trim().is_empty() {
            stream.push_str(&format!("from {}\n", tip.trim()));
        }
    }
    let mut child = Command::new("git")
        .args(["fast-import", "--quiet"])
        .current_dir(dir)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stream.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
    sh(dir, &["rev-parse", branch])
}

const REFSPECS: [&str; 2] = [
    "+refs/heads/main:refs/remotes/origin/main",
    "+refs/heads/pr:refs/mergepatrol/head",
];
const TRUNK: &str = "refs/remotes/origin/main";
const HEAD: &str = "refs/mergepatrol/head";

/// A fresh repo with `origin` as its remote, fetched as a job fetches it.
async fn fetch(origin: &Path, dir: &Path) {
    let url = format!("file://{}", origin.display());
    Git.init(dir, &url).await.unwrap();
    Git.fetch_pr_side(dir, "", &REFSPECS, TRUNK, HEAD)
        .await
        .unwrap();
}

fn count(dir: &Path) -> usize {
    sh(dir, &["rev-list", "--count", TRUNK, HEAD])
        .parse()
        .unwrap()
}

#[tokio::test]
async fn a_job_fetches_back_to_the_merge_base_not_the_whole_history() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    std::fs::create_dir(&origin).unwrap();
    sh(&origin, &["init", "-q", "--bare"]);
    let base = commits(&origin, "main", 5000);
    sh(&origin, &["branch", "pr", "main"]);
    commits(&origin, "pr", 2);
    // Further than the first fetch reaches.
    commits(&origin, "main", 100);

    let dir = tmp.path().join("job");
    fetch(&origin, &dir).await;

    assert_eq!(Git.merge_base(&dir, TRUNK, HEAD).await.unwrap(), base);
    assert_eq!(sh(&dir, &["rev-parse", "--is-shallow-repository"]), "true");
    assert!(count(&dir) < 1000, "fetched {} commits", count(&dir));
}

#[tokio::test]
async fn a_branch_merged_into_the_pr_is_fetched_whole() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    std::fs::create_dir(&origin).unwrap();
    sh(&origin, &["init", "-q", "--bare"]);
    commits(&origin, "main", 5000);
    sh(&origin, &["branch", "side", "main"]);
    // Deeper than the first fetch reaches, though the merge base is right under the PR.
    let side = commits(&origin, "side", 101);
    sh(&origin, &["branch", "pr", "main"]);
    let pr = commits(&origin, "pr", 1);
    let tree = sh(&origin, &["rev-parse", "pr^{tree}"]);
    let merge = sh(
        &origin,
        &[
            "commit-tree",
            &tree,
            "-p",
            &pr,
            "-p",
            &side,
            "-m",
            "merge side",
        ],
    );
    sh(&origin, &["update-ref", "refs/heads/pr", &merge]);
    commits(&origin, "main", 10);

    let dir = tmp.path().join("job");
    fetch(&origin, &dir).await;

    let range = |dir: &Path| sh(dir, &["rev-list", "--count", "main..pr"]);
    let pr_side = sh(&dir, &["rev-list", "--count", &format!("{TRUNK}..{HEAD}")]);
    assert_eq!(pr_side, range(&origin));
    assert_eq!(sh(&dir, &["rev-parse", "--is-shallow-repository"]), "true");
}

#[tokio::test]
async fn the_pinned_commits_are_fetched_after_their_branches_moved_on() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    std::fs::create_dir(&origin).unwrap();
    sh(&origin, &["init", "-q", "--bare"]);
    let trunk = commits(&origin, "main", 5000);
    sh(&origin, &["branch", "pr", "main"]);
    let head = commits(&origin, "pr", 2);
    // Pushed after the job was given `trunk` and `head`, past the first fetch.
    commits(&origin, "pr", 100);
    commits(&origin, "main", 100);

    let dir = tmp.path().join("job");
    let url = format!("file://{}", origin.display());
    Git.init(&dir, &url).await.unwrap();
    Git.fetch_pr_side(&dir, "", &REFSPECS, &trunk, &head)
        .await
        .unwrap();

    assert_eq!(
        sh(&dir, &["rev-list", "--count", &format!("{trunk}..{head}")]),
        "2"
    );
    assert_eq!(sh(&dir, &["rev-parse", "--is-shallow-repository"]), "true");
}

#[tokio::test]
async fn unrelated_histories_are_fetched_in_full() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    std::fs::create_dir(&origin).unwrap();
    sh(&origin, &["init", "-q", "--bare"]);
    commits(&origin, "main", 100);
    commits(&origin, "pr", 100);

    let dir = tmp.path().join("job");
    fetch(&origin, &dir).await;

    assert!(Git.merge_base(&dir, TRUNK, HEAD).await.is_err());
    assert_eq!(sh(&dir, &["rev-parse", "--is-shallow-repository"]), "false");
    assert_eq!(count(&dir), 200);
}
