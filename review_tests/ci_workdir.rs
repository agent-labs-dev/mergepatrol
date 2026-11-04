#[path = "../crates/core/src/ci.rs"]
mod ci;

#[test]
fn derived_checks_keep_the_steps_working_directory() {
    let root = std::env::temp_dir().join(format!("ci-workdir-{}", std::process::id()));
    std::fs::create_dir_all(root.join("ui")).unwrap();
    std::fs::write(root.join("ui/marker"), "present").unwrap();
    let yaml = "on: pull_request\njobs:\n  check:\n    steps:\n      - name: ui\n        working-directory: ui\n        run: test -f marker\n";
    assert!(
        std::process::Command::new("sh")
            .args(["-c", "test -f marker"])
            .current_dir(root.join("ui"))
            .status()
            .unwrap()
            .success()
    );
    let commands = ci::verify_from_ci(&[("ci.yml".into(), yaml.into())]);
    let failures: Vec<_> = commands
        .iter()
        .filter(|c| {
            !std::process::Command::new("sh")
                .args(["-c", c])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        })
        .collect();
    std::fs::remove_dir_all(&root).unwrap();
    assert!(
        failures.is_empty(),
        "Derived checks must preserve working-directory or be omitted: {failures:?}"
    );
}
