#[path = "../crates/core/src/ci.rs"]
mod ci;

#[test]
fn a_passing_heredoc_is_preserved_or_omitted() {
    let script = "python3 - <<'PY'\nassert (1 == 1)\nPY\n";
    let passes = |command: &str| {
        std::process::Command::new("sh")
            .args(["-c", command])
            .status()
            .unwrap()
            .success()
    };
    assert!(passes(script), "The actual workflow script passes");
    let block: String = script.lines().map(|line| format!("          {line}\n")).collect();
    let yaml = format!("on: pull_request\njobs:\n  check:\n    steps:\n      - run: |\n{block}");
    for command in ci::verify_from_ci(&[("ci.yml".into(), yaml)]) {
        assert!(passes(&command), "Derivation broke a passing heredoc: {command}");
    }
}
