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

fn commit(dir: &Path, content: &str, message: &str) -> String {
    std::fs::write(dir.join("a.txt"), content).unwrap();
    sh(dir, &["add", "a.txt"]);
    sh(dir, &["commit", "-q", "-m", message]);
    sh(dir, &["rev-parse", "HEAD"])
}

#[tokio::test]
async fn a_merge_resolution_leaves_untracked_check_output_out() {
    let tmp = tempfile::tempdir().unwrap();
    let wt = tmp.path();
    sh(wt, &["init", "-q", "-b", "main"]);
    let root = commit(wt, "one\n", "root");
    let trunk = commit(wt, "trunk\n", "trunk change");
    sh(wt, &["checkout", "-q", "--detach", &root]);
    commit(wt, "pr\n", "pr change");
    let message = "Merge main into pr\n\nUpdated-by: mergeloop";
    assert_eq!(Git.merge(wt, &trunk, message).await.unwrap(), ["a.txt"]);

    // The resolver edits the conflict and its checks leave an untracked coverage report.
    std::fs::write(wt.join("a.txt"), "trunk\npr\n").unwrap();
    std::fs::write(wt.join("coverage.out"), "check output\n").unwrap();
    Git.commit_merge(wt, message, "Resolved-by: mergepatrol (test)")
        .await
        .unwrap();

    // check_job_output rejects the PR if the merge includes coverage.out.
    assert_eq!(
        Git.changed_files(wt, &trunk, "HEAD").await.unwrap(),
        ["a.txt"],
        "a check artifact must not become a change outside the PR"
    );
}
