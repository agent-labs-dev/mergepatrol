#[path = "../crates/core/src/ci.rs"]
mod ci;

#[test]
fn a_status_message_does_not_discard_the_check_after_it() {
    for command in ["echo starting; test 1 = 2", "printf starting; test 1 = 2"] {
        let yaml = format!("on: pull_request\njobs:\n  check:\n    steps:\n      - run: {command}\n");
        let commands = ci::verify_from_ci(&[("ci.yml".into(), yaml)]);
        assert_eq!(commands, [command], "This step runs a check after its status message");
    }
}
