#[path = "../crates/core/src/ci.rs"]
mod ci;

#[test]
fn a_plain_yaml_run_scalar_keeps_shell_quotes() {
    let original = "test \"hello world\" = \"hello world\"";
    assert!(
        std::process::Command::new("sh")
            .args(["-c", original])
            .status()
            .unwrap()
            .success()
    );
    let yaml = format!("on: pull_request\njobs:\n  check:\n    steps:\n      - run: {original}\n");
    let commands = ci::verify_from_ci(&[("ci.yml".into(), yaml)]);
    assert_eq!(
        commands,
        [original],
        "A one-line run must retain its shell syntax"
    );
}
