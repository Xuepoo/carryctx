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

#[test]
fn test_decision_supersede_text_and_json_output() {
    let (dir, bin) = common::setup_test_project("decision_supersede_test");
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
    common::run_cmd(&dir, &bin, &["task", "create", "--title", "Supersede Task"]);

    let d1 = common::run_cmd(
        &dir,
        &bin,
        &[
            "decision",
            "add",
            "--title",
            "Decision One",
            "--task",
            "DS-0001",
            "--json",
        ],
    );
    assert!(d1.status.success());

    let d2 = common::run_cmd(
        &dir,
        &bin,
        &[
            "decision",
            "add",
            "--title",
            "Decision Two",
            "--task",
            "DS-0001",
            "--json",
        ],
    );
    assert!(d2.status.success());
    let d2_val: serde_json::Value = serde_json::from_slice(&d2.stdout).unwrap();
    let d2_ulid = d2_val["data"]["id"].as_str().unwrap();

    // 1. Text output test using display IDs
    let sup_text = common::run_cmd(
        &dir,
        &bin,
        &[
            "decision",
            "supersede",
            "DEC-0001",
            "--by",
            "DEC-0002",
            "--agent",
            "tester",
        ],
    );
    assert!(
        sup_text.status.success(),
        "decision supersede with display ID should succeed"
    );
    let stdout_text = String::from_utf8_lossy(&sup_text.stdout);
    assert!(
        stdout_text.contains("Decision DEC-0001 superseded by DEC-0002"),
        "stdout must contain both display IDs, got: {stdout_text}"
    );
    assert!(
        !stdout_text.contains("Decision  superseded by "),
        "stdout must not have missing IDs: {stdout_text}"
    );

    // 2. Add a third decision and supersede using ULIDs
    let d3 = common::run_cmd(
        &dir,
        &bin,
        &[
            "decision",
            "add",
            "--title",
            "Decision Three",
            "--task",
            "DS-0001",
            "--json",
        ],
    );
    assert!(d3.status.success());
    let d3_val: serde_json::Value = serde_json::from_slice(&d3.stdout).unwrap();
    let d3_ulid = d3_val["data"]["id"].as_str().unwrap();

    let sup_ulid = common::run_cmd(
        &dir,
        &bin,
        &[
            "decision",
            "supersede",
            d2_ulid,
            "--by",
            d3_ulid,
            "--agent",
            "tester",
        ],
    );
    assert!(
        sup_ulid.status.success(),
        "decision supersede with ULIDs should succeed"
    );
    let stdout_ulid = String::from_utf8_lossy(&sup_ulid.stdout);
    assert!(
        !stdout_ulid.contains("Decision  superseded by "),
        "stdout must not have missing IDs with ULID: {stdout_ulid}"
    );
}

/// Regression test for CTX-0068 / issue #101 (SQL robustness).
///
/// The decision keyword search used to interpolate the query into a LIKE
/// pattern without escaping, so `%` and `_` acted as wildcards: searching
/// `%` or `_` matched every decision regardless of content. Wildcards in
/// user input must now match only their literal characters.
#[test]
fn test_decision_search_treats_like_wildcards_as_literals() {
    let (dir, bin) = common::setup_test_project("decision_like_escape");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "DL"]);
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
        &["task", "create", "--title", "Wildcard host task"],
    );

    let add = |title: &str| {
        common::run_cmd(
            &dir,
            &bin,
            &[
                "decision", "add", "--title", title, "--task", "DL-0001", "--json",
            ],
        )
    };
    assert!(
        add("Speed up 50% of build_time").status.success(),
        "first decision add should succeed"
    );
    assert!(
        add("Rename worker queue keys").status.success(),
        "second decision add should succeed"
    );

    let hits_for = |query: &str| -> usize {
        let out = common::run_cmd(&dir, &bin, &["decision", "search", query, "--json"]);
        assert!(
            out.status.success(),
            "decision search {query:?} should succeed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
        value["data"].as_array().expect("data is array").len()
    };

    // `%` must match only its literal character (present in exactly one
    // title), not act as a wildcard matching everything.
    assert_eq!(hits_for("%"), 1, "literal %% should match one decision");
    assert_eq!(hits_for("_"), 1, "literal _ should match one decision");
    assert_eq!(
        hits_for("build_time"),
        1,
        "underscore inside a term should still match literally"
    );
    assert_eq!(hits_for("worker"), 1, "plain keyword search keeps working");
}

/// CTX-0072 / issue #105: decision-chain integrity — a decision can neither
/// supersede itself nor gain a second successor once already superseded.
#[test]
fn test_decision_supersede_rejects_self_and_double_supersession() {
    let (dir, bin) = common::setup_test_project("decision_supersede_guards");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "SG"]);
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
    common::run_cmd(&dir, &bin, &["task", "create", "--title", "Guard task"]);

    for title in ["Decision A", "Decision B", "Decision C"] {
        let add = common::run_cmd(
            &dir,
            &bin,
            &[
                "decision", "add", "--title", title, "--task", "SG-0001", "--json",
            ],
        );
        assert!(add.status.success(), "add {title} failed");
    }

    // Self-supersession is rejected.
    let self_sup = common::run_cmd(
        &dir,
        &bin,
        &[
            "decision",
            "supersede",
            "DEC-0001",
            "--by",
            "DEC-0001",
            "--agent",
            "tester",
            "--json",
        ],
    );
    assert!(
        !self_sup.status.success(),
        "a decision cannot supersede itself"
    );

    // First supersession succeeds.
    let sup1 = common::run_cmd(
        &dir,
        &bin,
        &[
            "decision",
            "supersede",
            "DEC-0001",
            "--by",
            "DEC-0002",
            "--agent",
            "tester",
            "--json",
        ],
    );
    assert!(sup1.status.success(), "first supersede failed");

    // A second successor for the same decision is rejected.
    let sup2 = common::run_cmd(
        &dir,
        &bin,
        &[
            "decision",
            "supersede",
            "DEC-0001",
            "--by",
            "DEC-0003",
            "--agent",
            "tester",
            "--json",
        ],
    );
    assert!(
        !sup2.status.success(),
        "already-superseded decision must not gain a second successor"
    );
    let stderr = String::from_utf8_lossy(&sup2.stderr);
    assert!(
        stderr.contains("STATE_CONFLICT"),
        "double supersession must report STATE_CONFLICT: {stderr}"
    );

    // The surviving chain still points at DEC-0002.
    let show = common::run_cmd(&dir, &bin, &["decision", "show", "DEC-0001", "--json"]);
    assert!(show.status.success());
    let value: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&show.stdout)).expect("valid json");
    assert_eq!(value["data"]["superseded_by"], "DEC-0002");
}
