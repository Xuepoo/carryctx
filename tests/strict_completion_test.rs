mod common;

use common::{init_and_agent, run_cmd, setup_test_project};

/// With `strict_completion = false` (the default), completing a task that
/// still has an open progress item must succeed and surface a warning in the
/// success envelope. Regression test for the warnings being silently
/// discarded at every `task <transition>` call site.
#[test]
fn test_complete_with_open_progress_warns_when_not_strict() {
    let (dir, bin) = setup_test_project("strict_completion_warn");
    init_and_agent(&dir, &bin);

    let create = run_cmd(&dir, &bin, &["task", "create", "--title", "warn test"]);
    assert!(create.status.success());
    let created: serde_json::Value =
        serde_json::from_slice(&create.stdout).expect("task create should print JSON");
    let task_ref = created["display_id"].as_str().unwrap().to_string();

    let todo = run_cmd(
        &dir,
        &bin,
        &["progress", "todo", "--task", &task_ref, "an open item"],
    );
    assert!(todo.status.success(), "progress todo should succeed");

    let start = run_cmd(&dir, &bin, &["task", "start", &task_ref]);
    assert!(start.status.success(), "task start should succeed");

    let complete = run_cmd(&dir, &bin, &["--json", "task", "complete", &task_ref]);
    assert!(
        complete.status.success(),
        "non-strict complete with an open item should still succeed: {}",
        String::from_utf8_lossy(&complete.stderr)
    );
    let envelope: serde_json::Value =
        serde_json::from_slice(&complete.stdout).expect("task complete should print JSON");
    assert_eq!(envelope["data"]["status"], "completed");
    let warnings = envelope["warnings"]
        .as_array()
        .expect("success envelope should carry a warnings array");
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap_or_default().contains("open progress")),
        "expected an open-progress warning, got: {warnings:?}"
    );
}

/// With `strict_completion = true`, completing a task that still has an open
/// progress item must be REJECTED (not silently allowed). Regression test for
/// the match-arm ordering bug in `evaluate_transition` where
/// `(Ac::Complete, St::Review | St::InProgress) => true` matched before the
/// `strict_completion` guard could ever run.
#[test]
fn test_complete_with_open_progress_blocked_when_strict() {
    let (dir, bin) = setup_test_project("strict_completion_block");
    init_and_agent(&dir, &bin);

    let create = run_cmd(&dir, &bin, &["task", "create", "--title", "strict test"]);
    assert!(create.status.success());
    let created: serde_json::Value =
        serde_json::from_slice(&create.stdout).expect("task create should print JSON");
    let task_ref = created["display_id"].as_str().unwrap().to_string();

    let todo = run_cmd(
        &dir,
        &bin,
        &["progress", "todo", "--task", &task_ref, "an open item"],
    );
    assert!(todo.status.success(), "progress todo should succeed");
    let todo_json: serde_json::Value =
        serde_json::from_slice(&todo.stdout).expect("progress todo should print JSON");
    let progress_ref = todo_json["display_id"].as_str().unwrap().to_string();

    let start = run_cmd(&dir, &bin, &["task", "start", &task_ref]);
    assert!(start.status.success(), "task start should succeed");

    // strict_completion = true via env, from a state where Complete would
    // otherwise be legal.
    let complete = std::process::Command::new(&bin)
        .args(["--json", "task", "complete", &task_ref])
        .env("CARRYCTX_AGENT", "tester")
        .env("CARRYCTX_STRICT_COMPLETION", "true")
        .current_dir(&dir)
        .output()
        .expect("command should execute");
    assert!(
        !complete.status.success(),
        "strict complete with an open item must be rejected, but succeeded: {}",
        String::from_utf8_lossy(&complete.stdout)
    );
    let envelope: serde_json::Value =
        serde_json::from_slice(&complete.stderr).expect("error envelope should print JSON");
    assert_eq!(envelope["error"]["code"], "STATE_CONFLICT");

    // The task must still be in_progress, not completed.
    let show = run_cmd(&dir, &bin, &["task", "show", &task_ref, "--json"]);
    assert!(show.status.success());
    let show_json: serde_json::Value =
        serde_json::from_slice(&show.stdout).expect("task show should print JSON");
    assert_eq!(show_json["data"]["status"], "in_progress");

    // Closing the open item, then completing, must succeed under the same
    // strict setting.
    let progress_complete = std::process::Command::new(&bin)
        .args(["progress", "complete", &progress_ref])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(&dir)
        .output()
        .expect("command should execute");
    assert!(progress_complete.status.success());

    let complete_again = std::process::Command::new(&bin)
        .args(["--json", "task", "complete", &task_ref])
        .env("CARRYCTX_AGENT", "tester")
        .env("CARRYCTX_STRICT_COMPLETION", "true")
        .current_dir(&dir)
        .output()
        .expect("command should execute");
    assert!(
        complete_again.status.success(),
        "strict complete with no open items should succeed: {}",
        String::from_utf8_lossy(&complete_again.stderr)
    );
}
