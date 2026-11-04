#[path = "../crates/core/src/ci.rs"]
mod ci;

#[test]
fn step_metadata_is_not_part_of_a_run_block() {
    let yaml = "on: pull_request\njobs:\n  check:\n    steps:\n      - run: |\n          test 1 = 1\n        timeout-minutes: 5\n      - run: test 2 = 2\n";
    let commands = ci::verify_from_ci(&[("ci.yml".into(), yaml.into())]);
    assert_eq!(
        commands,
        ["test 1 = 1", "test 2 = 2"],
        "Step metadata must not be appended as shell commands"
    );
}
