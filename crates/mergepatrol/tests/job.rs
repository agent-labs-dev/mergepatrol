//! Runs the real `mergepatrol job` binary through `LocalExecutor`, against local git repos.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use mergepatrol_executor::protocol::{BUNDLE_FILE, JobOutcome, JobSpec, ResolverSpec};
use mergepatrol_executor::{Executor, LocalExecutor};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=Ada", "-c", "user.email=ada@example.com"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn commit(dir: &Path, file: &str, content: &str) -> String {
    std::fs::write(dir.join(file), content).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-q", "-m", &format!("change {file}")]);
    git(dir, &["rev-parse", "HEAD"])
}

/// A remote with `main` and a `pr` branch. The PR changes `pr_file`, the trunk changes `trunk_file`.
fn remote(dir: &Path, pr_file: &str, trunk_file: &str) -> (String, String) {
    git(dir, &["init", "-q", "-b", "main"]);
    commit(dir, "shared.txt", "base\n");
    git(dir, &["checkout", "-q", "-b", "pr"]);
    let head = commit(dir, pr_file, "from the pr\n");
    git(dir, &["checkout", "-q", "main"]);
    let trunk = commit(dir, trunk_file, "from main\n");
    (head, trunk)
}

fn spec(url: &Path, head: &str, trunk: &str) -> JobSpec {
    JobSpec {
        repo: "o/r".into(),
        clone_url: url.to_str().unwrap().into(),
        token: String::new(),
        head_ref: "pr".into(),
        head_sha: head.into(),
        trunk: "main".into(),
        trunk_sha: trunk.into(),
        pr_title: "t".into(),
        pr_body: String::new(),
        resolver: ResolverSpec {
            model: "claude-opus-5-5".into(),
            effort: "medium".into(),
            timeout_secs: 60,
            max_turns: 5,
            anthropic_api_key: None,
        },
    }
}

fn executor() -> LocalExecutor {
    LocalExecutor {
        program: env!("CARGO_BIN_EXE_mergepatrol").into(),
        timeout: Duration::from_secs(60),
    }
}

#[tokio::test]
async fn clean_rebase_comes_back_as_a_bundle() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    std::fs::create_dir(&origin).unwrap();
    let (head, trunk) = remote(&origin, "feature.txt", "other.txt");

    let out = tmp.path().join("out");
    let result = executor()
        .run("t1", &spec(&origin, &head, &trunk), &out)
        .await
        .unwrap();
    assert_eq!(result.outcome, JobOutcome::Rebased, "{result:?}");

    // The bundle holds one commit on top of the trunk, with both changes.
    let check = tmp.path().join("check");
    std::fs::create_dir(&check).unwrap();
    git(&check, &["init", "-q"]);
    git(&check, &["fetch", "-q", origin.to_str().unwrap(), "main"]);
    git(
        &check,
        &[
            "fetch",
            "-q",
            out.join(BUNDLE_FILE).to_str().unwrap(),
            "refs/heads/result:refs/heads/result",
        ],
    );
    assert_eq!(git(&check, &["rev-parse", "result~1"]), trunk);
    assert_eq!(git(&check, &["show", "result:feature.txt"]), "from the pr");
}

#[tokio::test]
async fn conflict_without_a_key_fails_with_the_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    std::fs::create_dir(&origin).unwrap();
    let (head, trunk) = remote(&origin, "shared.txt", "shared.txt");

    let out = tmp.path().join("out");
    let result = executor()
        .run("t2", &spec(&origin, &head, &trunk), &out)
        .await
        .unwrap();
    let JobOutcome::Failed { reason } = result.outcome else {
        panic!("{result:?}")
    };
    assert!(reason.contains("no Anthropic API key"), "{reason}");
    assert_eq!(result.events[0].kind, "conflict");
    assert!(!out.join(BUNDLE_FILE).exists());
}

/// The same clean rebase, in a container from the job image, through `DockerExecutor`.
/// Needs Docker, `docker build --target job -t mergepatrol-job:latest .`, and `git daemon`.
/// `cargo test -p mergepatrol --test job -- --ignored`
#[tokio::test]
#[ignore]
async fn docker_executor_runs_a_job_in_a_container() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    std::fs::create_dir(&origin).unwrap();
    let (head, trunk) = remote(&origin, "feature.txt", "other.txt");

    // The container reaches this machine's git daemon through Docker Desktop's host name.
    // Run `git-daemon` itself, not `git daemon`, so that killing it stops the server.
    let port = std::net::TcpListener::bind("0.0.0.0:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let exec_path = git(tmp.path(), &["--exec-path"]);
    let mut daemon = Command::new(Path::new(&exec_path).join("git-daemon"))
        .args(["--export-all", "--reuseaddr", "--listen=0.0.0.0"])
        .arg(format!("--port={port}"))
        .arg(format!(
            "--base-path={}",
            tmp.path().canonicalize().unwrap().display()
        ))
        .spawn()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut spec = spec(&origin, &head, &trunk);
    spec.clone_url = format!("git://host.docker.internal:{port}/origin");
    let docker = mergepatrol_executor::DockerExecutor {
        image: "mergepatrol-job:latest".into(),
        runtime: None,
        network: None,
        dns: vec![],
        memory: "1g".into(),
        cpus: "1".into(),
        timeout: Duration::from_secs(120),
    };
    let out = tmp.path().join("out");
    let result = docker.run("test", &spec, &out).await;
    daemon.kill().unwrap();
    daemon.wait().unwrap();

    let result = result.unwrap();
    assert_eq!(result.outcome, JobOutcome::Rebased, "{result:?}");
    assert!(out.join(BUNDLE_FILE).exists());
}
