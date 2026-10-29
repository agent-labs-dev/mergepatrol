//! Runs the real Claude CLI on a small conflict. Needs `claude` on PATH and ANTHROPIC_API_KEY.
//! `cargo test -p mergepatrol-resolver --test claude_live -- --ignored --nocapture`

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use mergepatrol_resolver::{ClaudeResolver, Resolution, ResolveRequest, Resolver};

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok || args[0] == "rebase", "git {args:?}");
}

#[tokio::test]
#[ignore]
async fn claude_resolves_a_rename_conflict() {
    let tmp = tempfile::tempdir().unwrap();
    let wt = tmp.path();
    let write = |s: &str| std::fs::write(wt.join("greet.py"), s).unwrap();

    git(wt, &["init", "-q", "-b", "main"]);
    write("def greet(name):\n    return 'Hello ' + name\n");
    git(wt, &["add", "."]);
    git(wt, &["commit", "-q", "-m", "root"]);
    git(wt, &["checkout", "-q", "-b", "pr"]);
    write("def greet(name):\n    return 'Hello, ' + name + '!'\n");
    git(
        wt,
        &["commit", "-q", "-am", "Add punctuation to the greeting"],
    );
    git(wt, &["checkout", "-q", "main"]);
    write("def greet(person):\n    return 'Hello ' + person\n");
    git(wt, &["commit", "-q", "-am", "Rename name to person"]);
    git(wt, &["checkout", "-q", "pr"]);
    git(wt, &["rebase", "-q", "main"]);

    let resolver = ClaudeResolver {
        api_key: std::env::var("ANTHROPIC_API_KEY").ok(),
        base_url: None,
        model: "claude-opus-5-5".into(),
        effort: "medium".into(),
        timeout: Duration::from_secs(300),
        max_turns: 20,
    };
    let files = vec!["greet.py".to_owned()];
    let outcome = resolver
        .resolve(ResolveRequest {
            workdir: wt,
            conflicted_files: &files,
            pr_title: "Friendlier greeting",
            pr_body: "Adds a comma and an exclamation mark.",
            commit_message: "Add punctuation to the greeting",
            trunk: "main",
            verify_commands: &[],
            transcript: &wt.join("transcript.jsonl"),
        })
        .await
        .unwrap();

    let result = std::fs::read_to_string(wt.join("greet.py")).unwrap();
    println!("{outcome:?}\n{result}");
    assert!(matches!(outcome, Resolution::Resolved { .. }));
    assert!(!result.contains("<<<<<<<"));
    assert!(result.contains("person") && result.contains("!"));
}
