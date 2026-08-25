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

/// Regression test for https://github.com/Xuepoo/carryctx/issues/47.
///
/// A bare (unquoted) hyphenated query like `aria-owns` used to be handed
/// straight to FTS5 `MATCH`, where SQLite parses the hyphen as
/// `column:term` filter/exclusion syntax and fails with a misleading
/// `no such column: owns` — reading like a schema problem rather than a
/// quoting one. Verifies the query is sanitized before hitting SQLite,
/// and that already-quoted phrases and uppercase boolean operators keep
/// working exactly as `--help` documents.
#[test]
fn test_search_hyphenated_query_is_not_parsed_as_fts5_syntax() {
    let (dir, bin) = common::setup_test_project("search_hyphenated_query");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "SH"]);
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
        &[
            "task",
            "create",
            "--title",
            "axe aria-owns and aria-required-children checks",
        ],
    );

    // The exact reproduction from the issue: previously failed with
    // `DATABASE_ERROR: no such column: owns`.
    let bare = common::run_cmd(&dir, &bin, &["search", "aria-owns", "--json"]);
    assert!(
        bare.status.success(),
        "bare hyphenated query should not error: {}",
        String::from_utf8_lossy(&bare.stderr)
    );
    let bare_value: Value = serde_json::from_slice(&bare.stdout).expect("valid JSON");
    assert!(
        bare_value["success"].as_bool().unwrap_or(false),
        "response should report success: {bare_value}"
    );
    let bare_hits = bare_value["data"].as_array().expect("data is an array");
    assert_eq!(
        bare_hits.len(),
        1,
        "bare hyphenated query should find the task"
    );

    // Quoting was already a working workaround; must keep working.
    let quoted = common::run_cmd(&dir, &bin, &["search", "\"aria-owns\"", "--json"]);
    assert!(quoted.status.success());
    let quoted_value: Value = serde_json::from_slice(&quoted.stdout).expect("valid JSON");
    let quoted_hits = quoted_value["data"].as_array().expect("data is an array");
    assert_eq!(quoted_hits.len(), 1, "quoted phrase should still match");

    // Documented FTS5 boolean syntax must still behave as advertised.
    let boolean = common::run_cmd(
        &dir,
        &bin,
        &["search", "aria-owns OR nonexistentxyzterm", "--json"],
    );
    assert!(
        boolean.status.success(),
        "hyphenated term combined with OR should not error: {}",
        String::from_utf8_lossy(&boolean.stderr)
    );
    let boolean_value: Value = serde_json::from_slice(&boolean.stdout).expect("valid JSON");
    let boolean_hits = boolean_value["data"].as_array().expect("data is an array");
    assert_eq!(
        boolean_hits.len(),
        1,
        "OR-combined hyphenated term should still match"
    );
}

/// Regression test for CTX-0068 / issue #101 (FTS robustness).
///
/// An unterminated double quote used to surface raw `fts5: syntax error`
/// output, and a query with no searchable terms reached FTS5 as `MATCH ''`,
/// which is also an error. Both must degrade to ordinary (empty) results.
#[test]
fn test_search_unterminated_quote_and_termless_query_do_not_error() {
    let (dir, bin) = common::setup_test_project("search_quote_robustness");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "SQ"]);
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
        &["task", "create", "--title", "Ship exporter widget"],
    );

    // Unterminated quote: previously `fts5: syntax error near ""`.
    let unterminated = common::run_cmd(&dir, &bin, &["search", "exporter \"widget", "--json"]);
    assert!(
        unterminated.status.success(),
        "unterminated quote must not error: {}",
        String::from_utf8_lossy(&unterminated.stderr)
    );
    let unterminated_value: Value =
        serde_json::from_slice(&unterminated.stdout).expect("valid JSON");
    let hits = unterminated_value["data"]
        .as_array()
        .expect("data is array");
    assert_eq!(hits.len(), 1, "closed phrase should still match the task");

    // Termless queries must return empty results, not MATCH '' errors.
    for query in ["\"", "   ", "-"] {
        let termless = common::run_cmd(&dir, &bin, &["search", query, "--json"]);
        assert!(
            termless.status.success(),
            "termless query {query:?} must not error: {}",
            String::from_utf8_lossy(&termless.stderr)
        );
        let value: Value = serde_json::from_slice(&termless.stdout).expect("valid JSON");
        assert_eq!(
            value["data"].as_array().expect("data is array").len(),
            0,
            "termless query {query:?} should match nothing"
        );
    }
}

#[test]
fn test_search_bad_arguments_render_error_envelope() {
    // CTX-0074 / issue #96 remainder: an unresolvable --assignee used to
    // exit with a bare code and no output anywhere.
    let (dir, bin) = common::setup_test_project("search_envelope");
    common::init_and_agent(&dir, &bin);

    // Unknown --type values are rejected by clap itself with a clear usage
    // error; the handler-level validation stays as defense in depth.
    let out = common::run_cmd(
        &dir,
        &bin,
        &["--json", "search", "needle", "--assignee", "ghost"],
    );
    assert!(!out.status.success(), "unresolvable --assignee must fail");
    let json: serde_json::Value = serde_json::from_slice(&out.stderr)
        .expect("assignee failure must render a JSON error envelope");
    assert_eq!(json["success"].as_bool(), Some(false));
    assert_eq!(json["command"].as_str().unwrap(), "search");
    assert_eq!(
        json["error"]["code"].as_str().unwrap(),
        "RESOURCE_NOT_FOUND"
    );
}

/// CTX-0083 / issue #106 item 4: task hits must populate the top-level
/// `display_id` (previously null for task hits) in addition to the
/// compatibility `task_display_id` field. Non-task hits are unchanged.
#[test]
fn test_search_task_hit_populates_top_level_display_id() {
    let (dir, bin) = common::setup_test_project("search_task_display_id");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "TD"]);
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
        &["task", "create", "--title", "Quicksort widget exporter"],
    );

    let search = common::run_cmd(&dir, &bin, &["search", "quicksort", "--json"]);
    assert!(
        search.status.success(),
        "search should succeed: {}",
        String::from_utf8_lossy(&search.stderr)
    );
    let value: Value = serde_json::from_slice(&search.stdout).expect("valid JSON");
    let hits = value["data"].as_array().expect("data is an array");
    let task_hits: Vec<&Value> = hits.iter().filter(|h| h["kind"] == "task").collect();
    assert!(!task_hits.is_empty(), "expected at least one task hit");

    for hit in task_hits {
        let display_id = hit["display_id"].as_str().unwrap_or_else(|| {
            panic!("top-level display_id must be populated on task hits: {hit:?}")
        });
        assert_eq!(
            display_id,
            hit["task_display_id"].as_str().unwrap(),
            "display_id and task_display_id must agree on task hits"
        );
        assert!(display_id.starts_with("TD-"));
    }
}
