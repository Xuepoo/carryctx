//! CTX-0140: schema 0018 `tombstones` + `snapshot_state` and delete-path
//! coverage (design `2026-09-10-mergeable-git-managed-state.md` §1.3).
//!
//! Every hard-delete path must append a tombstone for each removed row in the
//! same transaction as the delete: scope remove, dependency remove, worktree
//! registration delete, stale-worktree prune, team-member removal, and
//! `project prune` with all of its cascaded children. Tombstones are a side
//! table: ordinary read paths and command output must stay unchanged.
//!
//! The delete-then-parity assertions below compare the tombstone set against
//! the row ids that actually disappeared, which is the storage half of the
//! format-level delete-then-export parity test owned by CTX-0139.

mod common;

use std::path::{Path, PathBuf};

use rusqlite::Connection;

fn open_state(dir: &Path) -> Connection {
    Connection::open(dir.join(".git/carryctx/state.sqlite")).expect("state database should open")
}

fn scalar(db: &Connection, sql: &str) -> i64 {
    db.query_row(sql, [], |row| row.get(0))
        .unwrap_or_else(|error| panic!("query failed: {sql}: {error}"))
}

fn scalar_param<P: rusqlite::Params>(db: &Connection, sql: &str, params: P) -> i64 {
    db.query_row(sql, params, |row| row.get(0))
        .unwrap_or_else(|error| panic!("query failed: {sql}: {error}"))
}

fn text_param<P: rusqlite::Params>(db: &Connection, sql: &str, params: P) -> String {
    db.query_row(sql, params, |row| row.get(0))
        .unwrap_or_else(|error| panic!("query failed: {sql}: {error}"))
}

fn tombstone_count(db: &Connection, table: &str) -> i64 {
    scalar_param(
        db,
        "SELECT COUNT(*) FROM tombstones WHERE table_name = ?1",
        [table],
    )
}

fn tombstone_row_ids(db: &Connection, table: &str) -> Vec<String> {
    let mut stmt = db
        .prepare("SELECT row_id FROM tombstones WHERE table_name = ?1 ORDER BY row_id")
        .unwrap();
    let rows = stmt
        .query_map([table], |row| row.get(0))
        .unwrap()
        .collect::<Result<Vec<String>, _>>()
        .unwrap();
    rows
}

fn sorted(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values
}

fn run(dir: &Path, bin: &Path, args: &[&str]) -> std::process::Output {
    let output = common::run_cmd(dir, bin, args);
    assert!(
        output.status.success(),
        "carryctx {args:?} failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn setup(name: &str, prefix: &str) -> (PathBuf, PathBuf, Connection) {
    let (dir, bin) = common::setup_test_project(name);
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", prefix]);
    run(
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
    let db = open_state(&dir);
    (dir, bin, db)
}

#[test]
fn schema_0018_installs_tombstones_and_snapshot_state() {
    let (_dir, _bin, db) = setup("tombstones_schema", "TB");

    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM schema_migrations WHERE version = 18"
        ),
        1,
        "migration 0018 must be recorded"
    );

    let table_columns = |table: &str| -> Vec<String> {
        let mut stmt = db
            .prepare("SELECT name FROM pragma_table_info(?1)")
            .unwrap();
        stmt.query_map([table], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<String>, _>>()
            .unwrap()
    };

    let tombstone_columns = table_columns("tombstones");
    for column in [
        "project_id",
        "table_name",
        "row_id",
        "deleted_at",
        "deleted_by",
        "reason",
    ] {
        assert!(
            tombstone_columns.iter().any(|name| name == column),
            "tombstones.{column} missing: {tombstone_columns:?}"
        );
    }

    let snapshot_columns = table_columns("snapshot_state");
    for column in ["project_id", "key", "value", "updated_at"] {
        assert!(
            snapshot_columns.iter().any(|name| name == column),
            "snapshot_state.{column} missing: {snapshot_columns:?}"
        );
    }

    assert_eq!(
        scalar(&db, "SELECT COUNT(*) FROM pragma_foreign_key_check"),
        0,
        "schema 0018 must not introduce foreign key violations"
    );
}

#[test]
fn entity_delete_paths_record_tombstones_for_every_removed_row() {
    let (dir, bin, db) = setup("tombstones_entity_paths", "TB");

    run(&dir, &bin, &["task", "create", "--title", "one"]);
    run(&dir, &bin, &["task", "create", "--title", "two"]);

    // Dependency remove.
    run(
        &dir,
        &bin,
        &["task", "depend", "TB-0002", "--on", "TB-0001"],
    );
    let dependency_id = text_param(&db, "SELECT id FROM task_dependencies", []);
    run(
        &dir,
        &bin,
        &["task", "undepend", "TB-0002", "--on", "TB-0001"],
    );

    // Scope remove.
    run(&dir, &bin, &["task", "scope", "add", "TB-0001", "src/**"]);
    let scope_id = text_param(&db, "SELECT id FROM scopes", []);
    run(
        &dir,
        &bin,
        &["task", "scope", "remove", "TB-0001", "src/**"],
    );

    // Team-member removal (composite key, no ULID column).
    run(&dir, &bin, &["team", "create", "--name", "core"]);
    run(
        &dir,
        &bin,
        &["team", "member", "add", "core", "--agent", "tester"],
    );
    let team_id = text_param(&db, "SELECT id FROM teams WHERE name = 'core'", []);
    let agent_id = text_param(&db, "SELECT id FROM agents WHERE name = 'tester'", []);
    run(
        &dir,
        &bin,
        &["team", "member", "remove", "core", "--agent", "tester"],
    );

    // Worktree registration removal (directory already gone -> orphan path).
    run(&dir, &bin, &["worktree", "create", "TB-0002", "--json"]);
    let removed_worktree_id = text_param(&db, "SELECT id FROM worktrees", []);
    std::fs::remove_dir_all(dir.join(".worktrees/tb-0002")).expect("fixture worktree removal");
    run(&dir, &bin, &["worktree", "remove", "TB-0002"]);

    // Stale-worktree prune (doctor --prune-stale-worktrees).
    run(&dir, &bin, &["task", "create", "--title", "three"]);
    run(&dir, &bin, &["worktree", "create", "TB-0003", "--json"]);
    let pruned_worktree_id = text_param(&db, "SELECT id FROM worktrees", []);
    std::fs::remove_dir_all(dir.join(".worktrees/tb-0003")).expect("fixture worktree removal");
    run(
        &dir,
        &bin,
        &["doctor", "--prune-stale-worktrees", "--yes", "--json"],
    );

    // Every removed row left exactly one tombstone, keyed by its identity.
    assert_eq!(tombstone_count(&db, "task_dependencies"), 1);
    assert_eq!(
        tombstone_row_ids(&db, "task_dependencies"),
        vec![dependency_id]
    );
    assert_eq!(tombstone_count(&db, "scopes"), 1);
    assert_eq!(tombstone_row_ids(&db, "scopes"), vec![scope_id]);
    assert_eq!(tombstone_count(&db, "team_members"), 1);
    let composite_key = serde_json::to_string(&[&team_id, &agent_id]).unwrap();
    assert_eq!(
        tombstone_row_ids(&db, "team_members"),
        vec![composite_key],
        "team_members uses the canonical composite row id"
    );
    assert_eq!(tombstone_count(&db, "worktrees"), 2);
    assert_eq!(
        tombstone_row_ids(&db, "worktrees"),
        sorted(vec![removed_worktree_id, pruned_worktree_id.clone()])
    );

    // The prune path records the acting agent as `deleted_by`.
    let pruned_by: Option<String> = db
        .query_row(
            "SELECT deleted_by FROM tombstones WHERE table_name = 'worktrees' AND row_id = ?1",
            [&pruned_worktree_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pruned_by, Some(agent_id));

    // The deleted rows are really gone and the database still validates.
    assert_eq!(scalar(&db, "SELECT COUNT(*) FROM task_dependencies"), 0);
    assert_eq!(scalar(&db, "SELECT COUNT(*) FROM scopes"), 0);
    assert_eq!(scalar(&db, "SELECT COUNT(*) FROM team_members"), 0);
    assert_eq!(scalar(&db, "SELECT COUNT(*) FROM worktrees"), 0);
    assert_eq!(
        scalar(&db, "SELECT COUNT(*) FROM pragma_foreign_key_check"),
        0
    );
}

#[test]
fn project_prune_records_tombstones_for_tasks_and_children() {
    let (dir, bin, db) = setup("tombstones_prune", "PR");
    let project_id = text_param(&db, "SELECT id FROM projects LIMIT 1", []);

    run(&dir, &bin, &["task", "create", "--title", "archived"]);
    run(&dir, &bin, &["task", "create", "--title", "keeper"]);
    run(
        &dir,
        &bin,
        &["progress", "note", "worked on it", "--task", "PR-0001"],
    );
    run(
        &dir,
        &bin,
        &["checkpoint", "--task", "PR-0001", "--done", "done"],
    );
    run(
        &dir,
        &bin,
        &[
            "decision",
            "add",
            "--title",
            "archived decision",
            "--task",
            "PR-0001",
        ],
    );
    run(
        &dir,
        &bin,
        &[
            "handoff",
            "create",
            "--target",
            "tester",
            "--task",
            "PR-0001",
            "--summary",
            "archived handoff",
        ],
    );
    run(&dir, &bin, &["task", "scope", "add", "PR-0001", "src/**"]);
    run(
        &dir,
        &bin,
        &["task", "depend", "PR-0002", "--on", "PR-0001"],
    );

    let task_id = text_param(&db, "SELECT id FROM tasks WHERE display_id = 'PR-0001'", []);
    let checkpoint_id = text_param(&db, "SELECT id FROM checkpoints", []);
    db.execute(
        "INSERT INTO checkpoint_corrections (id, checkpoint_id, project_id, corrected_at)
         VALUES ('cc-parity', ?1, ?2, '2020-01-01T00:00:00+00:00')",
        rusqlite::params![checkpoint_id, project_id],
    )
    .unwrap();
    let handoff_id = text_param(&db, "SELECT id FROM handoffs", []);
    let decision_id = text_param(&db, "SELECT id FROM decisions", []);
    let progress_id = text_param(&db, "SELECT id FROM progress_items", []);
    let scope_id = text_param(&db, "SELECT id FROM scopes", []);
    let dependency_id = text_param(&db, "SELECT id FROM task_dependencies", []);

    db.execute(
        "UPDATE tasks SET status = 'completed', updated_at = '2020-01-01T00:00:00+00:00'
         WHERE id = ?1",
        [&task_id],
    )
    .unwrap();

    run(
        &dir,
        &bin,
        &["project", "prune", "--older-than-days", "30", "--json"],
    );

    // Delete-then-parity: every removed row has a tombstone under its id.
    let expected: [(&str, &str); 7] = [
        ("tasks", &task_id),
        ("checkpoints", &checkpoint_id),
        ("checkpoint_corrections", "cc-parity"),
        ("handoffs", &handoff_id),
        ("decisions", &decision_id),
        ("progress_items", &progress_id),
        ("scopes", &scope_id),
    ];
    for (table, row_id) in expected {
        assert_eq!(
            scalar_param(
                &db,
                "SELECT COUNT(*) FROM tombstones WHERE table_name = ?1 AND row_id = ?2",
                rusqlite::params![table, row_id],
            ),
            1,
            "missing tombstone for {table} row {row_id}"
        );
    }
    assert_eq!(
        tombstone_row_ids(&db, "task_dependencies"),
        vec![dependency_id],
        "dependency edges touching a pruned task are tombstoned"
    );
    assert_eq!(tombstone_count(&db, "tasks"), 1, "only PR-0001 is pruned");
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) FROM tasks WHERE display_id = 'PR-0002'"
        ),
        1,
        "the keeper task survives"
    );

    // Children are gone and the database still validates.
    for table in [
        "checkpoints",
        "checkpoint_corrections",
        "handoffs",
        "decisions",
        "progress_items",
        "scopes",
    ] {
        assert_eq!(
            scalar(&db, &format!("SELECT COUNT(*) FROM {table}")),
            0,
            "{table} must be empty after pruning its only task"
        );
    }
    assert_eq!(
        scalar(&db, "SELECT COUNT(*) FROM pragma_foreign_key_check"),
        0
    );

    // Ordinary reads are unchanged by the side table.
    run(&dir, &bin, &["task", "list", "--json"]);
}

#[test]
fn delete_readd_cycles_keep_read_paths_unchanged() {
    let (dir, bin, db) = setup("tombstones_readd", "RA");
    run(&dir, &bin, &["task", "create", "--title", "one"]);
    run(&dir, &bin, &["task", "create", "--title", "two"]);

    // Team member: delete, re-add, delete again.
    run(&dir, &bin, &["team", "create", "--name", "core"]);
    for _ in 0..2 {
        run(
            &dir,
            &bin,
            &["team", "member", "add", "core", "--agent", "tester"],
        );
        run(
            &dir,
            &bin,
            &["team", "member", "remove", "core", "--agent", "tester"],
        );
    }
    assert_eq!(
        tombstone_count(&db, "team_members"),
        1,
        "a re-deleted composite row keeps one tombstone (earliest deletion)"
    );
    run(
        &dir,
        &bin,
        &["team", "member", "add", "core", "--agent", "tester"],
    );
    assert_eq!(
        scalar(&db, "SELECT COUNT(*) FROM team_members"),
        1,
        "a re-added member is live again"
    );
    let status = run(&dir, &bin, &["team", "status", "core", "--json"]);
    let status_json: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status_json["success"], true);

    // Dependency: remove and re-create twice -> two distinct row ids.
    for _ in 0..2 {
        run(
            &dir,
            &bin,
            &["task", "depend", "RA-0002", "--on", "RA-0001"],
        );
        run(
            &dir,
            &bin,
            &["task", "undepend", "RA-0002", "--on", "RA-0001"],
        );
    }
    let dependency_tombstones = tombstone_row_ids(&db, "task_dependencies");
    assert_eq!(dependency_tombstones.len(), 2);
    assert_ne!(
        dependency_tombstones[0], dependency_tombstones[1],
        "re-created dependencies carry fresh ULIDs"
    );

    // Scope: same cycle, and reads still work.
    run(&dir, &bin, &["task", "scope", "add", "RA-0001", "src/**"]);
    run(
        &dir,
        &bin,
        &["task", "scope", "remove", "RA-0001", "src/**"],
    );
    run(&dir, &bin, &["task", "scope", "add", "RA-0001", "src/**"]);
    assert_eq!(tombstone_count(&db, "scopes"), 1);
    let scope_list = run(&dir, &bin, &["task", "scope", "list", "RA-0001", "--json"]);
    let scope_json: serde_json::Value = serde_json::from_slice(&scope_list.stdout).unwrap();
    assert_eq!(
        scope_json["data"].as_array().map(|items| items.len()),
        Some(1),
        "the re-added scope is visible: {scope_json}"
    );
    run(&dir, &bin, &["task", "show", "RA-0001", "--json"]);
}

#[test]
fn doctor_reports_tombstone_counts() {
    let (dir, bin, db) = setup("tombstones_doctor", "DR");
    run(&dir, &bin, &["task", "create", "--title", "one"]);
    run(&dir, &bin, &["task", "create", "--title", "two"]);
    run(
        &dir,
        &bin,
        &["task", "depend", "DR-0002", "--on", "DR-0001"],
    );
    run(
        &dir,
        &bin,
        &["task", "undepend", "DR-0002", "--on", "DR-0001"],
    );

    let doctor = run(&dir, &bin, &["doctor", "--json"]);
    let report: serde_json::Value = serde_json::from_slice(&doctor.stdout).unwrap();
    let check = report["data"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["check"] == "storage.tombstones")
        .expect("doctor must report a storage.tombstones check");
    assert_eq!(check["count"], 1, "doctor reports the tombstone count");
    assert_eq!(
        scalar(&db, "SELECT COUNT(*) FROM tombstones"),
        1,
        "count matches the database"
    );
}
