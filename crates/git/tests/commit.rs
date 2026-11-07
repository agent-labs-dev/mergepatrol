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

#[tokio::test]
async fn a_commit_keeps_the_fix_when_verify_removes_an_untracked_file() {
    let tmp = tempfile::tempdir().unwrap();
    let wt = tmp.path();
    sh(wt, &["init", "-q", "-b", "main"]);
    std::fs::write(wt.join("a.txt"), "before\n").unwrap();
    sh(wt, &["add", "a.txt"]);
    sh(wt, &["commit", "-q", "-m", "root"]);
    let root = sh(wt, &["rev-parse", "HEAD"]);

    // The agent fixes a tracked file, adds a module, and runs checks that leave output.
    std::fs::write(wt.join("a.txt"), "fixed\n").unwrap();
    std::fs::write(wt.join("new.txt"), "the fix\n").unwrap();
    std::fs::write(wt.join("coverage.out"), "the agent's check output\n").unwrap();
    let new = Git.untracked_files(wt).await.unwrap();
    assert_eq!(new, ["coverage.out", "new.txt"]);

    // MergeLoop's verify command cleans previous output before running successfully.
    std::fs::remove_file(wt.join("coverage.out")).unwrap();
    Git.commit(wt, &new, "fix: repair a")
        .await
        .expect("a successful verify that cleans output must not prevent the fix commit");

    assert_eq!(
        Git.changed_files(wt, &root, "HEAD").await.unwrap(),
        ["a.txt", "new.txt"]
    );
    assert_eq!(sh(wt, &["show", "HEAD:a.txt"]), "fixed");
    assert_eq!(sh(wt, &["show", "HEAD:new.txt"]), "the fix");
}
