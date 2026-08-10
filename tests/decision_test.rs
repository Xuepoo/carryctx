mod common;

#[test]
fn test_decision_add_and_list() {
    let (dir, bin) = common::setup_test_project("decision_test");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "DC"]);
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
    common::run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", "Decision test task"],
    );

    // Add decision
    let add = common::run_cmd(
        &dir,
        &bin,
        &[
            "decision",
            "add",
            "--title",
            "Test decision",
            "--context",
            "Testing",
            "--decision",
            "Use markdown",
            "--consequences",
            "None",
            "--task",
            "DC-0001",
            "--json",
        ],
    );
    assert!(add.status.success(), "decision add should succeed");
    let stdout = String::from_utf8_lossy(&add.stdout);
    assert!(
        stdout.contains("Test decision"),
        "decision should contain title"
    );

    // List decisions
    let list = common::run_cmd(&dir, &bin, &["decision", "list", "--json"]);
    assert!(list.status.success(), "decision list should succeed");
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(
        stdout.contains("Test decision"),
        "list should contain the decision"
    );
}

#[test]
fn test_decision_search() {
    let (dir, bin) = common::setup_test_project("decision_search_test");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "DS"]);
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
        &["task", "create", "--title", "Search test task"],
    );
    common::run_cmd(
        &dir,
        &bin,
        &[
            "decision",
            "add",
            "--title",
            "UniqueSearchDecision",
            "--task",
            "DS-0001",
        ],
    );

    let search = common::run_cmd(
        &dir,
        &bin,
        &["decision", "search", "UniqueSearch", "--json"],
    );
    let stdout = String::from_utf8_lossy(&search.stdout);
    assert!(
        stdout.contains("UniqueSearchDecision"),
        "search should find the decision"
    );
}

#[test]
fn test_decision_add_rationale_is_stored_and_searchable() {
    let (dir, bin) = common::setup_test_project("decision_rationale_test");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "DR"]);
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
        &["task", "create", "--title", "Rationale test task"],
    );

    let add = common::run_cmd(
        &dir,
        &bin,
        &[
            "decision",
            "add",
            "--title",
            "Rationale decision",
            "--rationale",
            "UniqueRationaleReason",
            "--task",
            "DR-0001",
            "--json",
        ],
    );
    assert!(add.status.success(), "decision add should succeed");
    let stdout = String::from_utf8_lossy(&add.stdout);
    assert!(
        stdout.contains("UniqueRationaleReason"),
        "decision.add output should include the rationale field"
    );

    // Rationale alone (no title/context/consequences match) must be searchable.
    let search = common::run_cmd(
        &dir,
        &bin,
        &["decision", "search", "UniqueRationaleReason", "--json"],
    );
    let stdout = String::from_utf8_lossy(&search.stdout);
    assert!(
        stdout.contains("Rationale decision"),
        "search should find the decision purely by its rationale text"
    );
}

#[test]
fn test_decision_rapid_add_does_not_collide_on_display_id() {
    let (dir, bin) = common::setup_test_project("decision_rapid_test");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "DP"]);
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
        &["task", "create", "--title", "Rapid add test task"],
    );

    // Six rapid inserts used to collide because display_id was a truncated
    // ULID quantised to a 1024ms bucket (issue #54). All must now succeed
    // with distinct, sequential DEC-#### ids.
    let mut display_ids = Vec::new();
    for i in 0..6 {
        let add = common::run_cmd(
            &dir,
            &bin,
            &[
                "decision",
                "add",
                "--title",
                &format!("rapid {i}"),
                "--task",
                "DP-0001",
                "--json",
            ],
        );
        assert!(
            add.status.success(),
            "rapid decision add #{i} should succeed: {}",
            String::from_utf8_lossy(&add.stderr)
        );
        let stdout = String::from_utf8_lossy(&add.stdout);
        let value: serde_json::Value = serde_json::from_str(&stdout)
            .unwrap_or_else(|e| panic!("decision add #{i} did not print JSON: {e}: {stdout}"));
        let display_id = value["data"]["display_id"]
            .as_str()
            .expect("display_id present")
            .to_string();
        display_ids.push(display_id);
    }

    let unique: std::collections::HashSet<_> = display_ids.iter().collect();
    assert_eq!(
        unique.len(),
        display_ids.len(),
        "all display_ids must be unique, got {display_ids:?}"
    );
}

// ── Issue #71: decision list --task must filter and validate ─────────────

#[test]
fn test_decision_list_task_filters_by_task() {
    let (dir, bin) = common::setup_test_project("decision_task_filter");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "DF"]);
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
    common::run_cmd(&dir, &bin, &["task", "create", "--title", "Task A"]);
    common::run_cmd(&dir, &bin, &["task", "create", "--title", "Task B"]);

    for (task_ref, title) in [("DF-0001", "decision for A"), ("DF-0002", "decision for B")] {
        let add = common::run_cmd(
            &dir,
            &bin,
            &["decision", "add", "--title", title, "--task", task_ref],
        );
        assert!(add.status.success(), "decision add for {task_ref}");
    }

    // Unfiltered list sees both.
    let all = common::run_cmd(&dir, &bin, &["decision", "list", "--json"]);
    let all_value: serde_json::Value = serde_json::from_slice(&all.stdout).unwrap();
    assert_eq!(all_value["data"].as_array().unwrap().len(), 2);

    // Filtered list sees only the requested task's decision.
    let filtered = common::run_cmd(
        &dir,
        &bin,
        &["decision", "list", "--task", "DF-0001", "--json"],
    );
    assert!(filtered.status.success());
    let filtered_value: serde_json::Value = serde_json::from_slice(&filtered.stdout).unwrap();
    let rows = filtered_value["data"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "list --task must narrow the row count");
    let task_ids: std::collections::HashSet<_> = rows
        .iter()
        .map(|r| r["task_id"].as_str().unwrap())
        .collect();
    assert_eq!(task_ids.len(), 1, "every row must belong to the same task");

    // A bad ref is rejected, not silently ignored.
    let bad = common::run_cmd(
        &dir,
        &bin,
        &["decision", "list", "--task", "GARBAGE-9999", "--json"],
    );
    assert!(!bad.status.success(), "bad task ref must fail");
    let bad_value: serde_json::Value = serde_json::from_slice(&bad.stderr).unwrap();
    assert_eq!(bad_value["error"]["code"], "RESOURCE_NOT_FOUND");
}
