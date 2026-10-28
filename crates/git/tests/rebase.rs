use std::path::Path;
use std::process::Command;

use mergepatrol_executor::LocalExecutor;
use mergepatrol_git::{Git, RebaseStep};

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

fn commit(dir: &Path, file: &str, content: &str, msg: &str) -> String {
    std::fs::write(dir.join(file), content).unwrap();
    sh(dir, &["add", file]);
    sh(dir, &["commit", "-q", "-m", msg]);
    sh(dir, &["rev-parse", "HEAD"])
}

#[tokio::test]
async fn conflict_is_resolved_committed_and_listed_with_its_trailer() {
    let tmp = tempfile::tempdir().unwrap();
    let wt = tmp.path();
    sh(wt, &["init", "-q", "-b", "main"]);
    let root = commit(wt, "a.txt", "one\n", "root");
    let trunk = commit(wt, "a.txt", "trunk\n", "trunk change");
    sh(wt, &["checkout", "-q", "--detach", &root]);
    commit(wt, "a.txt", "pr\n", "pr change");
    commit(wt, "b.txt", "new\n", "pr adds b");

    let exec = LocalExecutor::new(tmp.path());
    let git = Git::new(&exec);

    let RebaseStep::Conflict { files, .. } = git.rebase(wt, &trunk).await.unwrap() else {
        panic!("expected a conflict");
    };
    assert_eq!(files, ["a.txt"]);

    // What the resolver does.
    std::fs::write(wt.join("a.txt"), "trunk\npr\n").unwrap();
    git.commit_resolution(wt, "Resolved-by: mergepatrol (test)")
        .await
        .unwrap();
    assert_eq!(git.rebase_continue(wt).await.unwrap(), RebaseStep::Done);

    let commits = git.commits(wt, &trunk, "HEAD").await.unwrap();
    assert_eq!(commits.len(), 2);
    assert!(
        commits[0]
            .message
            .ends_with("Resolved-by: mergepatrol (test)"),
        "{}",
        commits[0].message
    );
    assert_eq!(commits[0].author, "Ada <ada@example.com>");
    assert_eq!(commits[1].changes.len(), 1);
    assert_eq!(commits[1].changes[0].path, "b.txt");
}
