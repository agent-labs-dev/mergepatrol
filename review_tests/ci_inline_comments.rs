#[path = "../crates/core/src/ci.rs"]
mod ci;

#[test]
fn inline_comments_do_not_hide_later_checks() {
    let script = "printf '%s\\n' \"a \\\" quote\" # status\ntest 1 = 2";
    let passes = |command: &str| {
        std::process::Command::new("sh")
            .args(["-c", command])
            .status()
            .unwrap()
            .success()
    };
    assert!(!passes(script));
    let block: String = script.lines().map(|line| format!("          {line}\n")).collect();
    let yaml = format!("on: pull_request\njobs:\n  check:\n    steps:\n      - run: |\n{block}");
    let commands = ci::verify_from_ci(&[("ci.yml".into(), yaml)]);
    for command in commands {
        assert!(!passes(&command), "The derived check hides the failing test: {command}");
    }
}
