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

/// CTX-0072 / issue #105: reorder validates membership, uniqueness, and full
/// coverage before applying positions; the repo UPDATE used to silently drop
/// foreign ids leaving mixed stale positions.
#[test]
fn test_reorder_rejects_foreign_duplicate_and_partial_lists() {
    let (dir, bin) = common::setup_test_project("progress_reorder_guards");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "PR"]);
    common::init_and_agent(&dir, &bin);

    let mk = |title: &str| {
        let t = common::run_cmd(&dir, &bin, &["task", "create", "--title", title, "--json"]);
        assert!(t.status.success(), "create {title} failed");
        task_display_id(&dir, &bin, title)
    };
    let task_a = mk("reorder A");
    let task_b = mk("reorder B");

    let item = |task_ref: &str, content: &str| {
        let p = common::run_cmd(
            &dir,
            &bin,
            &["progress", "note", content, "--task", task_ref, "--json"],
        );
        assert!(p.status.success(), "progress add failed");
        String::from_utf8_lossy(&p.stdout)
            .split("\"display_id\":\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap()
            .to_string()
    };

    let a1 = item(&task_a, "a1");
    let a2 = item(&task_a, "a2");
    let _b1 = item(&task_b, "b1");

    // Foreign item is rejected.
    let foreign = common::run_cmd(
        &dir,
        &bin,
        &[
            "progress", "reorder", "--task", &task_a, "--order", &a1, "--order", &_b1, "--json",
        ],
    );
    assert!(!foreign.status.success(), "foreign item must be rejected");

    // Duplicates are rejected.
    let dup = common::run_cmd(
        &dir,
        &bin,
        &[
            "progress", "reorder", "--task", &task_a, "--order", &a1, "--order", &a1, "--json",
        ],
    );
    assert!(!dup.status.success(), "duplicate items must be rejected");

    // Partial coverage is rejected.
    let partial = common::run_cmd(
        &dir,
        &bin,
        &[
            "progress", "reorder", "--task", &task_a, "--order", &a1, "--json",
        ],
    );
    assert!(!partial.status.success(), "partial list must be rejected");

    // Full valid reorder succeeds.
    let ok = common::run_cmd(
        &dir,
        &bin,
        &[
            "progress", "reorder", "--task", &task_a, "--order", &a2, "--order", &a1, "--json",
        ],
    );
    assert!(
        ok.status.success(),
        "valid reorder must succeed: {}",
        String::from_utf8_lossy(&ok.stderr)
    );
}

fn task_display_id(dir: &std::path::Path, bin: &std::path::Path, title: &str) -> String {
    let list = common::run_cmd(dir, bin, &["task", "list", "--json"]);
    assert!(list.status.success(), "task list failed");
    let value: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&list.stdout)).expect("valid json envelope");
    value["data"]
        .as_array()
        .expect("task list array")
        .iter()
        .find(|t| t["title"] == serde_json::Value::String(title.to_string()))
        .unwrap_or_else(|| panic!("task '{title}' not found"))["display_id"]
        .as_str()
        .expect("display id")
        .to_string()
}
