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

/// A repo with one commit on `main`, to fetch from.
fn origin(dir: &Path) -> String {
    std::fs::create_dir(dir).unwrap();
    sh(dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a.txt"), "1\n").unwrap();
    sh(dir, &["add", "a.txt"]);
    sh(dir, &["commit", "-q", "-m", "one"]);
    sh(dir, &["rev-parse", "HEAD"])
}

const MAIN: &str = "+refs/heads/main:refs/remotes/origin/main";

#[tokio::test]
async fn a_cache_left_without_its_remote_heals() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    let head = origin(&src);
    // As a crash between `init` and `remote add` left it.
    let cache = tmp.path().join("cache.git");
    std::fs::create_dir(&cache).unwrap();
    sh(&cache, &["init", "-q", "--bare"]);

    let git = Git;
    let url = src.to_str().unwrap();
    git.ensure_cache(&cache, url).await.unwrap();
    git.fetch(&cache, "", &[MAIN]).await.unwrap();
    assert_eq!(sh(&cache, &["rev-parse", "refs/remotes/origin/main"]), head);
    // Once healed, it stays as it is.
    git.ensure_cache(&cache, url).await.unwrap();
    assert_eq!(sh(&cache, &["remote", "get-url", "origin"]), url);
}

#[tokio::test]
async fn a_lock_left_by_a_killed_git_is_cleared() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    origin(&src);
    let cache = tmp.path().join("cache.git");
    let git = Git;
    let url = src.to_str().unwrap();
    git.ensure_cache(&cache, url).await.unwrap();
    git.fetch(&cache, "", &[MAIN]).await.unwrap();

    std::fs::write(src.join("a.txt"), "2\n").unwrap();
    sh(&src, &["commit", "-q", "-am", "two"]);
    let head = sh(&src, &["rev-parse", "HEAD"]);
    // As a fetch killed while it moved the ref left it.
    let lock = cache.join("refs/remotes/origin/main.lock");
    std::fs::write(&lock, "").unwrap();
    std::fs::write(cache.join("packed-refs.lock"), "").unwrap();
    std::fs::write(cache.join("config.lock"), "").unwrap();
    let err = git.fetch(&cache, "", &[MAIN]).await.unwrap_err();
    assert!(format!("{err:#}").contains("lock"), "{err:#}");

    git.ensure_cache(&cache, url).await.unwrap();
    git.fetch(&cache, "", &[MAIN]).await.unwrap();
    assert_eq!(sh(&cache, &["rev-parse", "refs/remotes/origin/main"]), head);
    assert!(!cache.join("packed-refs.lock").exists());
    assert!(!cache.join("config.lock").exists());
}
