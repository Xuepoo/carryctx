mod common;

use std::process::Command;

use carryctx::adapter::filesystem::JournalEntry;
use carryctx::adapter::sqlite::ProjectDatabase;
use carryctx::adapter::xdg::XdgPaths;
use carryctx::application::project_mgmt;

fn json(output: &std::process::Output) -> serde_json::Value {
    let stream = if output.stdout.is_empty() {
        &output.stderr
    } else {
        &output.stdout
    };
    serde_json::from_slice(stream).unwrap_or_else(|error| {
        panic!(
            "expected JSON stdout, got {error}: {}",
            String::from_utf8_lossy(stream)
        )
    })
}

fn init(dir: &std::path::Path, bin: &std::path::Path) {
    let output = common::run_cmd(dir, bin, &["init", "--force"]);
    assert!(output.status.success(), "init failed: {:?}", output);
}

#[test]
fn restore_replaces_state_and_creates_a_verified_pre_restore_backup() {
    let (dir, bin) = common::setup_test_project("project_restore_valid");
    init(&dir, &bin);

    let backup = common::run_cmd(&dir, &bin, &["project", "backup", "--json"]);
    assert!(backup.status.success(), "backup failed: {:?}", backup);
    let backup_path = json(&backup)["data"].as_str().unwrap().to_owned();

    let changed = common::run_cmd(
        &dir,
        &bin,
        &[
            "agent",
            "register",
            "--name",
            "after-backup",
            "--provider",
            "test",
        ],
    );
    assert!(
        changed.status.success(),
        "state mutation failed: {:?}",
        changed
    );

    let restored = common::run_cmd(&dir, &bin, &["project", "restore", &backup_path, "--json"]);
    assert!(restored.status.success(), "restore failed: {:?}", restored);

    let agents = common::run_cmd(&dir, &bin, &["agent", "list", "--json"]);
    assert!(agents.status.success());
    assert!(!String::from_utf8_lossy(&agents.stdout).contains("after-backup"));

    let backup_dir = dir.join(".git/carryctx/backups");
    let pre_restore_count = std::fs::read_dir(backup_dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("pre_restore_")
        })
        .count();
    assert_eq!(pre_restore_count, 1);
}

#[test]
fn corrupt_backup_returns_error_and_preserves_original_database() {
    let (dir, bin) = common::setup_test_project("project_restore_corrupt");
    init(&dir, &bin);
    let db_path = dir.join(".git/carryctx/state.sqlite");
    let original = std::fs::read(&db_path).unwrap();
    let backup_path = dir.join("corrupt.sqlite");
    std::fs::write(&backup_path, b"not a sqlite database").unwrap();

    let restored = common::run_cmd(
        &dir,
        &bin,
        &[
            "project",
            "restore",
            backup_path.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(!restored.status.success());
    assert_eq!(json(&restored)["error"]["code"], "DATABASE_ERROR");
    assert_eq!(std::fs::read(&db_path).unwrap(), original);
}

#[test]
fn valid_sqlite_backup_that_fails_candidate_validation_preserves_original() {
    let (dir, bin) = common::setup_test_project("project_restore_candidate_failure");
    init(&dir, &bin);
    let db_path = dir.join(".git/carryctx/state.sqlite");
    let original = std::fs::read(&db_path).unwrap();

    let invalid_schema = dir.join("invalid_schema.sqlite");
    let connection = rusqlite::Connection::open(&invalid_schema).unwrap();
    connection
        .execute_batch("CREATE TABLE valid_but_not_carryctx (id INTEGER PRIMARY KEY);")
        .unwrap();
    drop(connection);
    assert!(ProjectDatabase::open_readonly(&invalid_schema).is_ok());

    let restored = common::run_cmd(
        &dir,
        &bin,
        &[
            "project",
            "restore",
            invalid_schema.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(!restored.status.success());
    assert_eq!(json(&restored)["success"], false);
    assert_eq!(std::fs::read(&db_path).unwrap(), original);

    let reopen = ProjectDatabase::open_readonly(&db_path).unwrap();
    assert_eq!(
        reopen
            .connection()
            .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
}

#[test]
fn concurrent_backups_receive_distinct_destinations() {
    let (dir, bin) = common::setup_test_project("project_restore_backup_names");
    init(&dir, &bin);
    let first = Command::new(&bin)
        .args(["project", "backup", "--json"])
        .current_dir(&dir)
        .output()
        .unwrap();
    let second = Command::new(&bin)
        .args(["project", "backup", "--json"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(first.status.success());
    assert!(second.status.success());
    assert_ne!(json(&first)["data"], json(&second)["data"]);
}

#[test]
fn restore_is_rejected_while_a_writable_connection_holds_admission_lock() {
    let (dir, bin) = common::setup_test_project("project_restore_lock_contention");
    init(&dir, &bin);
    let backup = common::run_cmd(&dir, &bin, &["project", "backup", "--json"]);
    let backup_path = json(&backup)["data"].as_str().unwrap().to_owned();
    let db_path = dir.join(".git/carryctx/state.sqlite");
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    let lock_path = dir.join(".git/carryctx/locks/command.lock");
    let metadata = lock_path.join("meta.json");
    std::fs::create_dir_all(&lock_path).unwrap();
    std::fs::write(
        metadata,
        format!(
            r#"{{"operation_id":"held","pid":{},"hostname":"test","acquired_at":"now"}}"#,
            std::process::id()
        ),
    )
    .unwrap();

    let restored = common::run_cmd(&dir, &bin, &["project", "restore", &backup_path, "--json"]);
    assert_eq!(restored.status.code(), Some(3));
    assert!(restored.stdout.is_empty());
    assert_eq!(json(&restored)["error"]["code"], "STATE_CONFLICT");
    assert!(!dir.join(".git/carryctx/state.sqlite.restore").exists());
    drop(connection);
    std::fs::remove_dir_all(lock_path).unwrap();
}

#[test]
fn malformed_restore_journal_cannot_cleanup_an_out_of_scope_file() {
    let root = tempfile::tempdir().unwrap();
    let common_dir = root.path().join("repo/.git");
    let state_dir = common_dir.join("carryctx");
    let journal_dir = state_dir.join("journals");
    std::fs::create_dir_all(&journal_dir).unwrap();
    let protected = root.path().join("protected.sqlite");
    std::fs::write(&protected, b"must remain").unwrap();

    let operation_id = ulid::Ulid::generate().to_string();
    std::fs::write(
        journal_dir.join(format!("{operation_id}.json")),
        serde_json::to_vec(&JournalEntry {
            operation_id: protected.to_string_lossy().into_owned(),
            kind: "project.restore".into(),
            status: "completed".into(),
            created_at: "now".into(),
            metadata: serde_json::json!({
                "databasePath": state_dir.join("state.sqlite"),
                "candidatePath": state_dir.join("state.sqlite.candidate"),
                "originalPath": state_dir.join("state.sqlite.original"),
            }),
        })
        .unwrap(),
    )
    .unwrap();

    let xdg = XdgPaths::new();
    let error = project_mgmt::recover_restore_journals(&xdg, &common_dir).unwrap_err();
    assert_eq!(error.code, "DATABASE_ERROR");
    assert_eq!(std::fs::read(&protected).unwrap(), b"must remain");
}
