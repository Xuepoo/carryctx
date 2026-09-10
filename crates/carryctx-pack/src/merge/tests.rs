//! Fixture matrix for the pure merge engine (design §6, CTX-0141).
//!
//! Every fixture is a small in-memory [`TableSet`]; no I/O is involved. The
//! commutativity property ignores machine-local columns and the vectors that
//! are expressed relative to `ours` (`writes`, `deletes`, `renumbers`,
//! `aliases`), because those are intentionally orientation-dependent.

use super::identity::machine_local_columns;
use super::{MergeOptions, MergeReport, TableSet, merge_tables};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn table_set(entries: &[(&str, Vec<Value>)]) -> TableSet {
    entries
        .iter()
        .map(|(table, rows)| ((*table).to_string(), rows.clone()))
        .collect()
}

fn set(table: &str, rows: Vec<Value>) -> TableSet {
    table_set(&[(table, rows)])
}

fn task(id: &str, title: &str, status: &str, updated_at: &str) -> Value {
    json!({
        "id": id,
        "project_id": "01PROJECT",
        "display_id": format!("CTX-{id}"),
        "title": title,
        "status": status,
        "updated_at": updated_at,
    })
}

fn session(id: &str, state: &str, ended_at: Option<&str>, updated_at: &str) -> Value {
    json!({
        "id": id,
        "project_id": "01PROJECT",
        "agent_id": "01AGENT",
        "state": state,
        "working_directory": "/local",
        "ended_at": ended_at,
        "updated_at": updated_at,
    })
}

fn session_in(
    id: &str,
    state: &str,
    ended_at: Option<&str>,
    updated_at: &str,
    working_directory: &str,
) -> Value {
    let mut row = session(id, state, ended_at, updated_at);
    row["working_directory"] = Value::String(working_directory.to_string());
    row
}

fn tombstone(table: &str, row_id: &str, deleted_at: &str) -> Value {
    json!({
        "project_id": "01PROJECT",
        "table_name": table,
        "row_id": row_id,
        "deleted_at": deleted_at,
        "deleted_by": "tester",
    })
}

fn event(id: &str, note: &str) -> Value {
    json!({
        "id": id,
        "project_id": "01PROJECT",
        "type": "progress.noted",
        "occurred_at": "2026-01-01T00:00:00Z",
        "note": note,
    })
}

fn find<'a>(report: &'a MergeReport, table: &str, id: &str) -> Option<&'a Value> {
    report
        .result
        .get(table)?
        .iter()
        .find(|row| row.get("id").and_then(Value::as_str) == Some(id))
}

fn tombstone_for<'a>(report: &'a MergeReport, table: &str, row_id: &str) -> Option<&'a Value> {
    report.result.get("tombstones")?.iter().find(|row| {
        row.get("table_name").and_then(Value::as_str) == Some(table)
            && row.get("row_id").and_then(Value::as_str) == Some(row_id)
    })
}

fn semantic_result(report: &MergeReport) -> TableSet {
    report
        .result
        .iter()
        .map(|(table, rows)| {
            let stripped = rows
                .iter()
                .map(|row| {
                    let mut map = row.as_object().cloned().unwrap_or_default();
                    for column in machine_local_columns(table) {
                        map.remove(*column);
                    }
                    Value::Object(map)
                })
                .collect();
            (table.clone(), stripped)
        })
        .collect()
}

fn conflict_ids(report: &MergeReport) -> Vec<String> {
    report.conflicts.iter().map(|c| c.id.clone()).collect()
}

// ---------------------------------------------------------------------------
// Update semantics
// ---------------------------------------------------------------------------

#[test]
fn unchanged_rows_are_a_noop() {
    let base = set(
        "tasks",
        vec![task("01A", "x", "planned", "2026-01-01T00:00:00Z")],
    );
    let report = merge_tables(Some(&base), &base, &base, &MergeOptions::default()).unwrap();
    assert_eq!(report.result["tasks"], base["tasks"]);
    assert!(report.conflicts.is_empty());
    assert!(report.auto_resolutions.is_empty());
    assert!(report.writes.is_empty());
    assert!(report.deletes.is_empty());
    assert!(!report.degraded);
}

#[test]
fn one_sided_edit_takes_the_changed_side_without_a_resolution() {
    let base = set(
        "tasks",
        vec![task("01A", "base", "planned", "2026-01-01T00:00:00Z")],
    );
    let theirs = set(
        "tasks",
        vec![task("01A", "changed", "planned", "2026-01-02T00:00:00Z")],
    );
    let report = merge_tables(Some(&base), &base, &theirs, &MergeOptions::default()).unwrap();
    assert_eq!(find(&report, "tasks", "01A").unwrap()["title"], "changed");
    assert!(report.conflicts.is_empty());
    assert!(report.auto_resolutions.is_empty());
    assert_eq!(report.writes.len(), 1);
    assert_eq!(report.writes[0].kind, super::WriteKind::Update);
}

#[test]
fn both_sided_edit_elects_the_newer_updated_at() {
    let base = set(
        "tasks",
        vec![task("01A", "base", "planned", "2026-01-01T00:00:00Z")],
    );
    let ours = set(
        "tasks",
        vec![task("01A", "ours", "planned", "2026-01-03T00:00:00Z")],
    );
    let theirs = set(
        "tasks",
        vec![task("01A", "theirs", "planned", "2026-01-02T00:00:00Z")],
    );
    let report = merge_tables(Some(&base), &ours, &theirs, &MergeOptions::default()).unwrap();
    assert_eq!(find(&report, "tasks", "01A").unwrap()["title"], "ours");
    assert!(report.conflicts.is_empty());
    assert_eq!(report.auto_resolutions.len(), 1);
    assert_eq!(report.auto_resolutions[0].kind, "row_edit");
}

#[test]
fn equal_timestamp_tie_break_is_independent_of_side_order() {
    let base = set(
        "tasks",
        vec![task("01A", "base", "planned", "2026-01-01T00:00:00Z")],
    );
    let ours = set(
        "tasks",
        vec![task("01A", "alpha", "planned", "2026-01-02T00:00:00Z")],
    );
    let theirs = set(
        "tasks",
        vec![task("01A", "beta", "planned", "2026-01-02T00:00:00Z")],
    );
    let first = merge_tables(Some(&base), &ours, &theirs, &MergeOptions::default()).unwrap();
    let second = merge_tables(Some(&base), &theirs, &ours, &MergeOptions::default()).unwrap();
    assert_eq!(first.result["tasks"], second.result["tasks"]);
    assert_eq!(first.auto_resolutions, second.auto_resolutions);
    assert_eq!(first.conflicts, second.conflicts);
}

#[test]
fn null_union_keeps_a_set_monotonic_fact_over_a_newer_null() {
    let base = set(
        "sessions",
        vec![session("01S", "active", None, "2026-01-01T00:00:00Z")],
    );
    let ours = set(
        "sessions",
        vec![session(
            "01S",
            "ended",
            Some("2026-01-02T00:00:00Z"),
            "2026-01-02T00:00:00Z",
        )],
    );
    let theirs = set(
        "sessions",
        vec![session_in(
            "01S",
            "ended",
            None,
            "2026-01-05T00:00:00Z",
            "/theirs",
        )],
    );
    let report = merge_tables(Some(&base), &ours, &theirs, &MergeOptions::default()).unwrap();
    let elected = find(&report, "sessions", "01S").unwrap();
    assert_eq!(elected["ended_at"], "2026-01-02T00:00:00Z");
    assert_eq!(elected["working_directory"], "/local");
}

#[test]
fn machine_local_columns_are_never_overwritten_by_a_theirs_only_edit() {
    let base = set(
        "sessions",
        vec![session_in(
            "01S",
            "active",
            None,
            "2026-01-01T00:00:00Z",
            "/base",
        )],
    );
    let ours = set(
        "sessions",
        vec![session_in(
            "01S",
            "active",
            None,
            "2026-01-01T00:00:00Z",
            "/ours",
        )],
    );
    let theirs = set(
        "sessions",
        vec![session_in(
            "01S",
            "paused",
            None,
            "2026-01-04T00:00:00Z",
            "/theirs",
        )],
    );
    let report = merge_tables(Some(&base), &ours, &theirs, &MergeOptions::default()).unwrap();
    let elected = find(&report, "sessions", "01S").unwrap();
    assert_eq!(elected["state"], "paused");
    assert_eq!(elected["working_directory"], "/ours");
}

#[test]
fn terminal_status_beats_a_newer_open_status() {
    let base = set(
        "tasks",
        vec![task("01A", "base", "in_progress", "2026-01-01T00:00:00Z")],
    );
    let ours = set(
        "tasks",
        vec![task("01A", "ours", "in_progress", "2026-01-09T00:00:00Z")],
    );
    let theirs = set(
        "tasks",
        vec![task("01A", "theirs", "completed", "2026-01-02T00:00:00Z")],
    );
    let report = merge_tables(Some(&base), &ours, &theirs, &MergeOptions::default()).unwrap();
    assert_eq!(
        find(&report, "tasks", "01A").unwrap()["status"],
        "completed"
    );
    assert!(report.conflicts.is_empty());
    assert_eq!(report.auto_resolutions.len(), 1);
    assert_eq!(report.auto_resolutions[0].kind, "status_terminal");
}

#[test]
fn completed_versus_cancelled_is_a_status_gap_conflict() {
    let base = set(
        "tasks",
        vec![task("01A", "base", "planned", "2026-01-01T00:00:00Z")],
    );
    let ours = set(
        "tasks",
        vec![task("01A", "ours", "completed", "2026-01-02T00:00:00Z")],
    );
    let theirs = set(
        "tasks",
        vec![task("01A", "theirs", "cancelled", "2026-01-03T00:00:00Z")],
    );
    let report = merge_tables(Some(&base), &ours, &theirs, &MergeOptions::default()).unwrap();
    assert_eq!(report.conflicts.len(), 1);
    assert_eq!(report.conflicts[0].kind, "status_gap");
    assert_eq!(
        find(&report, "tasks", "01A").unwrap()["status"],
        "completed"
    );
}

#[test]
fn strict_edits_promotes_both_changed_edit_to_conflict() {
    let base = set(
        "tasks",
        vec![task("01A", "base", "planned", "2026-01-01T00:00:00Z")],
    );
    let ours = set(
        "tasks",
        vec![task("01A", "ours", "planned", "2026-01-02T00:00:00Z")],
    );
    let theirs = set(
        "tasks",
        vec![task("01A", "theirs", "planned", "2026-01-03T00:00:00Z")],
    );
    let strict = MergeOptions { strict_edits: true };
    let report = merge_tables(Some(&base), &ours, &theirs, &strict).unwrap();
    assert_eq!(report.conflicts.len(), 1);
    assert_eq!(report.conflicts[0].kind, "row_edit");
    assert!(report.auto_resolutions.is_empty());
    assert_eq!(find(&report, "tasks", "01A").unwrap()["title"], "ours");
}

#[test]
fn immutable_table_union_is_clean_and_differences_conflict() {
    let row = |kind: &str| {
        json!({
            "id": "01DEP",
            "project_id": "01PROJECT",
            "task_id": "01T1",
            "prerequisite_task_id": "01T2",
            "kind": kind,
            "created_at": "2026-01-01T00:00:00Z",
        })
    };
    let base = set("task_dependencies", vec![row("strong")]);
    let identical = merge_tables(
        Some(&base),
        &set("task_dependencies", vec![row("strong")]),
        &set("task_dependencies", vec![row("strong")]),
        &MergeOptions::default(),
    )
    .unwrap();
    assert!(identical.conflicts.is_empty());
    assert_eq!(identical.result["task_dependencies"].len(), 1);

    let differing = merge_tables(
        Some(&base),
        &set("task_dependencies", vec![row("strong")]),
        &set("task_dependencies", vec![row("informational")]),
        &MergeOptions::default(),
    )
    .unwrap();
    assert_eq!(differing.conflicts.len(), 1);
    assert_eq!(differing.conflicts[0].kind, "immutable_edit");
    assert_eq!(
        "strong",
        find(&differing, "task_dependencies", "01DEP").unwrap()["kind"]
    );
}

#[test]
fn composite_identity_merges_team_members_by_key() {
    let member = |role: &str, updated_at: &str| {
        json!({
            "project_id": "01PROJECT",
            "team_id": "01TEAM",
            "agent_id": "01AGENT",
            "role": role,
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": updated_at,
        })
    };
    let base = set("team_members", vec![member("dev", "2026-01-01T00:00:00Z")]);
    let ours = set("team_members", vec![member("dev", "2026-01-01T00:00:00Z")]);
    let theirs = set("team_members", vec![member("lead", "2026-01-05T00:00:00Z")]);
    let report = merge_tables(Some(&base), &ours, &theirs, &MergeOptions::default()).unwrap();
    assert_eq!(report.result["team_members"].len(), 1);
    assert_eq!(report.result["team_members"][0]["role"], "lead");
}

// ---------------------------------------------------------------------------
// Tombstones
// ---------------------------------------------------------------------------

#[test]
fn tombstone_on_ours_with_unchanged_theirs_deletes() {
    let base = set(
        "tasks",
        vec![task("01A", "base", "planned", "2026-01-01T00:00:00Z")],
    );
    let ours = table_set(&[
        ("tasks", vec![]),
        (
            "tombstones",
            vec![tombstone("tasks", "01A", "2026-01-02T00:00:00Z")],
        ),
    ]);
    let report = merge_tables(Some(&base), &ours, &base, &MergeOptions::default()).unwrap();
    assert!(report.result["tasks"].is_empty());
    // `ours` already lacks the row, so there is nothing to delete locally;
    // the tombstone in `result` carries the deletion forward.
    assert!(report.deletes.is_empty());
    assert!(report.conflicts.is_empty());
    assert!(tombstone_for(&report, "tasks", "01A").is_some());
}

#[test]
fn tombstone_versus_edit_on_the_other_side_conflicts() {
    let base = set(
        "tasks",
        vec![task("01A", "base", "planned", "2026-01-01T00:00:00Z")],
    );
    let ours = table_set(&[
        ("tasks", vec![]),
        (
            "tombstones",
            vec![tombstone("tasks", "01A", "2026-01-02T00:00:00Z")],
        ),
    ]);
    let theirs = set(
        "tasks",
        vec![task("01A", "edited", "planned", "2026-01-03T00:00:00Z")],
    );
    let report = merge_tables(Some(&base), &ours, &theirs, &MergeOptions::default()).unwrap();
    assert_eq!(report.conflicts.len(), 1);
    assert_eq!(report.conflicts[0].kind, "delete_vs_edit");
    // Blocking conflicts leave the key at ours (deleted here).
    assert!(report.result["tasks"].is_empty());
    assert!(tombstone_for(&report, "tasks", "01A").is_some());
}

#[test]
fn both_sides_delete_and_the_earliest_deleted_at_wins() {
    let base = set(
        "tasks",
        vec![task("01A", "base", "planned", "2026-01-01T00:00:00Z")],
    );
    let ours = table_set(&[
        ("tasks", vec![]),
        (
            "tombstones",
            vec![tombstone("tasks", "01A", "2026-01-05T00:00:00Z")],
        ),
    ]);
    let theirs = table_set(&[
        ("tasks", vec![]),
        (
            "tombstones",
            vec![tombstone("tasks", "01A", "2026-01-02T00:00:00Z")],
        ),
    ]);
    let report = merge_tables(Some(&base), &ours, &theirs, &MergeOptions::default()).unwrap();
    assert!(report.conflicts.is_empty());
    let kept = tombstone_for(&report, "tasks", "01A").unwrap();
    assert_eq!(kept["deleted_at"], "2026-01-02T00:00:00Z");
}

#[test]
fn tombstone_on_theirs_with_unchanged_ours_deletes() {
    let base = set(
        "tasks",
        vec![task("01A", "base", "planned", "2026-01-01T00:00:00Z")],
    );
    let theirs = table_set(&[
        ("tasks", vec![]),
        (
            "tombstones",
            vec![tombstone("tasks", "01A", "2026-01-02T00:00:00Z")],
        ),
    ]);
    let report = merge_tables(Some(&base), &base, &theirs, &MergeOptions::default()).unwrap();
    assert!(report.result["tasks"].is_empty());
    assert!(report.conflicts.is_empty());
    assert_eq!(report.deletes.len(), 1);
}

#[test]
fn edited_ours_versus_unchanged_theirs_keeps_the_edit() {
    let base = set(
        "tasks",
        vec![task("01A", "base", "planned", "2026-01-01T00:00:00Z")],
    );
    let ours = set(
        "tasks",
        vec![task("01A", "edited", "planned", "2026-01-02T00:00:00Z")],
    );
    let report = merge_tables(Some(&base), &ours, &base, &MergeOptions::default()).unwrap();
    assert_eq!(find(&report, "tasks", "01A").unwrap()["title"], "edited");
    assert!(report.conflicts.is_empty());
}

#[test]
fn base_absent_tombstone_versus_present_is_delete_vs_edit() {
    let base = set("tasks", vec![]);
    let ours = table_set(&[
        ("tasks", vec![]),
        (
            "tombstones",
            vec![tombstone("tasks", "01A", "2026-01-02T00:00:00Z")],
        ),
    ]);
    let theirs = set(
        "tasks",
        vec![task("01A", "late", "planned", "2026-01-03T00:00:00Z")],
    );
    let report = merge_tables(Some(&base), &ours, &theirs, &MergeOptions::default()).unwrap();
    assert_eq!(report.conflicts.len(), 1);
    assert_eq!(report.conflicts[0].kind, "delete_vs_edit");
}

#[test]
fn base_absent_tombstone_alone_is_carried_forward() {
    let base = set("tasks", vec![]);
    let ours = table_set(&[
        ("tasks", vec![]),
        (
            "tombstones",
            vec![tombstone("tasks", "01A", "2026-01-02T00:00:00Z")],
        ),
    ]);
    let theirs = set("tasks", vec![]);
    let report = merge_tables(Some(&base), &ours, &theirs, &MergeOptions::default()).unwrap();
    assert!(report.conflicts.is_empty());
    assert!(tombstone_for(&report, "tasks", "01A").is_some());
}

// ---------------------------------------------------------------------------
// Append-only tables
// ---------------------------------------------------------------------------

#[test]
fn append_only_tables_union_and_dedupe_by_id() {
    let ours = set("events", vec![event("E1", "a"), event("E2", "b")]);
    let theirs = set("events", vec![event("E2", "b"), event("E3", "c")]);
    let report = merge_tables(None, &ours, &theirs, &MergeOptions::default()).unwrap();
    let ids: Vec<&str> = report.result["events"]
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["E1", "E2", "E3"]);
}

#[test]
fn append_only_tamper_fails_validation() {
    let ours = set("events", vec![event("E1", "original")]);
    let theirs = set("events", vec![event("E1", "tampered")]);
    let error = merge_tables(None, &ours, &theirs, &MergeOptions::default()).unwrap_err();
    assert_eq!(error.code, "VALIDATION_FAILED");
}

// ---------------------------------------------------------------------------
// Sequences
// ---------------------------------------------------------------------------

#[test]
fn sequences_keep_the_max_and_never_rewind() {
    let ours = table_set(&[(
        "sequences",
        vec![
            json!({"project_id": "01PROJECT", "kind": "display_id_CTX", "next_value": 5}),
            json!({"project_id": "01PROJECT", "kind": "display_id_decision", "next_value": 3}),
        ],
    )]);
    let theirs = table_set(&[(
        "sequences",
        vec![json!({"project_id": "01PROJECT", "kind": "display_id_CTX", "next_value": 9})],
    )]);
    let report = merge_tables(None, &ours, &theirs, &MergeOptions::default()).unwrap();
    let value = |kind: &str| {
        report.result["sequences"]
            .iter()
            .find(|row| row["kind"] == kind)
            .map(|row| row["next_value"].as_u64().unwrap())
    };
    assert_eq!(value("display_id_CTX"), Some(9));
    assert_eq!(value("display_id_decision"), Some(3));
}

// ---------------------------------------------------------------------------
// Identity artifacts
// ---------------------------------------------------------------------------

#[test]
fn display_id_collision_plans_a_renumber_and_bumps_sequences() {
    let ours = set(
        "tasks",
        vec![json!({
            "id": "01A", "project_id": "01PROJECT", "display_id": "CTX-0001",
            "title": "a", "status": "planned", "updated_at": "2026-01-01T00:00:00Z",
        })],
    );
    let theirs = set(
        "tasks",
        vec![json!({
            "id": "01B", "project_id": "01PROJECT", "display_id": "CTX-0001",
            "title": "b", "status": "planned", "updated_at": "2026-01-01T00:00:00Z",
        })],
    );
    let report = merge_tables(None, &ours, &theirs, &MergeOptions::default()).unwrap();
    assert_eq!(report.result["tasks"].len(), 2);
    assert_eq!(report.renumbers.len(), 1);
    let renumber = &report.renumbers[0];
    assert_eq!(renumber.row_id, "01B");
    assert_eq!(renumber.display_id, "CTX-0001");
    assert_eq!(renumber.new_display_id, "CTX-0002");
    let next = report.result["sequences"][0]["next_value"]
        .as_u64()
        .unwrap();
    assert!(next >= 3);
    assert!(report.warnings.iter().any(|w| w.contains("CTX-0001")));
}

#[test]
fn agent_name_collision_aliases_incoming_and_remaps_references() {
    let ours = set(
        "agents",
        vec![json!({
            "id": "01AGENTA", "project_id": "01PROJECT", "name": "alice",
            "updated_at": "2026-01-01T00:00:00Z",
        })],
    );
    let theirs = table_set(&[
        (
            "agents",
            vec![json!({
                "id": "01AGENTB", "project_id": "01PROJECT", "name": "alice",
                "updated_at": "2026-01-01T00:00:00Z",
            })],
        ),
        (
            "sessions",
            vec![json!({
                "id": "01S", "project_id": "01PROJECT", "agent_id": "01AGENTB",
                "state": "active", "working_directory": "/x",
                "updated_at": "2026-01-01T00:00:00Z",
            })],
        ),
    ]);
    let report = merge_tables(None, &ours, &theirs, &MergeOptions::default()).unwrap();
    assert_eq!(report.result["agents"].len(), 1);
    assert_eq!(report.result["agents"][0]["id"], "01AGENTA");
    assert_eq!(report.result["sessions"][0]["agent_id"], "01AGENTA");
    assert_eq!(report.aliases.len(), 1);
    assert_eq!(report.aliases[0].incoming_agent_id, "01AGENTB");
    assert_eq!(report.aliases[0].existing_agent_id, "01AGENTA");
    assert!(
        report.aliases[0]
            .remapped_references
            .iter()
            .any(|remap| remap.column == "agent_id" && remap.row_id == "01S")
    );
}

#[test]
fn team_name_collision_is_a_blocking_unique_key_conflict() {
    let ours = set(
        "teams",
        vec![json!({
            "id": "01TEAM1", "project_id": "01PROJECT", "name": "core",
            "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z",
        })],
    );
    let theirs = set(
        "teams",
        vec![json!({
            "id": "01TEAM2", "project_id": "01PROJECT", "name": "core",
            "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z",
        })],
    );
    let report = merge_tables(None, &ours, &theirs, &MergeOptions::default()).unwrap();
    assert_eq!(report.conflicts.len(), 1);
    assert_eq!(report.conflicts[0].kind, "unique_key");
    assert_eq!(report.result["teams"].len(), 1);
    assert_eq!(report.result["teams"][0]["id"], "01TEAM1");
}

// ---------------------------------------------------------------------------
// Determinism properties
// ---------------------------------------------------------------------------

fn assert_commutative(base: Option<&TableSet>, ours: &TableSet, theirs: &TableSet) {
    let first = merge_tables(base, ours, theirs, &MergeOptions::default()).unwrap();
    let second = merge_tables(base, theirs, ours, &MergeOptions::default()).unwrap();
    assert_eq!(semantic_result(&first), semantic_result(&second));
    assert_eq!(conflict_ids(&first), conflict_ids(&second));
    assert_eq!(first.auto_resolutions, second.auto_resolutions);
}

#[test]
fn merge_is_commutative_over_several_fixtures() {
    // Disjoint one-sided edits.
    let base = set(
        "tasks",
        vec![
            task("01A", "a0", "planned", "2026-01-01T00:00:00Z"),
            task("01B", "b0", "planned", "2026-01-01T00:00:00Z"),
        ],
    );
    let ours = set(
        "tasks",
        vec![
            task("01A", "a1", "planned", "2026-01-03T00:00:00Z"),
            task("01B", "b0", "planned", "2026-01-01T00:00:00Z"),
        ],
    );
    let theirs = set(
        "tasks",
        vec![
            task("01A", "a0", "planned", "2026-01-01T00:00:00Z"),
            task("01B", "b1", "planned", "2026-01-04T00:00:00Z"),
        ],
    );
    assert_commutative(Some(&base), &ours, &theirs);

    // Both-sided edit (LWW) and equal-timestamp tie-break.
    let ours = set(
        "tasks",
        vec![task("01A", "ours", "planned", "2026-01-05T00:00:00Z")],
    );
    let theirs = set(
        "tasks",
        vec![task("01A", "theirs", "planned", "2026-01-04T00:00:00Z")],
    );
    assert_commutative(Some(&base), &ours, &theirs);

    // NULL-union on a monotonic fact.
    let sbase = set(
        "sessions",
        vec![session("01S", "active", None, "2026-01-01T00:00:00Z")],
    );
    let ours = set(
        "sessions",
        vec![session(
            "01S",
            "ended",
            Some("2026-01-02T00:00:00Z"),
            "2026-01-02T00:00:00Z",
        )],
    );
    let theirs = set(
        "sessions",
        vec![session("01S", "ended", None, "2026-01-06T00:00:00Z")],
    );
    assert_commutative(Some(&sbase), &ours, &theirs);

    // Symmetric tombstone delete.
    let base = set(
        "tasks",
        vec![task("01A", "base", "planned", "2026-01-01T00:00:00Z")],
    );
    let ours = table_set(&[
        ("tasks", vec![]),
        (
            "tombstones",
            vec![tombstone("tasks", "01A", "2026-01-02T00:00:00Z")],
        ),
    ]);
    assert_commutative(Some(&base), &ours, &base);

    // Sequence floors.
    let ours = table_set(&[(
        "sequences",
        vec![json!({"project_id": "01PROJECT", "kind": "display_id_CTX", "next_value": 7})],
    )]);
    let theirs = table_set(&[(
        "sequences",
        vec![json!({"project_id": "01PROJECT", "kind": "display_id_CTX", "next_value": 11})],
    )]);
    assert_commutative(None, &ours, &theirs);
}

#[test]
fn conflict_sets_are_commutative() {
    let base = set(
        "tasks",
        vec![task("01A", "base", "planned", "2026-01-01T00:00:00Z")],
    );
    let ours = table_set(&[
        ("tasks", vec![]),
        (
            "tombstones",
            vec![tombstone("tasks", "01A", "2026-01-02T00:00:00Z")],
        ),
    ]);
    let theirs = set(
        "tasks",
        vec![task("01A", "edited", "planned", "2026-01-03T00:00:00Z")],
    );
    // Blocking conflicts leave the key at `ours`, so the result is
    // orientation-dependent; the conflict identities and auto-resolutions
    // are not.
    let first = merge_tables(Some(&base), &ours, &theirs, &MergeOptions::default()).unwrap();
    let second = merge_tables(Some(&base), &theirs, &ours, &MergeOptions::default()).unwrap();
    assert_eq!(conflict_ids(&first), conflict_ids(&second));
    assert_eq!(first.auto_resolutions, second.auto_resolutions);
}

#[test]
fn merge_is_idempotent_with_and_without_a_base() {
    let mut m = BTreeMap::new();
    m.insert(
        "tasks".to_string(),
        vec![task("01A", "x", "planned", "2026-01-01T00:00:00Z")],
    );
    m.insert(
        "sequences".to_string(),
        vec![json!({"project_id": "01PROJECT", "kind": "display_id_CTX", "next_value": 5})],
    );
    m.insert(
        "tombstones".to_string(),
        vec![tombstone("tasks", "01GONE", "2026-01-01T00:00:00Z")],
    );

    let with_base = merge_tables(Some(&m), &m, &m, &MergeOptions::default()).unwrap();
    assert_eq!(with_base.result, m);
    assert!(with_base.conflicts.is_empty());
    assert!(with_base.auto_resolutions.is_empty());
    assert!(with_base.writes.is_empty());
    assert!(with_base.deletes.is_empty());
    assert!(!with_base.degraded);

    let without_base = merge_tables(None, &m, &m, &MergeOptions::default()).unwrap();
    assert_eq!(without_base.result, m);
    assert!(without_base.conflicts.is_empty());
    assert!(without_base.auto_resolutions.is_empty());
    assert!(without_base.degraded);
}
