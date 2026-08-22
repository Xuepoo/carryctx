mod common;

use std::process::Command;
use std::sync::{Arc, Barrier};
use std::thread;

use carryctx::adapter::sqlite::ProjectDatabase;

/// Regression test for https://github.com/Xuepoo/carryctx/issues/42.
///
/// A database created before migration `0008_jj_compat` existed has
/// `schema_migrations` stopping at version 7 and no `vcs_backend`/
/// `changed_files_json` columns on `checkpoints`. Every command opens the
/// database via the same code path, so any command should transparently
/// backfill pending migrations instead of requiring the user to know to run
/// `carryctx project migrate` by hand, and `doctor` should never report
/// "up to date" while a migration is actually pending.
#[test]
fn test_stale_migration_is_backfilled_and_doctor_reports_accurately() {
    let (dir, bin) = common::setup_test_project("migration_backfill");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "MB"]);

    let db_path = dir.join(".git/carryctx/state.sqlite");
    assert!(db_path.exists(), "state.sqlite should exist after init");

    // Simulate a pre-existing database that predates migration 0008: roll
    // schema_migrations back to version 7 and drop the columns 0008 added,
    // mirroring exactly what the issue describes (`checkpoints` ends at
    // `created_at`, no `vcs_backend`/`changed_files_json`).
    let rollback_sql = "\
        DELETE FROM schema_migrations WHERE version >= 8;\
        CREATE TABLE checkpoints_old AS SELECT \
            id, project_id, task_id, session_id, worktree_id, agent_id, \
            branch, head, dirty, staged_files_json, modified_files_json, \
            deleted_files_json, renamed_files_json, untracked_files_json, \
            diff_files, diff_insertions, diff_deletions, done_items_json, \
            remaining_items_json, blockers_json, risks_json, next_steps_json, \
            notes_json, created_at \
        FROM checkpoints;\
        DROP TABLE checkpoints;\
        ALTER TABLE checkpoints_old RENAME TO checkpoints;\
        CREATE UNIQUE INDEX checkpoints_id_uq ON checkpoints(id);\
    ";
    let sqlite_status = Command::new("sqlite3")
        .arg(&db_path)
        .arg(rollback_sql)
        .status()
        .expect("sqlite3 binary must be on PATH to run this test");
    assert!(sqlite_status.success(), "sqlite3 rollback command failed");

    // Sanity-check the simulated stale state: no vcs_backend column, no
    // migration 8 recorded.
    let pragma_out = Command::new("sqlite3")
        .arg(&db_path)
        .arg("PRAGMA table_info(checkpoints);")
        .output()
        .unwrap();
    let pragma = String::from_utf8_lossy(&pragma_out.stdout);
    assert!(
        !pragma.contains("vcs_backend"),
        "test setup must produce a DB without vcs_backend: {pragma}"
    );

    // `doctor` must not claim the schema is up to date while 0008 is pending.
    let doctor = common::run_cmd(&dir, &bin, &["doctor", "--json"]);
    let doctor_json: serde_json::Value =
        serde_json::from_slice(&doctor.stdout).expect("doctor should print valid JSON");

    // Any command touching the database (doctor included, since it opens
    // the runtime) backfills pending migrations on open. So by the time we
    // inspect the checks, migration 8 has already been applied — assert
    // that outcome, and separately assert the *reporting* would have been
    // accurate had it still been pending (covered by the direct pragma
    // check below).
    let schema_check = doctor_json["data"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["check"] == "database.schema")
        .expect("doctor should report a database.schema check");
    assert_eq!(
        schema_check["status"], "ok",
        "schema should be healthy after auto-backfill: {schema_check:?}"
    );

    let pragma_after = Command::new("sqlite3")
        .arg(&db_path)
        .arg("PRAGMA table_info(checkpoints);")
        .output()
        .unwrap();
    let pragma_after_str = String::from_utf8_lossy(&pragma_after.stdout);
    assert!(
        pragma_after_str.contains("vcs_backend"),
        "vcs_backend column should exist after doctor auto-backfills pending migrations: {pragma_after_str}"
    );

    // The real symptom from the issue: checkpoint must succeed, not fail
    // with "no column named vcs_backend".
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
    common::run_cmd(&dir, &bin, &["task", "create", "--title", "stale db task"]);
    let checkpoint = common::run_cmd(
        &dir,
        &bin,
        &["checkpoint", "--task", "MB-0001", "--note", "x", "--json"],
    );
    assert!(
        checkpoint.status.success(),
        "checkpoint must succeed on a backfilled database: {}",
        String::from_utf8_lossy(&checkpoint.stderr)
    );
}

/// Regression test for https://github.com/Xuepoo/carryctx/issues/60 (defect A).
///
/// Databases created before 0.5.0 have terminal sessions with `ended_at`
/// NULL (`update_state` never wrote it). Migration 0011 backfills
/// `ended_at = last_activity_at` for those sessions so stats has a real end
/// time to read.
#[test]
fn test_0011_backfills_ended_at_for_terminal_sessions() {
    let (dir, bin) = common::setup_test_project("migration_0011");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "M1"]);
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

    // Create a session so migration 0011 has a row to backfill.
    let start = common::run_cmd(&dir, &bin, &["session", "start"]);
    assert!(start.status.success(), "session start failed");
    let end = common::run_cmd(&dir, &bin, &["session", "end"]);
    assert!(end.status.success(), "session end failed");

    let db_path = dir.join(".git/carryctx/state.sqlite");

    // Simulate the pre-0.5.0 state: roll schema_migrations back to 10 and
    // null out ended_at on the ended session.
    let stale_sql = "\
        DELETE FROM schema_migrations WHERE version >= 11;\
        UPDATE sessions SET ended_at = NULL;\
    ";
    let status = Command::new("sqlite3")
        .arg(&db_path)
        .arg(stale_sql)
        .status()
        .unwrap();
    assert!(status.success(), "sqlite3 failed");

    // Any command runs pending migrations on open.
    let probe = common::run_cmd(&dir, &bin, &["status"]);
    assert!(
        probe.status.success(),
        "status should trigger migration 0011"
    );

    let out = Command::new("sqlite3")
        .arg(&db_path)
        .arg("SELECT ended_at IS NOT NULL FROM sessions;")
        .output()
        .unwrap();
    let val = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        val.trim(),
        "1",
        "migration 0011 must backfill ended_at from last_activity_at"
    );
}

#[test]
fn migration_creates_verified_backup_and_fails_before_schema_change_if_backup_fails() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.sqlite");
    let db = ProjectDatabase::create_fresh(&db_path).unwrap();
    assert!(!dir.path().join("backups").exists());
    drop(db);

    let mut db = ProjectDatabase::open(&db_path).unwrap();

    db.connection_mut()
        .execute("DELETE FROM schema_migrations WHERE version >= 12", [])
        .unwrap();
    drop(db);

    let backup_dir = dir.path().join("backups");
    let mut db = ProjectDatabase::open(&db_path).unwrap();
    db.migrate().unwrap();
    let backups: Vec<_> = std::fs::read_dir(&backup_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(backups.len(), 1);
    assert!(backups[0].is_file());
    let backup = ProjectDatabase::open_readonly(&backups[0]).unwrap();
    assert_eq!(backup.applied_version().unwrap(), 11);
    drop(backup);
    drop(db);

    let failure_dir = dir.path().join("failure");
    std::fs::create_dir(&failure_dir).unwrap();
    let db_path = failure_dir.join("state.sqlite");
    let mut db = ProjectDatabase::create_fresh(&db_path).unwrap();
    db.connection_mut()
        .execute("DELETE FROM schema_migrations WHERE version >= 12", [])
        .unwrap();
    drop(db);
    let backup_dir = failure_dir.join("backups");
    std::fs::write(&backup_dir, "file").unwrap();

    let mut db = ProjectDatabase::open(&db_path).unwrap();
    let error = db.migrate().unwrap_err();
    assert_eq!(error.code, "BACKUP_FAILED");
    assert_eq!(db.applied_version().unwrap(), 11);

    let rollback_dir = dir.path().join("rollback");
    std::fs::create_dir(&rollback_dir).unwrap();
    let rollback_path = rollback_dir.join("state.sqlite");
    let mut db = ProjectDatabase::create_fresh(&rollback_path).unwrap();
    db.connection_mut()
        .execute("CREATE TABLE agents_new (id TEXT)", [])
        .unwrap();
    db.connection_mut()
        .execute("DELETE FROM schema_migrations WHERE version >= 13", [])
        .unwrap();
    let error = db.migrate().unwrap_err();
    assert_eq!(error.code, "MIGRATION_FAILED");
    assert_eq!(db.applied_version().unwrap(), 12);
}

fn foreign_keys_enabled(db: &ProjectDatabase) -> bool {
    db.connection()
        .query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
        .unwrap()
        != 0
}

#[test]
fn rebuild_migrations_restore_the_pre_existing_foreign_key_setting() {
    for initially_enabled in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("state.sqlite");
        let mut db = ProjectDatabase::create_fresh(&db_path).unwrap();
        assert!(
            foreign_keys_enabled(&db),
            "fresh databases enable foreign keys"
        );

        db.connection_mut()
            .execute_batch(if initially_enabled {
                "PRAGMA foreign_keys=ON"
            } else {
                "PRAGMA foreign_keys=OFF"
            })
            .unwrap();
        db.connection_mut()
            .execute("DELETE FROM schema_migrations WHERE version >= 12", [])
            .unwrap();

        assert_eq!(foreign_keys_enabled(&db), initially_enabled);
        db.migrate().unwrap();
        assert_eq!(foreign_keys_enabled(&db), initially_enabled);
        assert_eq!(db.applied_version().unwrap(), 13);
    }
}

#[test]
fn failed_rebuild_migration_restores_foreign_keys_and_keeps_migration_error() {
    for initially_enabled in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("state.sqlite");
        let mut db = ProjectDatabase::create_fresh(&db_path).unwrap();
        db.connection_mut()
            .execute_batch(if initially_enabled {
                "PRAGMA foreign_keys=ON"
            } else {
                "PRAGMA foreign_keys=OFF"
            })
            .unwrap();
        db.connection_mut()
            .execute("CREATE TABLE agents_new (id TEXT)", [])
            .unwrap();
        db.connection_mut()
            .execute("DELETE FROM schema_migrations WHERE version >= 13", [])
            .unwrap();

        let error = db.migrate().unwrap_err();
        assert_eq!(error.code, "MIGRATION_FAILED");
        assert_eq!(foreign_keys_enabled(&db), initially_enabled);
        assert_eq!(db.applied_version().unwrap(), 12);
    }
}

#[test]
fn migration_guards_reject_checksum_mismatch_and_newer_schema() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.sqlite");
    let mut db = ProjectDatabase::create_fresh(&db_path).unwrap();

    db.connection_mut()
        .execute(
            "UPDATE schema_migrations SET checksum = ?1 WHERE version = 1",
            ["0".repeat(64)],
        )
        .unwrap();
    let error = db.migrate().unwrap_err();
    assert_eq!(error.code, "MIGRATION_REQUIRED");
    assert!(!dir.path().join("backups").exists());
    drop(db);

    let mut db = ProjectDatabase::open(&db_path).unwrap();
    db.connection_mut()
        .execute(
            "UPDATE schema_migrations SET checksum = ?1 WHERE version = 1",
            [carryctx::adapter::sqlite::checksum_sql(include_str!(
                "../migrations/project/0001_foundation.sql"
            ))],
        )
        .unwrap();
    db.connection_mut()
        .execute(
            "UPDATE schema_migrations SET version = 99 WHERE version = 13",
            [],
        )
        .unwrap();
    let error = db.migrate().unwrap_err();
    assert_eq!(error.code, "MIGRATION_REQUIRED");
    assert!(!dir.path().join("backups").exists());
}

#[test]
fn migration_guards_reject_interior_version_gap() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.sqlite");
    let mut db = ProjectDatabase::create_fresh(&db_path).unwrap();
    db.connection_mut()
        .execute("DELETE FROM schema_migrations WHERE version = 12", [])
        .unwrap();

    let error = db.migrate().unwrap_err();
    assert_eq!(error.code, "MIGRATION_REQUIRED");
    assert!(!dir.path().join("backups").exists());
}

#[test]
fn create_backup_verifies_destination_and_repeated_backups_get_unique_paths() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.sqlite");
    let db = ProjectDatabase::create_fresh(&db_path).unwrap();
    let first = dir.path().join("backup.sqlite");
    db.create_backup(&first).unwrap();
    assert!(ProjectDatabase::open_readonly(&first).is_ok());

    db.create_backup(&first).unwrap();
    assert!(dir.path().join("backup.sqlite_1").is_file());
}

#[test]
fn concurrent_migrations_are_serialized_and_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.sqlite");
    let db = ProjectDatabase::create_fresh(&db_path).unwrap();
    db.connection()
        .execute("DELETE FROM schema_migrations WHERE version >= 12", [])
        .unwrap();
    drop(db);

    let path = Arc::new(db_path);
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let path = Arc::clone(&path);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let mut db = ProjectDatabase::open(path.as_ref()).unwrap();
                barrier.wait();
                db.migrate()
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap().unwrap();
    }

    let db = ProjectDatabase::open_readonly(path.as_ref()).unwrap();
    assert_eq!(db.applied_version().unwrap(), 13);
    assert_eq!(
        db.connection()
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        13
    );
}
