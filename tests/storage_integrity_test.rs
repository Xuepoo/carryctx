//! Regression tests for CTX-0068 / issues #99 and #100 (storage integrity).
//!
//! Covers the data-loss risks around prune/archive/init: pruning must
//! archive and delete every child record under enabled foreign keys without
//! leaving orphans, `init --force` must never cascade-wipe project state,
//! and the backup audit event must actually persist with a real project id.

mod common;

use std::process::Command;

fn sqlite(db_path: &std::path::Path, sql: &str) -> String {
    let out = Command::new("sqlite3")
        .arg(db_path)
        .arg(sql)
        .output()
        .expect("sqlite3 binary must be on PATH to run this test");
    assert!(
        out.status.success(),
        "sqlite3 failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn scalar(db_path: &std::path::Path, sql: &str) -> i64 {
    sqlite(db_path, sql).parse().expect("integer result")
}

fn setup_initialized(name: &str, prefix: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let (dir, bin) = common::setup_test_project(name);
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", prefix]);
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
    (dir, bin)
}

/// Regression test for issue #100 (prune/archive FK integrity).
///
/// Pruning a completed task used to swallow unlink/copy errors, disable
/// foreign keys to get past the NO ACTION keys on handoffs and
/// checkpoint_corrections, and hard-delete parent rows. With migration 0014
/// in place the prune runs with foreign keys ON, archives every child
/// record first, deletes children before parents, and leaves zero foreign
/// key violations.
#[test]
fn test_prune_archives_and_deletes_children_without_fk_violations() {
    let (dir, bin) = setup_initialized("prune_fk_integrity", "PF");
    let db_path = dir.join(".git/carryctx/state.sqlite");
    let archive_path = dir.join(".git/carryctx/archive.sqlite");

    common::run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", "Task destined for the archive"],
    );
    let created = common::run_cmd(
        &dir,
        &bin,
        &[
            "checkpoint",
            "--task",
            "PF-0001",
            "--done",
            "archived work recorded here",
        ],
    );
    assert!(created.status.success(), "checkpoint failed");
    let handoff = common::run_cmd(
        &dir,
        &bin,
        &[
            "handoff",
            "create",
            "--target",
            "tester",
            "--task",
            "PF-0001",
            "--summary",
            "handoff tied to pruned task",
        ],
    );
    assert!(handoff.status.success(), "handoff create failed");
    let decision = common::run_cmd(
        &dir,
        &bin,
        &[
            "decision",
            "add",
            "--title",
            "Archive decision",
            "--task",
            "PF-0001",
        ],
    );
    assert!(decision.status.success(), "decision add failed");

    // Complete the task and age it beyond the prune threshold.
    sqlite(
        &db_path,
        "UPDATE tasks SET status='completed', updated_at='2020-01-01T00:00:00+00:00'
         WHERE display_id='PF-0001';",
    );

    let prune = common::run_cmd(
        &dir,
        &bin,
        &["project", "prune", "--older-than-days", "30", "--json"],
    );
    assert!(
        prune.status.success(),
        "prune should succeed with foreign keys enabled: {} / stdout: {}",
        String::from_utf8_lossy(&prune.stderr),
        String::from_utf8_lossy(&prune.stdout),
    );

    let violations = scalar(&db_path, "SELECT COUNT(*) FROM pragma_foreign_key_check;");
    assert_eq!(violations, 0, "prune must leave no FK violations");

    // Audit rows survive the unlink; their dangling task references are
    // nulled and the append-only guard is back in force.
    assert!(
        scalar(&db_path, "SELECT COUNT(*) FROM events;") > 0,
        "audit history must be preserved by the prune"
    );
    assert_eq!(
        scalar(
            &db_path,
            "SELECT COUNT(*) FROM events WHERE task_id IS NOT NULL AND task_id IN (SELECT id FROM tasks WHERE display_id='PF-0001');"
        ),
        0
    );
    let guard_update = Command::new("sqlite3")
        .arg(&db_path)
        .arg("UPDATE events SET type='tampered';")
        .output()
        .expect("sqlite3 binary must be on PATH");
    assert!(
        !guard_update.status.success(),
        "the events append-only guard must be restored after pruning"
    );

    for table in [
        "tasks",
        "checkpoints",
        "checkpoint_corrections",
        "handoffs",
        "decisions",
        "progress_items",
        "scopes",
        "task_dependencies",
    ] {
        assert_eq!(
            scalar(&db_path, &format!("SELECT COUNT(*) FROM {table};")),
            0,
            "{table} must be empty after pruning its only task"
        );
    }

    // The archive database received the records before deletion.
    assert!(archive_path.exists(), "archive database must exist");
    assert_eq!(scalar(&archive_path, "SELECT COUNT(*) FROM tasks;"), 1);
    assert_eq!(scalar(&archive_path, "SELECT COUNT(*) FROM handoffs;"), 1);
    assert_eq!(
        scalar(&archive_path, "SELECT COUNT(*) FROM checkpoints;"),
        1
    );
    assert_eq!(scalar(&archive_path, "SELECT COUNT(*) FROM decisions;"), 1);

    // The surviving project still validates end-to-end.
    let show = common::run_cmd(&dir, &bin, &["project", "show", "--json"]);
    assert!(show.status.success(), "project show after prune failed");
}

/// Regression test for issue #100 (`init --force` cascade wipe).
///
/// `init --force` used INSERT OR REPLACE on projects, which deleted the row
/// and cascaded away all tasks/agents/progress/teams. It must upsert
/// instead, keep the same project id even when `.carryctx/config.toml` was
/// removed, and leave existing tasks intact.
#[test]
fn test_init_force_preserves_tasks_and_project_identity() {
    let (dir, bin) = setup_initialized("init_force_preserves_rows", "IF");
    let db_path = dir.join(".git/carryctx/state.sqlite");

    common::run_cmd(&dir, &bin, &["task", "create", "--title", "Surviving task"]);
    let first_id = sqlite(&db_path, "SELECT id FROM projects LIMIT 1;");

    // Forced re-init with config present keeps identity and rows.
    let reinit_config = common::run_cmd(&dir, &bin, &["init", "--force", "--json"]);
    assert!(
        reinit_config.status.success(),
        "re-init with config failed: {}",
        String::from_utf8_lossy(&reinit_config.stderr)
    );

    // Remove the config so init would otherwise mint a fresh project id;
    // it must reuse the one stored in the state database instead.
    std::fs::remove_file(dir.join(".carryctx/config.toml")).unwrap();
    let reinit_noconfig = common::run_cmd(&dir, &bin, &["init", "--force", "--json"]);
    assert!(
        reinit_noconfig.status.success(),
        "re-init without config failed: {}",
        String::from_utf8_lossy(&reinit_noconfig.stderr)
    );

    assert_eq!(
        sqlite(&db_path, "SELECT id FROM projects LIMIT 1;"),
        first_id,
        "project identity must be stable across forced re-inits"
    );
    assert_eq!(
        scalar(&db_path, "SELECT COUNT(*) FROM tasks;"),
        1,
        "tasks must survive init --force"
    );
    assert_eq!(
        scalar(&db_path, "SELECT COUNT(*) FROM agents;"),
        1,
        "agents must survive init --force"
    );

    let list = common::run_cmd(&dir, &bin, &["task", "list", "--json"]);
    let value: serde_json::Value =
        serde_json::from_slice(&list.stdout).expect("valid JSON from task list");
    let titles: Vec<&str> = value["data"]
        .as_array()
        .expect("data array")
        .iter()
        .filter_map(|t| t["title"].as_str())
        .collect();
    assert!(
        titles.contains(&"Surviving task"),
        "the pre-existing task must still be listed: {value}"
    );
}

/// Regression test for issue #99 (backup audit event).
///
/// The backup audit event used to be attempted with project_id="" on a
/// read-only connection and its failure swallowed, so no audit event ever
/// landed. It now persists through the command's unit-of-work connection
/// with the real project id.
#[test]
fn test_backup_appends_audit_event_with_real_project_id() {
    let (dir, bin) = setup_initialized("backup_audit_event", "BA");
    let db_path = dir.join(".git/carryctx/state.sqlite");

    let backup = common::run_cmd(&dir, &bin, &["project", "backup", "--json"]);
    assert!(
        backup.status.success(),
        "project backup failed: {}",
        String::from_utf8_lossy(&backup.stderr)
    );

    let events = scalar(
        &db_path,
        "SELECT COUNT(*) FROM events WHERE type='project.backup_created'
         AND project_id=(SELECT id FROM projects LIMIT 1);",
    );
    assert!(
        events >= 1,
        "a backup audit event with the real project id must exist"
    );

    // The backup itself is readable as a valid database.
    let backup_file = sqlite(
        &db_path,
        "SELECT json_extract(payload_json,'$.backupPath') FROM events
         WHERE type='project.backup_created' ORDER BY occurred_at DESC LIMIT 1;",
    );
    assert!(
        !backup_file.is_empty(),
        "audit payload must reference the backup file"
    );
    assert!(std::path::Path::new(&backup_file).exists());
}
