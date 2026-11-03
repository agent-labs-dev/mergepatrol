//! Runs the real `mergepatrol job` binary through `LocalExecutor`, against local git repos.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use mergepatrol_core::{CheckFailure, Provider};
use mergepatrol_executor::protocol::{
    ANSWER_FILE, ApiAccess, BUNDLE_FILE, JobOutcome, JobSpec, REVIEW_FILE, ResolverSpec,
    ReviewOutput, Task,
};
use mergepatrol_executor::{Executor, LocalExecutor};

/// Where the spec says the workspace adds a key.
const SETTINGS: &str = "https://mergeloop.test/acme/settings";

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
        head_ref: "refs/heads/pr".into(),
        head_sha: head.into(),
        trunk: "main".into(),
        trunk_sha: trunk.into(),
        base_ref: None,
        upstream: None,
        pr_title: "t".into(),
        pr_body: String::new(),
        verify: Vec::new(),
        leave_conflicts: false,
        resolver: ResolverSpec {
            provider: Default::default(),
            model: "claude-opus-5-5".into(),
            effort: "medium".into(),
            timeout_secs: 60,
            max_turns: 5,
            api: None,
            settings_url: Some(SETTINGS.into()),
        },
        task: Default::default(),
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
    assert_eq!(result.outcome, JobOutcome::Done, "{result:?}");

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
async fn a_stack_layer_replays_only_its_own_commits_onto_the_layer_below() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    std::fs::create_dir(&origin).unwrap();
    git(&origin, &["init", "-q", "-b", "main"]);
    commit(&origin, "shared.txt", "base\n");
    git(&origin, &["checkout", "-q", "-b", "l1"]);
    let l1 = commit(&origin, "l1.txt", "layer 1\n");
    git(&origin, &["checkout", "-q", "-b", "l2"]);
    let l2 = commit(&origin, "l2.txt", "layer 2\n");
    // The layer below, rebased onto a trunk that moved: other commits than `l1`.
    git(&origin, &["checkout", "-q", "main"]);
    commit(&origin, "other.txt", "from main\n");
    git(&origin, &["checkout", "-q", "-b", "l1-rebased"]);
    let l1_rebased = commit(&origin, "l1.txt", "layer 1\n");

    let spec = JobSpec {
        head_ref: "refs/heads/l2".into(),
        base_ref: Some("l1-rebased".into()),
        upstream: Some(l1),
        ..spec(&origin, &l2, &l1_rebased)
    };
    let out = tmp.path().join("out");
    let result = executor().run("t-stack", &spec, &out).await.unwrap();
    assert_eq!(result.outcome, JobOutcome::Done, "{result:?}");

    let check = tmp.path().join("check");
    std::fs::create_dir(&check).unwrap();
    git(&check, &["init", "-q"]);
    git(
        &check,
        &["fetch", "-q", origin.to_str().unwrap(), "l1-rebased"],
    );
    git(
        &check,
        &[
            "fetch",
            "-q",
            out.join(BUNDLE_FILE).to_str().unwrap(),
            "refs/heads/result:refs/heads/result",
        ],
    );
    // One commit, layer 2's, on the rebased layer 1.
    assert_eq!(git(&check, &["rev-parse", "result~1"]), l1_rebased);
    assert_eq!(git(&check, &["show", "result:l2.txt"]), "layer 2");
}

#[tokio::test]
async fn a_rebase_that_leaves_nothing_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    std::fs::create_dir(&origin).unwrap();
    let (_, trunk) = remote(&origin, "feature.txt", "other.txt");

    let out = tmp.path().join("out");
    let result = executor()
        .run("t5", &spec(&origin, &trunk, &trunk), &out)
        .await
        .unwrap();
    let JobOutcome::Failed { reason } = result.outcome else {
        panic!("{result:?}")
    };
    assert!(
        reason.contains("its changes are already on main"),
        "{reason}"
    );
}

#[tokio::test]
async fn a_conflict_left_for_off_the_lane_stops_with_no_agent() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    std::fs::create_dir(&origin).unwrap();
    let (head, trunk) = remote(&origin, "shared.txt", "shared.txt");

    let mut spec = spec(&origin, &head, &trunk);
    spec.leave_conflicts = true;
    let result = executor()
        .run("t8", &spec, &tmp.path().join("out"))
        .await
        .unwrap();
    let JobOutcome::Failed { reason } = result.outcome else {
        panic!("{result:?}")
    };
    assert!(reason.starts_with("it conflicts in shared.txt"), "{reason}");
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
    // Whose key it needs, and where to add it.
    assert!(reason.contains("no Anthropic API key"), "{reason}");
    assert!(reason.contains(SETTINGS), "{reason}");
    assert_eq!(result.events[0].kind, "conflict");
    assert!(!out.join(BUNDLE_FILE).exists());
}

#[tokio::test]
async fn fix_without_a_key_fails_with_the_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    std::fs::create_dir(&origin).unwrap();
    let (head, trunk) = remote(&origin, "feature.txt", "other.txt");

    let mut spec = spec(&origin, &head, &trunk);
    spec.task = Task::FixChecks {
        failures: vec![CheckFailure {
            name: "ci".into(),
            details: "it broke".into(),
        }],
    };
    let out = tmp.path().join("out");
    let result = executor().run("t3", &spec, &out).await.unwrap();
    let JobOutcome::Failed { reason } = result.outcome else {
        panic!("{result:?}")
    };
    assert!(reason.contains("no Anthropic API key"), "{reason}");
    assert!(!out.join(BUNDLE_FILE).exists());
}

#[tokio::test]
async fn an_openai_fix_runs_codex_and_needs_its_key() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    std::fs::create_dir(&origin).unwrap();
    let (head, trunk) = remote(&origin, "feature.txt", "other.txt");

    let mut spec = spec(&origin, &head, &trunk);
    spec.resolver.provider = Provider::OpenAi;
    spec.resolver.model = "gpt-6.1-sol".into();
    spec.task = Task::FixChecks {
        failures: vec![CheckFailure {
            name: "ci".into(),
            details: "it broke".into(),
        }],
    };
    let out = tmp.path().join("out");
    let result = executor().run("t4", &spec, &out).await.unwrap();
    let JobOutcome::Failed { reason } = result.outcome else {
        panic!("{result:?}")
    };
    // Codex says whose key is missing; the Claude CLI would ask for Anthropic's.
    assert!(reason.contains("no OpenAI API key"), "{reason}");
    assert!(!out.join(BUNDLE_FILE).exists());
}

/// An executor whose job finds, on its `PATH`, a `claude` that takes one turn, says `answer`
/// and that the run cost $0.12, and changes nothing.
fn with_fake_claude(dir: &Path, answer: &str) -> LocalExecutor {
    with_fake_claude_running(dir, "", answer)
}

/// `with_fake_claude`, with a `claude` that first runs `shell` in the worktree.
fn with_fake_claude_running(dir: &Path, shell: &str, answer: &str) -> LocalExecutor {
    let bin = dir.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let events = [
        serde_json::json!({ "type": "assistant" }),
        serde_json::json!({ "type": "result", "result": answer, "total_cost_usd": 0.12 }),
    ];
    let lines: String = events.iter().map(|e| format!("echo '{e}'\n")).collect();
    let lines = format!("{shell}\n{lines}");
    let script = |path: &Path, body: &str| {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    script(&bin.join("claude"), &lines);
    let job = dir.join("job.sh");
    script(
        &job,
        &format!(
            "PATH={}:$PATH exec {} \"$@\"\n",
            bin.display(),
            env!("CARGO_BIN_EXE_mergepatrol")
        ),
    );
    LocalExecutor {
        program: job,
        timeout: Duration::from_secs(60),
    }
}

#[tokio::test]
async fn a_fix_that_fails_still_tells_what_it_spent() {
    for (answer, why) in [
        (
            "MERGEPATROL_GIVE_UP: a flaky test",
            "the agent gave up: a flaky test",
        ),
        ("Fixed.", "the agent changed nothing"),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let origin = tmp.path().join("origin");
        std::fs::create_dir(&origin).unwrap();
        let (head, trunk) = remote(&origin, "feature.txt", "other.txt");

        let mut spec = spec(&origin, &head, &trunk);
        spec.resolver.api = Some(ApiAccess {
            base_url: "http://127.0.0.1:9".into(),
            token: "job-token".into(),
        });
        spec.task = Task::FixChecks {
            failures: vec![CheckFailure {
                name: "ci".into(),
                details: "it broke".into(),
            }],
        };
        let out = tmp.path().join("out");
        let result = with_fake_claude(tmp.path(), answer)
            .run("t5", &spec, &out)
            .await
            .unwrap();
        let JobOutcome::Failed { reason } = &result.outcome else {
            panic!("{result:?}")
        };
        assert!(reason.contains(why), "{reason}");
        let spent = result.events.last().unwrap();
        assert_eq!(spent.kind, "unfixed", "{result:?}");
        assert_eq!(
            spent.message,
            "1 turns, $0.12, by claude-opus-5-5 at medium effort"
        );
    }
}

#[tokio::test]
async fn a_rebase_agent_with_a_shell_may_regenerate_but_not_move_head() {
    let run = |shell: &'static str| async move {
        let tmp = tempfile::tempdir().unwrap();
        let origin = tmp.path().join("origin");
        std::fs::create_dir(&origin).unwrap();
        let (head, trunk) = remote(&origin, "shared.txt", "shared.txt");
        let mut spec = spec(&origin, &head, &trunk);
        spec.resolver.api = Some(ApiAccess {
            base_url: "http://127.0.0.1:9".into(),
            token: "job-token".into(),
        });
        let out = tmp.path().join("out");
        let result = with_fake_claude_running(tmp.path(), shell, "Resolved.")
            .run("t9", &spec, &out)
            .await
            .unwrap();
        let resolved = result.events.iter().any(|e| e.kind == "resolved");
        (result.outcome, out.join(BUNDLE_FILE).exists(), resolved)
    };

    let (outcome, bundled, resolved) = run("echo generated > shared.txt").await;
    assert!(matches!(outcome, JobOutcome::Done), "{outcome:?}");
    assert!(bundled);
    assert!(
        resolved,
        "the orchestrator reads this to withhold the approval"
    );

    let committed = "echo generated > shared.txt && git add -A && \
        git -c user.name=a -c user.email=a@example.com commit -qm moved";
    let (outcome, bundled, _) = run(committed).await;
    let JobOutcome::Failed { reason } = outcome else {
        panic!("{outcome:?}")
    };
    assert!(reason.contains("the agent moved HEAD"), "{reason}");
    assert!(!bundled);
}

#[tokio::test]
async fn a_fix_of_a_review_commits_what_the_agent_changed_with_the_fix_trailer() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    std::fs::create_dir(&origin).unwrap();
    let (head, trunk) = remote(&origin, "feature.txt", "other.txt");

    let seen = tmp.path().join("prompt");
    let shell = format!(
        "printf '%s' \"$*\" > {} && echo fixed > feature.txt",
        seen.display()
    );
    let executor = with_fake_claude_running(tmp.path(), &shell, "done");
    let mut spec = spec(&origin, &head, &trunk);
    spec.resolver.api = Some(ApiAccess {
        base_url: "http://127.0.0.1:9".into(),
        token: "job-token".into(),
    });
    spec.task = Task::FixReview {
        findings: vec![
            serde_json::from_value(serde_json::json!({
                "kind": "other", "title": "The socket leaks", "file": "feature.txt", "line": 1,
                "confidence": 90, "lean": "change", "finding": "It stays open.",
                "suggestion": "Close it."
            }))
            .unwrap(),
        ],
    };
    let out = tmp.path().join("out");
    let result = executor.run("t7", &spec, &out).await.unwrap();
    assert!(matches!(result.outcome, JobOutcome::Done), "{result:?}");
    let prompt = std::fs::read_to_string(&seen).unwrap();
    assert!(
        prompt.contains("The socket leaks (feature.txt:1)"),
        "{prompt}"
    );
    assert!(prompt.contains("Close it."), "{prompt}");

    let check = tmp.path().join("check");
    std::fs::create_dir(&check).unwrap();
    git(&check, &["init", "-q"]);
    git(&check, &["fetch", "-q", origin.to_str().unwrap(), "pr"]);
    git(
        &check,
        &[
            "fetch",
            "-q",
            out.join(BUNDLE_FILE).to_str().unwrap(),
            "refs/heads/result:refs/heads/result",
        ],
    );
    let log = git(&check, &["log", "-1", "--format=%B", "result"]);
    assert!(log.contains("Fix the review's findings"), "{log}");
    assert!(log.contains("Fixed-by: mergepatrol"), "{log}");
}

#[tokio::test]
async fn a_fix_of_a_review_on_a_stack_layer_goes_on_top_of_that_layer() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    std::fs::create_dir(&origin).unwrap();
    git(&origin, &["init", "-q", "-b", "main"]);
    commit(&origin, "shared.txt", "base\n");
    git(&origin, &["checkout", "-q", "-b", "l1"]);
    let l1 = commit(&origin, "l1.txt", "layer 1\n");
    git(&origin, &["checkout", "-q", "-b", "l2"]);
    let l2 = commit(&origin, "l2.txt", "layer 2\n");

    let executor = with_fake_claude_running(tmp.path(), "echo fixed > l2.txt", "done");
    let mut spec = JobSpec {
        head_ref: "refs/heads/l2".into(),
        base_ref: Some("l1".into()),
        upstream: Some(l1.clone()),
        ..spec(&origin, &l2, &l1)
    };
    spec.resolver.api = Some(ApiAccess {
        base_url: "http://127.0.0.1:9".into(),
        token: "job-token".into(),
    });
    spec.task = Task::FixReview {
        findings: vec![
            serde_json::from_value(serde_json::json!({
                "kind": "other", "title": "Wrong", "file": "l2.txt", "line": 1,
                "confidence": 90, "lean": "change", "finding": "It is wrong.",
                "suggestion": "Fix it."
            }))
            .unwrap(),
        ],
    };
    let out = tmp.path().join("out");
    let result = executor.run("t-layer", &spec, &out).await.unwrap();
    assert!(matches!(result.outcome, JobOutcome::Done), "{result:?}");

    let check = tmp.path().join("check");
    std::fs::create_dir(&check).unwrap();
    git(&check, &["init", "-q"]);
    git(&check, &["fetch", "-q", origin.to_str().unwrap(), "l2"]);
    git(
        &check,
        &[
            "fetch",
            "-q",
            out.join(BUNDLE_FILE).to_str().unwrap(),
            "refs/heads/result:refs/heads/result",
        ],
    );
    assert_eq!(git(&check, &["rev-parse", "result~1"]), l2);
    assert_eq!(git(&check, &["show", "result:l2.txt"]), "fixed");
}

#[tokio::test]
async fn a_review_since_an_approval_names_only_the_queues_commits() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    std::fs::create_dir(&origin).unwrap();
    git(&origin, &["init", "-q", "-b", "main"]);
    let trunk = commit(&origin, "app.rs", "one\n");
    git(&origin, &["checkout", "-q", "-b", "pr"]);
    let approved = commit(&origin, "app.rs", "two\n");
    std::fs::write(origin.join("app.rs"), "three\n").unwrap();
    git(&origin, &["add", "-A"]);
    git(
        &origin,
        &["commit", "-qm", "Fix ci\n\nFixed-by: mergepatrol, a"],
    );
    let head = git(&origin, &["rev-parse", "HEAD"]);

    let seen = tmp.path().join("prompt");
    let keep = format!("printf '%s' \"$*\" > {}", seen.display());
    let answer = r#"{"summary":"s","findings":[]}"#;
    let executor = with_fake_claude_running(tmp.path(), &keep, answer);
    let mut spec = spec(&origin, &head, &trunk);
    spec.resolver.api = Some(mergepatrol_executor::protocol::ApiAccess {
        base_url: "http://127.0.0.1:9".into(),
        token: "t".into(),
    });
    spec.task = Task::Review {
        answered: Vec::new(),
        sent_back: Vec::new(),
        since: Some(approved),
    };
    let result = executor
        .run("t6", &spec, &tmp.path().join("out"))
        .await
        .unwrap();
    assert!(matches!(result.outcome, JobOutcome::Done), "{result:?}");
    let prompt = std::fs::read_to_string(&seen).unwrap();
    assert!(prompt.contains("Review only these commits"), "{prompt}");
    assert!(prompt.contains("Fix ci"), "{prompt}");
}

#[tokio::test]
async fn a_review_writes_what_the_agent_found_with_the_code_it_is_about() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    std::fs::create_dir(&origin).unwrap();
    git(&origin, &["init", "-q", "-b", "main"]);
    let lines: String = (1..=20).map(|i| format!("line {i}\n")).collect();
    let trunk = commit(&origin, "app.rs", &lines);
    git(&origin, &["checkout", "-q", "-b", "pr"]);
    let changed = lines.replace("line 10\n", "line 10 changed\n");
    let head = commit(&origin, "app.rs", &changed);

    let answer = serde_json::json!({
        "summary": "It changes a line.",
        "findings": [{
            "kind": "security", "title": "Line 10 changed", "file": "app.rs", "line": 10,
            "confidence": 64, "lean": "fine", "finding": "f", "unsure": "u",
            "checked": ["c"], "would_clear": "w", "suggestion": "s",
            "related": [{ "file": "app.rs", "line": 2, "why": "context" }]
        }]
    })
    .to_string();
    let mut spec = spec(&origin, &head, &trunk);
    spec.resolver.api = Some(ApiAccess {
        base_url: "http://127.0.0.1:9".into(),
        token: "job-token".into(),
    });
    spec.task = Task::Review {
        answered: Vec::new(),
        sent_back: Vec::new(),
        since: None,
    };
    let out = tmp.path().join("out");
    // The fake CLI echoes its answer, so one line: `echo` would break a `\n`.
    let result = with_fake_claude(tmp.path(), &format!("Here it is: {answer}"))
        .run("t6", &spec, &out)
        .await
        .unwrap();
    assert_eq!(result.outcome, JobOutcome::Done, "{result:?}");
    assert!(!out.join(BUNDLE_FILE).exists(), "a review returns no code");
    let spent = result.events.last().unwrap();
    assert_eq!(spent.kind, "reviewed", "{result:?}");

    let review: ReviewOutput =
        serde_json::from_slice(&std::fs::read(out.join(REVIEW_FILE)).unwrap()).unwrap();
    let f = &review.findings[0];
    assert_eq!(f.id, "f1");
    let hot: Vec<&str> = f
        .code
        .iter()
        .filter(|c| c.hot)
        .map(|c| c.text.as_str())
        .collect();
    assert_eq!(hot, ["line 10 changed"]);
    assert!(
        f.code.iter().any(|c| c.text == "line 10"),
        "the removed line shows"
    );
    let related: Vec<&str> = f.related[0].code.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(related, ["line 1", "line 2", "line 3", "line 4", "line 5"]);

    // A question about it, answered in plain text.
    spec.task = Task::Question {
        finding: Box::new(f.clone()),
        question: "Why?".into(),
        sent_back: false,
    };
    let tmp2 = tempfile::tempdir().unwrap();
    let out = tmp2.path().join("out");
    let result = with_fake_claude(tmp2.path(), "Because it is.")
        .run("t7", &spec, &out)
        .await
        .unwrap();
    assert_eq!(result.outcome, JobOutcome::Done, "{result:?}");
    assert_eq!(
        std::fs::read_to_string(out.join(ANSWER_FILE)).unwrap(),
        "Because it is."
    );
}

#[tokio::test]
async fn a_review_the_agent_cannot_give_fails_and_says_why() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    std::fs::create_dir(&origin).unwrap();
    let (head, trunk) = remote(&origin, "feature.txt", "other.txt");
    let mut spec = spec(&origin, &head, &trunk);
    spec.resolver.api = Some(ApiAccess {
        base_url: "http://127.0.0.1:9".into(),
        token: "job-token".into(),
    });
    spec.task = Task::Review {
        answered: Vec::new(),
        sent_back: Vec::new(),
        since: None,
    };
    let out = tmp.path().join("out");
    let result = with_fake_claude(tmp.path(), "Looks good to me!")
        .run("t8", &spec, &out)
        .await
        .unwrap();
    let JobOutcome::Failed { reason } = &result.outcome else {
        panic!("{result:?}")
    };
    assert!(reason.contains("not JSON"), "{reason}");
    assert_eq!(result.events.last().unwrap().kind, "unreviewed");
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
        resolv_conf: None,
        memory: "1g".into(),
        cpus: "1".into(),
        timeout: Duration::from_secs(120),
    };
    let out = tmp.path().join("out");
    let result = docker.run("test", &spec, &out).await;
    daemon.kill().unwrap();
    daemon.wait().unwrap();

    let result = result.unwrap();
    assert_eq!(result.outcome, JobOutcome::Done, "{result:?}");
    assert!(out.join(BUNDLE_FILE).exists());
}

/// A job that is cut off (its queue stopped) leaves no container or volume behind.
/// Needs Docker and the job image. `cargo test -p mergepatrol --test job -- --ignored`
#[tokio::test]
#[ignore]
async fn a_cut_off_docker_job_leaves_no_container() {
    let tmp = tempfile::tempdir().unwrap();
    let mut spec = spec(tmp.path(), "0", "0");
    // An address that never answers: the job hangs in its first fetch.
    spec.clone_url = "git://10.255.255.1/never".into();
    let docker = mergepatrol_executor::DockerExecutor {
        image: "mergepatrol-job:latest".into(),
        runtime: None,
        network: None,
        resolv_conf: None,
        memory: "1g".into(),
        cpus: "1".into(),
        timeout: Duration::from_secs(120),
    };
    let out = tmp.path().join("out");
    let name = "name=^mergepatrol-job-cut-off$";
    // From an earlier failed run of this test.
    Command::new("docker")
        .args(["rm", "-f", "-v", "mergepatrol-job-cut-off"])
        .output()
        .unwrap();
    let job = tokio::spawn(async move { docker.run("cut-off", &spec, &out).await });
    let containers = || {
        let out = Command::new("docker")
            .args(["ps", "-aq", "--filter", name])
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    let running = || {
        let out = Command::new("docker")
            .args(["ps", "-q", "--filter", name, "--filter", "status=running"])
            .output()
            .unwrap();
        !out.stdout.is_empty()
    };
    for _ in 0..50 {
        if running() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(running(), "the job never started");

    job.abort();
    let _ = job.await;
    for _ in 0..50 {
        if containers().is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("the container is still there: {}", containers());
}

/// The whole OpenAI path: the job hits a conflict, and Codex resolves it through the
/// orchestrator's proxy, which holds the key. Needs `codex` on PATH and OPENAI_API_KEY. For
/// another Responses API as the upstream, set JOB_LIVE_OPENAI_UPSTREAM (without `/v1`),
/// JOB_LIVE_OPENAI_KEY and JOB_LIVE_OPENAI_MODEL.
/// `cargo test -p mergepatrol --test job -- --ignored an_openai_conflict`
#[tokio::test]
#[ignore = "needs codex and an API key"]
async fn an_openai_conflict_is_resolved_through_the_proxy() {
    use std::sync::Arc;

    use mergepatrol_executor::protocol::ApiAccess;
    use mergepatrol_executor::proxy::{Api, ApiProxy, Credential, Upstreams};

    let env = |name: &str| std::env::var(name).ok();
    let proxy = Arc::new(ApiProxy::new(Upstreams {
        openai: env("JOB_LIVE_OPENAI_UPSTREAM").unwrap_or_else(|| "https://api.openai.com".into()),
        ..Upstreams::default()
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(axum::serve(listener, proxy.clone().router()).into_future());
    let key = env("JOB_LIVE_OPENAI_KEY")
        .or_else(|| env("OPENAI_API_KEY"))
        .expect("an API key");
    let granted = proxy.grant(Provider::OpenAi, Credential::ApiKey(key.into()), 100);

    let tmp = tempfile::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    std::fs::create_dir(&origin).unwrap();
    git(&origin, &["init", "-q", "-b", "main"]);
    commit(
        &origin,
        "greet.py",
        "def greet(name):\n    return 'Hello ' + name\n",
    );
    git(&origin, &["checkout", "-q", "-b", "pr"]);
    let head = commit(
        &origin,
        "greet.py",
        "def greet(name):\n    return 'Hello, ' + name + '!'\n",
    );
    git(&origin, &["checkout", "-q", "main"]);
    let trunk = commit(
        &origin,
        "greet.py",
        "def greet(person):\n    return 'Hello ' + person\n",
    );

    let mut spec = spec(&origin, &head, &trunk);
    spec.pr_title = "Friendlier greeting".into();
    spec.resolver.provider = Provider::OpenAi;
    spec.resolver.model = env("JOB_LIVE_OPENAI_MODEL").unwrap_or_else(|| "gpt-6.1-sol".into());
    spec.resolver.timeout_secs = 300;
    spec.resolver.max_turns = 30;
    spec.resolver.api = Some(ApiAccess {
        base_url: Api::of(Provider::OpenAi, &spec.resolver.model).base_url(&proxy_url),
        token: granted.token.clone(),
    });
    let out = tmp.path().join("out");
    let executor = LocalExecutor {
        program: env!("CARGO_BIN_EXE_mergepatrol").into(),
        timeout: Duration::from_secs(360),
    };
    let result = executor.run("live", &spec, &out).await.unwrap();
    println!("{:#?}", result.events);
    assert_eq!(result.outcome, JobOutcome::Done, "{result:?}");
    assert!(result.events.iter().any(|e| e.kind == "resolved"));

    // The resolution keeps both sides: the trunk's rename, and the PR's punctuation.
    let check = tmp.path().join("check");
    std::fs::create_dir(&check).unwrap();
    git(&check, &["init", "-q"]);
    git(&check, &["fetch", "-q", origin.to_str().unwrap(), "main"]);
    let bundle = out.join(BUNDLE_FILE);
    git(
        &check,
        &["fetch", "-q", bundle.to_str().unwrap(), "refs/heads/result"],
    );
    let resolved = git(&check, &["show", "FETCH_HEAD:greet.py"]);
    println!("{resolved}");
    assert!(
        resolved.contains("person") && resolved.contains('!'),
        "{resolved}"
    );
    let log = git(&check, &["log", "-1", "--format=%B", "FETCH_HEAD"]);
    assert!(log.contains("Resolved-by: mergepatrol, codex ("), "{log}");
}
