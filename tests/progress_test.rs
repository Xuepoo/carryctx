mod common;

#[test]
fn test_progress_todo_and_list() {
    let (dir, bin) = common::setup_test_project("progress_test");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "TP"]);
    common::run_cmd(
        &dir,
        &bin,
        &[
            "agent",
            "register",
            "--name",
            "tester",
            "--provider",
            "test",
        ],
    );

    // Create task
    let create = common::run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", "Progress test task", "--json"],
    );
    let stdout = String::from_utf8_lossy(&create.stdout);
    assert!(
        stdout.contains("display_id"),
        "task create should return display_id"
    );

    // Add progress items
    let todo = common::run_cmd(
        &dir,
        &bin,
        &[
            "progress",
            "todo",
            "--task",
            "TP-0001",
            "Test progress",
            "--json",
        ],
    );
    assert!(todo.status.success(), "progress todo should succeed");
    assert!(
        String::from_utf8_lossy(&todo.stdout).contains("Test progress"),
        "todo should contain content"
    );

    // List progress
    let list = common::run_cmd(
        &dir,
        &bin,
        &["progress", "list", "--task", "TP-0001", "--json"],
    );
    assert!(list.status.success(), "progress list should succeed");
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(
        stdout.contains("Test progress"),
        "list should contain the progress item"
    );
}

/// Regression test for https://github.com/Xuepoo/carryctx/issues/76.
///
/// `progress show` on a missing ref short-circuited with a bare `ExitCode`,
/// skipping the standard error envelope. Machine consumers must get the
/// standard `success:false` envelope on stderr with exit code 7.
#[test]
fn test_progress_show_missing_returns_standard_error_envelope() {
    let (dir, bin) = common::setup_test_project("progress_show_missing");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "TP"]);
    common::run_cmd(
        &dir,
        &bin,
        &[
            "agent",
            "register",
            "--name",
            "tester",
            "--provider",
            "test",
        ],
    );

    let show = common::run_cmd(&dir, &bin, &["progress", "show", "PG-9999", "--json"]);
    assert!(!show.status.success(), "missing progress must fail");
    assert_eq!(show.status.code(), Some(7), "exit code must be 7");
    let stderr: serde_json::Value = serde_json::from_slice(&show.stderr).unwrap_or_else(|e| {
        panic!(
            "stderr must be a JSON envelope: {e}: {}",
            String::from_utf8_lossy(&show.stderr)
        )
    });
    assert_eq!(stderr["success"], false);
    assert_eq!(stderr["command"], "progress.show");
    assert_eq!(stderr["error"]["code"], "RESOURCE_NOT_FOUND");
}
