mod common;

use common::{init_and_agent, run_cmd, setup_test_project};
use serde_json::Value;

fn json_stdout(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "stdout was not JSON: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

#[test]
fn task_scope_add_list_remove_is_audited() {
    let (dir, bin) = setup_test_project("task_scope_lifecycle");
    init_and_agent(&dir, &bin);

    let task = run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", "scoped task", "--json"],
    );
    assert!(task.status.success());
    let task_ref = json_stdout(&task)["data"]["display_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let added = run_cmd(
        &dir,
        &bin,
        &[
            "task",
            "scope",
            "add",
            &task_ref,
            "src/storage/**",
            "--json",
        ],
    );
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    let added_json = json_stdout(&added);
    assert_eq!(added_json["command"], "task.scope_add");
    assert_eq!(added_json["data"]["pattern"], "src/storage/**");
    assert!(!added_json["data"]["id"].as_str().unwrap().is_empty());

    let listed = run_cmd(&dir, &bin, &["task", "scope", "list", &task_ref, "--json"]);
    assert!(listed.status.success());
    assert_eq!(json_stdout(&listed)["data"][0]["pattern"], "src/storage/**");

    let removed = run_cmd(
        &dir,
        &bin,
        &[
            "task",
            "scope",
            "remove",
            &task_ref,
            "src/storage/**",
            "--json",
        ],
    );
    assert!(removed.status.success());
    assert_eq!(json_stdout(&removed)["command"], "task.scope_remove");

    let listed_again = run_cmd(&dir, &bin, &["task", "scope", "list", &task_ref, "--json"]);
    assert!(listed_again.status.success());
    assert_eq!(json_stdout(&listed_again)["data"], Value::Array(vec![]));
}

#[test]
fn task_scope_conflicts_are_limited_to_requested_task() {
    let (dir, bin) = setup_test_project("task_scope_conflicts");
    init_and_agent(&dir, &bin);

    let first = run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", "first", "--json"],
    );
    let first_ref = json_stdout(&first)["data"]["display_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let second = run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", "second", "--json"],
    );
    let second_ref = json_stdout(&second)["data"]["display_id"]
        .as_str()
        .unwrap()
        .to_owned();

    for task_ref in [&first_ref, &second_ref] {
        let output = run_cmd(
            &dir,
            &bin,
            &["task", "scope", "add", task_ref, "src/**", "--json"],
        );
        assert!(output.status.success());
    }

    let conflicts = run_cmd(
        &dir,
        &bin,
        &["task", "scope", "conflicts", &first_ref, "--json"],
    );
    assert!(conflicts.status.success());
    let conflicts_json = json_stdout(&conflicts);
    let data = conflicts_json["data"].as_array().unwrap();
    assert_eq!(data.len(), 1);
    assert_eq!(data[0]["task_display_id_a"], first_ref);
    assert_eq!(data[0]["task_display_id_b"], second_ref);
}

#[test]
fn task_scope_conflicts_reject_unknown_task() {
    let (dir, bin) = setup_test_project("task_scope_unknown_conflicts");
    init_and_agent(&dir, &bin);
    let output = run_cmd(
        &dir,
        &bin,
        &["task", "scope", "conflicts", "CTX-9999", "--json"],
    );
    assert!(!output.status.success());
    let body: Value = serde_json::from_slice(&output.stderr).unwrap_or_else(|_| {
        panic!(
            "stderr was not JSON: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert_eq!(body["success"], false);
    assert_eq!(body["error"]["code"], "RESOURCE_NOT_FOUND");
}
