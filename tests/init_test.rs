mod common;

#[test]
fn test_init_success() {
    let (dir, bin) = common::setup_test_project("init_success");
    let output = std::process::Command::new(&bin)
        .args([
            "init",
            "--name",
            "TestProject",
            "--task-prefix",
            "TP",
            "--force",
        ])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "init should succeed: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        dir.join(".carryctx").join("config.toml").exists(),
        "config.toml should exist"
    );
}

#[test]
fn test_init_without_force_fails_on_second_call() {
    let (dir, bin) = common::setup_test_project("init_no_force");
    let first = std::process::Command::new(&bin)
        .args(["init", "--force"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(first.status.success(), "first init should succeed");
    let second = std::process::Command::new(&bin)
        .args(["init"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        !second.status.success(),
        "second init without --force should fail"
    );
}
