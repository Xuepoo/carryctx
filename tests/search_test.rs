mod common;

use serde_json::Value;

/// Regression / feature test for https://github.com/Xuepoo/carryctx/issues/45.
///
/// Verifies full-text search finds matches across tasks, progress items,
/// checkpoints, and decisions, that every hit resolves the owning task's
/// display ID/status/branch, and that `--type`/`--status`/`--agent`
/// narrow the result set correctly.
#[test]
fn test_search_finds_hits_across_all_entity_kinds() {
    let (dir, bin) = common::setup_test_project("search_all_kinds");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "SR"]);
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

    let task_out = common::run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", "Fix markdown worker protocol"],
    );
    assert!(task_out.status.success(), "task create should succeed");

    common::run_cmd(
        &dir,
        &bin,
        &[
            "progress",
            "todo",
            "--task",
            "SR-0001",
            "Wire up streaming markdown append",
        ],
    );
    common::run_cmd(
        &dir,
        &bin,
        &[
            "checkpoint",
            "--task",
            "SR-0001",
            "--done",
            "markdown worker protocol changed to append-based",
        ],
    );
    common::run_cmd(
        &dir,
        &bin,
        &[
            "decision",
            "add",
            "--title",
            "Use append protocol for markdown worker",
            "--task",
            "SR-0001",
        ],
    );

    let search = common::run_cmd(&dir, &bin, &["search", "markdown", "--json"]);
    assert!(
        search.status.success(),
        "search should succeed: {}",
        String::from_utf8_lossy(&search.stderr)
    );
    let value: Value = serde_json::from_slice(&search.stdout).expect("valid JSON");
    let hits = value["data"].as_array().expect("data is an array");
    assert_eq!(
        hits.len(),
        4,
        "expected one hit per entity kind (task, progress, checkpoint, decision): {hits:?}"
    );

    let kinds: std::collections::HashSet<&str> =
        hits.iter().map(|h| h["kind"].as_str().unwrap()).collect();
    assert_eq!(
        kinds,
        ["task", "progress", "checkpoint", "decision"]
            .into_iter()
            .collect(),
        "every entity kind should be represented: {kinds:?}"
    );

    for hit in hits {
        assert_eq!(
            hit["task_display_id"], "SR-0001",
            "every hit should resolve back to the owning task's display ID: {hit:?}"
        );
        assert!(
            hit["snippet"].as_str().unwrap().contains('['),
            "snippet should bracket the match: {hit:?}"
        );
    }
}

#[test]
fn test_search_type_filter_narrows_to_one_kind() {
    let (dir, bin) = common::setup_test_project("search_type_filter");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "SF"]);
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
    common::run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", "Investigate flaky retry test"],
    );
    common::run_cmd(
        &dir,
        &bin,
        &[
            "checkpoint",
            "--task",
            "SF-0001",
            "--done",
            "Reproduced the flaky retry failure locally",
        ],
    );

    let task_only = common::run_cmd(&dir, &bin, &["search", "flaky", "--type", "task", "--json"]);
    let task_value: Value = serde_json::from_slice(&task_only.stdout).unwrap();
    assert_eq!(
        task_value["data"].as_array().unwrap().len(),
        1,
        "title contains 'flaky', should match the task"
    );

    let checkpoint_only = common::run_cmd(
        &dir,
        &bin,
        &["search", "flaky", "--type", "checkpoint", "--json"],
    );
    let checkpoint_value: Value = serde_json::from_slice(&checkpoint_only.stdout).unwrap();
    assert_eq!(
        checkpoint_value["data"].as_array().unwrap().len(),
        1,
        "checkpoint note contains 'flaky', should match the checkpoint"
    );

    let decision_only = common::run_cmd(
        &dir,
        &bin,
        &["search", "flaky", "--type", "decision", "--json"],
    );
    let decision_value: Value = serde_json::from_slice(&decision_only.stdout).unwrap();
    assert_eq!(
        decision_value["data"].as_array().unwrap().len(),
        0,
        "no decisions recorded, --type decision should return nothing"
    );
}

#[test]
fn test_search_status_filter_only_matches_owning_task_status() {
    let (dir, bin) = common::setup_test_project("search_status_filter");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "SS"]);
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
    common::run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", "Ship the exporter widget"],
    );

    let matching = common::run_cmd(
        &dir,
        &bin,
        &["search", "exporter", "--status", "ready", "--json"],
    );
    let matching_value: Value = serde_json::from_slice(&matching.stdout).unwrap();
    assert_eq!(
        matching_value["data"].as_array().unwrap().len(),
        1,
        "a fresh task is 'ready', --status ready should match"
    );

    let non_matching = common::run_cmd(
        &dir,
        &bin,
        &["search", "exporter", "--status", "completed", "--json"],
    );
    let non_matching_value: Value = serde_json::from_slice(&non_matching.stdout).unwrap();
    assert_eq!(
        non_matching_value["data"].as_array().unwrap().len(),
        0,
        "task is not completed, --status completed should not match"
    );
}

#[test]
fn test_search_no_match_returns_empty_array_not_error() {
    let (dir, bin) = common::setup_test_project("search_no_match");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "SN"]);
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
    common::run_cmd(&dir, &bin, &["task", "create", "--title", "Some task"]);

    let result = common::run_cmd(&dir, &bin, &["search", "nonexistentxyzterm", "--json"]);
    assert!(
        result.status.success(),
        "search with no matches should still exit 0"
    );
    let value: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(value["data"].as_array().unwrap().len(), 0);
}
