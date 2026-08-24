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

/// CTX-0072 / issue #105: config-provided task prefixes are validated at the
/// persistence boundary (uppercase ASCII, 1-10 chars) instead of silently
/// entering the display-id space.
#[test]
fn test_init_rejects_invalid_task_prefix() {
    let (dir, bin) = common::setup_test_project("init_bad_prefix");

    for bad in ["ctx", "TOOLONGPREFIX", "WITH SPACE", ""] {
        let out = common::run_cmd(
            &dir,
            &bin,
            &["init", "--force", "--task-prefix", bad, "--json"],
        );
        assert!(!out.status.success(), "prefix '{bad}' must be rejected");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("VALIDATION_FAILED"),
            "invalid prefix must report VALIDATION_FAILED: {stderr}"
        );
    }

    // A valid custom prefix still works and is persisted.
    let ok = common::run_cmd(
        &dir,
        &bin,
        &["init", "--force", "--task-prefix", "ABC", "--json"],
    );
    assert!(
        ok.status.success(),
        "valid prefix must be accepted: {}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&ok.stdout)).expect("valid json");
    assert_eq!(value["data"]["task_prefix"], "ABC");
}
