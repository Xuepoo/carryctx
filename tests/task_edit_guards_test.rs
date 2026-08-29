mod common;

/// CTX-0072 / issue #105: edit_task mutability policy and audit fidelity.
///
/// - Terminal tasks (completed/cancelled) are immutable.
/// - Optional fields (`description`, `required_role`) can be cleared by
///   passing an empty string.
/// - The `task.edited` audit payload includes `required_role` before/after.
/// - Titles are capped at 200 characters because they are duplicated into
///   event payloads.
#[test]
fn test_edit_completed_task_is_rejected() {
    let (dir, bin) = common::setup_test_project("edit_terminal");
    common::init_and_agent(&dir, &bin);

    common::run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", "done deal", "--json"],
    );
    // Claim moves the task to in_progress; complete makes it terminal.
    let id = task_display_id(&dir, &bin, "done deal");
    common::run_cmd_as(&dir, &bin, "tester", &["task", "claim", &id, "--json"]);
    let complete = common::run_cmd_as(&dir, &bin, "tester", &["task", "complete", &id, "--json"]);
    assert!(complete.status.success(), "complete should succeed");

    let retitle = common::run_cmd(
        &dir,
        &bin,
        &[
            "task",
            "edit",
            &id,
            "--title",
            "renamed after death",
            "--json",
        ],
    );
    assert!(
        !retitle.status.success(),
        "editing a completed task must fail"
    );
    let stderr = String::from_utf8_lossy(&retitle.stderr);
    assert!(
        stderr.contains("STATE_CONFLICT"),
        "terminal edit must report STATE_CONFLICT: {stderr}"
    );

    // Cancelled tasks are equally frozen.
    common::run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", "dead on arrival", "--json"],
    );
    let id2 = task_display_id(&dir, &bin, "dead on arrival");
    common::run_cmd(
        &dir,
        &bin,
        &["task", "cancel", &id2, "--reason", "not needed", "--json"],
    );
    let reprioritize = common::run_cmd(
        &dir,
        &bin,
        &["task", "edit", &id2, "--priority", "urgent", "--json"],
    );
    assert!(
        !reprioritize.status.success(),
        "editing a cancelled task must fail"
    );
}

#[test]
fn test_force_corrects_terminal_tasks_only_for_terminal_actor() {
    let (dir, bin) = common::setup_test_project("edit_terminal_force");
    common::init_and_agent(&dir, &bin);
    let register_other = common::run_cmd(
        &dir,
        &bin,
        &["agent", "register", "--name", "other", "--provider", "test"],
    );
    assert!(register_other.status.success());

    common::run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", "correct me", "--json"],
    );
    let id = task_display_id(&dir, &bin, "correct me");
    let complete = common::run_cmd_as(&dir, &bin, "tester", &["task", "claim", &id, "--json"]);
    assert!(complete.status.success());
    let complete = common::run_cmd_as(&dir, &bin, "tester", &["task", "complete", &id, "--json"]);
    assert!(complete.status.success());

    let unauthorized = common::run_cmd_as(
        &dir,
        &bin,
        "other",
        &[
            "task", "edit", &id, "--title", "blocked", "--force", "--json",
        ],
    );
    assert!(!unauthorized.status.success());
    assert!(String::from_utf8_lossy(&unauthorized.stderr).contains("PERMISSION_SCOPE"));

    let corrected = common::run_cmd_as(
        &dir,
        &bin,
        "tester",
        &[
            "task",
            "edit",
            &id,
            "--title",
            "corrected",
            "--force",
            "--json",
        ],
    );
    assert!(
        corrected.status.success(),
        "{}",
        String::from_utf8_lossy(&corrected.stderr)
    );
    let events = common::run_cmd(
        &dir,
        &bin,
        &["event", "list", "--event-type", "task.corrected", "--json"],
    );
    assert!(events.status.success());
    let value: serde_json::Value = serde_json::from_slice(&events.stdout).expect("valid JSON");
    assert_eq!(value["data"]["events"][0]["payload"]["forced"], true);
    assert_eq!(
        value["data"]["events"][0]["payload"]["before"]["title"],
        "correct me"
    );
    assert_eq!(
        value["data"]["events"][0]["payload"]["after"]["title"],
        "corrected"
    );

    common::run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", "cancel correction", "--json"],
    );
    let cancelled_id = task_display_id(&dir, &bin, "cancel correction");
    let cancelled = common::run_cmd_as(
        &dir,
        &bin,
        "tester",
        &[
            "task",
            "cancel",
            &cancelled_id,
            "--reason",
            "obsolete",
            "--json",
        ],
    );
    assert!(cancelled.status.success());
    let corrected_cancel = common::run_cmd_as(
        &dir,
        &bin,
        "tester",
        &[
            "task",
            "edit",
            &cancelled_id,
            "--priority",
            "urgent",
            "--force",
            "--json",
        ],
    );
    assert!(
        corrected_cancel.status.success(),
        "{}",
        String::from_utf8_lossy(&corrected_cancel.stderr)
    );
    let cancelled_events = common::run_cmd(
        &dir,
        &bin,
        &[
            "event",
            "list",
            "--event-type",
            "task.corrected",
            "--task",
            &cancelled_id,
            "--json",
        ],
    );
    assert!(cancelled_events.status.success());
}

#[test]
fn test_force_rejects_deactivated_terminal_actor_without_mutation() {
    let (dir, bin) = common::setup_test_project("edit_terminal_deactivated");
    common::init_and_agent(&dir, &bin);
    common::run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", "deactivated owner", "--json"],
    );
    let id = task_display_id(&dir, &bin, "deactivated owner");
    assert!(
        common::run_cmd_as(&dir, &bin, "tester", &["task", "claim", &id, "--json"])
            .status
            .success()
    );
    assert!(
        common::run_cmd_as(&dir, &bin, "tester", &["task", "complete", &id, "--json"])
            .status
            .success()
    );
    assert!(
        common::run_cmd(&dir, &bin, &["agent", "deactivate", "tester", "--json"])
            .status
            .success()
    );

    let correction = common::run_cmd_as(
        &dir,
        &bin,
        "tester",
        &[
            "task",
            "edit",
            &id,
            "--title",
            "must not change",
            "--force",
            "--json",
        ],
    );
    assert!(!correction.status.success());
    assert!(String::from_utf8_lossy(&correction.stderr).contains("PERMISSION_SCOPE"));
    let task = common::run_cmd(&dir, &bin, &["task", "show", &id, "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&task.stdout).expect("valid JSON");
    assert_eq!(value["data"]["title"], "deactivated owner");
    let events = common::run_cmd(
        &dir,
        &bin,
        &["event", "list", "--event-type", "task.corrected", "--json"],
    );
    let value: serde_json::Value = serde_json::from_slice(&events.stdout).expect("valid JSON");
    assert!(value["data"]["events"].as_array().unwrap().is_empty());
}

#[test]
fn test_force_is_rejected_for_nonterminal_tasks() {
    let (dir, bin) = common::setup_test_project("edit_force_nonterminal");
    common::init_and_agent(&dir, &bin);
    common::run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", "still active", "--json"],
    );
    let id = task_display_id(&dir, &bin, "still active");

    let forced = common::run_cmd(
        &dir,
        &bin,
        &[
            "task",
            "edit",
            &id,
            "--title",
            "must not change",
            "--force",
            "--json",
        ],
    );
    assert!(!forced.status.success());
    assert!(String::from_utf8_lossy(&forced.stderr).contains("STATE_CONFLICT"));

    let task = common::run_cmd(&dir, &bin, &["task", "show", &id, "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&task.stdout).expect("valid JSON");
    assert_eq!(value["data"]["title"], "still active");
    let events = common::run_cmd(
        &dir,
        &bin,
        &[
            "event",
            "list",
            "--event-type",
            "task.edited",
            "--task",
            &id,
            "--json",
        ],
    );
    let value: serde_json::Value = serde_json::from_slice(&events.stdout).expect("valid JSON");
    assert!(value["data"]["events"].as_array().unwrap().is_empty());
}

#[test]
fn test_force_accepts_cancelled_legacy_name_actor_when_active() {
    let (dir, bin) = common::setup_test_project("edit_terminal_legacy_actor");
    common::init_and_agent(&dir, &bin);
    common::run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", "legacy cancelled", "--json"],
    );
    let id = task_display_id(&dir, &bin, "legacy cancelled");
    assert!(
        common::run_cmd_as(
            &dir,
            &bin,
            "tester",
            &["task", "cancel", &id, "--reason", "obsolete", "--json"]
        )
        .status
        .success()
    );

    let db_path = dir.join(".git/carryctx/state.sqlite");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    db.execute_batch("PRAGMA foreign_keys=OFF; DROP TRIGGER events_reject_delete;")
        .unwrap();
    db.execute(
        "DELETE FROM events WHERE task_id = (SELECT id FROM tasks WHERE display_id = ?1) AND type = 'task.cancelled'",
        [&id],
    ).unwrap();
    let task_id: String = db
        .query_row("SELECT id FROM tasks WHERE display_id = ?1", [&id], |row| {
            row.get(0)
        })
        .unwrap();
    let project_id: String = db
        .query_row(
            "SELECT project_id FROM tasks WHERE id = ?1",
            [&task_id],
            |row| row.get(0),
        )
        .unwrap();
    db.execute(
        "INSERT INTO events (id, project_id, type, aggregate_type, aggregate_id, payload_json, actor_agent_id, task_id, occurred_at) VALUES (?1, ?2, 'task.cancelled', 'task', ?3, '{}', ?4, ?3, '2026-01-01T00:00:00Z')",
        rusqlite::params!["01LEGACY000000000000000000", project_id, task_id, "tester"],
    ).unwrap();
    db.execute_batch(
        "PRAGMA foreign_keys=ON; CREATE TRIGGER events_reject_delete BEFORE DELETE ON events BEGIN SELECT RAISE(ABORT, 'events are append-only'); END;",
    )
    .unwrap();
    drop(db);

    let corrected = common::run_cmd_as(
        &dir,
        &bin,
        "tester",
        &[
            "task",
            "edit",
            &id,
            "--title",
            "legacy corrected",
            "--force",
            "--json",
        ],
    );
    assert!(
        corrected.status.success(),
        "{}",
        String::from_utf8_lossy(&corrected.stderr)
    );
}

#[test]
fn test_force_accepts_legacy_owner_name_without_terminal_actor() {
    let (dir, bin) = common::setup_test_project("edit_legacy_owner");
    common::init_and_agent(&dir, &bin);
    common::run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", "legacy owner", "--json"],
    );
    let id = task_display_id(&dir, &bin, "legacy owner");
    assert!(
        common::run_cmd_as(&dir, &bin, "tester", &["task", "claim", &id, "--json"])
            .status
            .success()
    );
    assert!(
        common::run_cmd_as(&dir, &bin, "tester", &["task", "complete", &id, "--json"])
            .status
            .success()
    );

    let db = rusqlite::Connection::open(dir.join(".git/carryctx/state.sqlite")).unwrap();
    db.execute_batch("PRAGMA foreign_keys=OFF; DROP TRIGGER events_reject_delete;")
        .unwrap();
    db.execute("DELETE FROM events WHERE task_id = (SELECT id FROM tasks WHERE display_id = ?1) AND type = 'task.completed'", [&id]).unwrap();
    db.execute(
        "UPDATE tasks SET owner_agent_id = 'tester' WHERE display_id = ?1",
        [&id],
    )
    .unwrap();
    db.execute_batch("PRAGMA foreign_keys=ON; CREATE TRIGGER events_reject_delete BEFORE DELETE ON events BEGIN SELECT RAISE(ABORT, 'events are append-only'); END;").unwrap();
    drop(db);

    let corrected = common::run_cmd_as(
        &dir,
        &bin,
        "tester",
        &[
            "task",
            "edit",
            &id,
            "--title",
            "owner corrected",
            "--force",
            "--json",
        ],
    );
    assert!(
        corrected.status.success(),
        "{}",
        String::from_utf8_lossy(&corrected.stderr)
    );
}

#[test]
fn test_force_authorizes_terminal_actor_after_more_than_200_later_events() {
    let (dir, bin) = common::setup_test_project("edit_terminal_event_history");
    common::init_and_agent(&dir, &bin);
    common::run_cmd(
        &dir,
        &bin,
        &["agent", "register", "--name", "other", "--provider", "test"],
    );
    common::run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", "long history", "--json"],
    );
    let id = task_display_id(&dir, &bin, "long history");
    assert!(
        common::run_cmd_as(&dir, &bin, "tester", &["task", "claim", &id, "--json"])
            .status
            .success()
    );
    assert!(
        common::run_cmd_as(&dir, &bin, "tester", &["task", "complete", &id, "--json"])
            .status
            .success()
    );

    let db = rusqlite::Connection::open(dir.join(".git/carryctx/state.sqlite")).unwrap();
    let task_id: String = db
        .query_row("SELECT id FROM tasks WHERE display_id = ?1", [&id], |row| {
            row.get(0)
        })
        .unwrap();
    let project_id: String = db
        .query_row(
            "SELECT project_id FROM tasks WHERE id = ?1",
            [&task_id],
            |row| row.get(0),
        )
        .unwrap();
    let other_id: String = db
        .query_row("SELECT id FROM agents WHERE name = 'other'", [], |row| {
            row.get(0)
        })
        .unwrap();
    for index in 0..201 {
        db.execute(
            "INSERT INTO events (id, project_id, type, aggregate_type, aggregate_id, payload_json, actor_agent_id, task_id, occurred_at) VALUES (?1, ?2, 'task.completed', 'task', ?3, '{}', ?4, ?3, ?5)",
            rusqlite::params![format!("01HISTORY{index:016}"), project_id, task_id, other_id, format!("2026-01-02T00:00:{index:02}Z")],
        ).unwrap();
    }
    drop(db);

    let corrected = common::run_cmd_as(
        &dir,
        &bin,
        "tester",
        &[
            "task",
            "edit",
            &id,
            "--title",
            "history corrected",
            "--force",
            "--json",
        ],
    );
    assert!(
        corrected.status.success(),
        "{}",
        String::from_utf8_lossy(&corrected.stderr)
    );
}

#[test]
fn test_edit_can_clear_optional_fields() {
    let (dir, bin) = common::setup_test_project("edit_clear_optional");
    common::init_and_agent(&dir, &bin);

    let create = common::run_cmd(
        &dir,
        &bin,
        &[
            "task",
            "create",
            "--title",
            "clearable",
            "--description",
            "original text",
            "--required-role",
            "reviewer",
            "--json",
        ],
    );
    assert!(create.status.success(), "create failed");
    let id = task_display_id(&dir, &bin, "clearable");

    // Empty string clears; previously these could never be unset.
    let clear = common::run_cmd(
        &dir,
        &bin,
        &[
            "task",
            "edit",
            &id,
            "--description",
            "",
            "--required-role",
            "",
            "--json",
        ],
    );
    assert!(
        clear.status.success(),
        "clearing optional fields must succeed: {}",
        String::from_utf8_lossy(&clear.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&clear.stdout)).expect("valid json");
    assert_eq!(value["data"]["description"], serde_json::Value::Null);
    assert_eq!(value["data"]["required_role"], serde_json::Value::Null);

    // Omitting the flags keeps values: clearing again is a no-op.
    let keep = common::run_cmd(
        &dir,
        &bin,
        &["task", "edit", &id, "--priority", "high", "--json"],
    );
    assert!(keep.status.success());
    let value: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&keep.stdout)).expect("valid json");
    assert_eq!(value["data"]["description"], serde_json::Value::Null);
}

#[test]
fn test_edit_audit_event_records_required_role() {
    let (dir, bin) = common::setup_test_project("edit_audit_role");
    common::init_and_agent(&dir, &bin);

    let create = common::run_cmd(
        &dir,
        &bin,
        &[
            "task",
            "create",
            "--title",
            "audited",
            "--required-role",
            "implementer",
            "--json",
        ],
    );
    assert!(create.status.success(), "create failed");
    let id = task_display_id(&dir, &bin, "audited");

    let edit = common::run_cmd(
        &dir,
        &bin,
        &["task", "edit", &id, "--required-role", "reviewer", "--json"],
    );
    assert!(edit.status.success(), "edit failed");

    let events = common::run_cmd(
        &dir,
        &bin,
        &["event", "list", "--event-type", "task.edited", "--json"],
    );
    assert!(events.status.success(), "event list failed");
    let value: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&events.stdout)).expect("valid json");
    let payload = &value["data"]["events"][0]["payload"];
    assert_eq!(payload["before"]["required_role"], "implementer");
    assert_eq!(payload["after"]["required_role"], "reviewer");
    assert!(
        payload.get("forced").is_none(),
        "ordinary task.edited payload must retain its historical shape"
    );
}

#[test]
fn test_title_length_cap_rejected() {
    let (dir, bin) = common::setup_test_project("title_cap");
    common::init_and_agent(&dir, &bin);

    let long_title = "x".repeat(201);
    let out = common::run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", &long_title, "--json"],
    );
    assert!(!out.status.success(), "201-char title must be rejected");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("200"),
        "error must mention the cap: {stderr}"
    );

    // Exactly at the cap is fine.
    let ok_title = "y".repeat(200);
    let out = common::run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", &ok_title, "--json"],
    );
    assert!(
        out.status.success(),
        "200-char title must be accepted: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// CTX-0072 / issue #105: actor identity consistency — transitions and edits
/// store the resolved internal ULID in audit events, not the raw agent name.
#[test]
fn test_transition_events_store_canonical_actor_ulid() {
    let (dir, bin) = common::setup_test_project("actor_canonical");
    common::init_and_agent(&dir, &bin);

    common::run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", "tracked", "--json"],
    );
    let id = task_display_id(&dir, &bin, "tracked");

    // Act under a registered name: claim already resolved its actor, but
    // start/edit historically persisted the raw name into audit events.
    let start = common::run_cmd_as(&dir, &bin, "tester", &["task", "start", &id, "--json"]);
    assert!(start.status.success(), "start failed");
    let edit = common::run_cmd_as(
        &dir,
        &bin,
        "tester",
        &["task", "edit", &id, "--priority", "high", "--json"],
    );
    assert!(edit.status.success(), "edit failed");

    let events = common::run_cmd(
        &dir,
        &bin,
        &["event", "list", "--agent", "tester", "--json"],
    );
    assert!(events.status.success(), "event list failed");
    let value: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&events.stdout)).expect("valid json");
    let events = value["data"]["events"].as_array().expect("events array");
    assert!(!events.is_empty(), "expected transition/edit events");

    // Every actor id must be the tester ULID, never the raw name.
    let whoami = common::run_cmd(&dir, &bin, &["agent", "show", "tester", "--json"]);
    assert!(
        whoami.status.success(),
        "agent show failed: {}",
        String::from_utf8_lossy(&whoami.stderr)
    );
    let ulid: String =
        serde_json::from_str::<serde_json::Value>(&String::from_utf8_lossy(&whoami.stdout))
            .expect("valid agent json")["data"]["id"]
            .as_str()
            .expect("agent id")
            .to_string();

    for event in events {
        let actor = event["actor_agent_id"].as_str().unwrap_or_default();
        assert_eq!(
            actor, ulid,
            "audit event must carry the canonical ULID, got '{actor}'"
        );
    }
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
        .unwrap_or_else(|| panic!("task with title '{title}' not found in {value}"))["display_id"]
        .as_str()
        .expect("display id")
        .to_string()
}
