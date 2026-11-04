#[path = "../crates/core/src/ci.rs"]
mod ci;

#[test]
fn folded_run_scalar_keeps_one_shell_command() {
    // YAML folds these equally indented lines into `test 1 = 1`.
    let yaml = "on: pull_request\njobs:\n  check:\n    steps:\n      - run: >-\n          test\n          1 = 1\n";
    let original = "test 1 = 1";
    assert!(
        std::process::Command::new("sh")
            .args(["-c", original])
            .status()
            .unwrap()
            .success()
    );
    let commands = ci::verify_from_ci(&[("ci.yml".into(), yaml.into())]);
    // Conservatively omitting unsupported YAML is also acceptable.
    for command in commands {
        assert!(
            std::process::Command::new("sh")
                .args(["-c", &command])
                .status()
                .unwrap()
                .success(),
            "The derived check changed the passing folded command: {command}"
        );
    }
}
