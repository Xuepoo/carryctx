mod common;

use std::path::Path;

use carryctx::adapter::filesystem::JournalEntry;
use carryctx::adapter::xdg::XdgPaths;
use carryctx::application::project_mgmt;

fn json(output: &std::process::Output) -> serde_json::Value {
    let bytes = if output.stdout.is_empty() {
        &output.stderr
    } else {
        &output.stdout
    };
    serde_json::from_slice(bytes).unwrap_or_else(|error| {
        panic!(
            "expected JSON output ({error}): {}",
            String::from_utf8_lossy(bytes)
        )
    })
}

fn init(dir: &Path, bin: &Path) {
    let output = common::run_cmd(dir, bin, &["init", "--force"]);
    assert!(output.status.success(), "init failed: {:?}", output);
}

#[test]
fn sync_push_and_pull_round_trip_with_snapshot_and_backup() {
    let (dir, bin) = common::setup_test_project("sync_round_trip");
    init(&dir, &bin);
    let remote = dir.join("remote");

    let pushed = common::run_cmd(
        &dir,
        &bin,
        &[
            "sync",
            "push",
            "--remote",
            remote.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(pushed.status.success(), "push failed: {:?}", pushed);
    assert_eq!(json(&pushed)["data"]["status"], "pushed");
    let remote_db = remote.join(".git.sqlite");
    std::fs::write(remote.join(".git.sqlite-wal"), b"stale").unwrap();
    std::fs::write(remote.join(".git.sqlite-shm"), b"stale").unwrap();

    let registered = common::run_cmd(
        &dir,
        &bin,
        &[
            "agent",
            "register",
            "--name",
            "local-only",
            "--provider",
            "test",
        ],
    );
    assert!(registered.status.success());

    let pulled = common::run_cmd(
        &dir,
        &bin,
        &[
            "sync",
            "pull",
            "--remote",
            remote.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(pulled.status.success(), "pull failed: {:?}", pulled);
    assert!(!dir.join(".git/carryctx/state.sqlite-wal").exists());
    assert!(!dir.join(".git/carryctx/state.sqlite-shm").exists());
    assert!(remote_db.exists());

    let agents = common::run_cmd(&dir, &bin, &["agent", "list", "--json"]);
    assert!(agents.status.success());
    assert!(!String::from_utf8_lossy(&agents.stdout).contains("local-only"));
    assert!(
        dir.join(".git/carryctx/backups")
            .read_dir()
            .unwrap()
            .any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("pre_sync_pull_")
            })
    );
}

#[test]
fn sync_pull_rejects_missing_malformed_and_incompatible_remote_without_data_loss() {
    let (dir, bin) = common::setup_test_project("sync_invalid_remote");
    init(&dir, &bin);
    let db_path = dir.join(".git/carryctx/state.sqlite");
    let original = std::fs::read(&db_path).unwrap();
    let remote = dir.join("remote");

    let missing = common::run_cmd(
        &dir,
        &bin,
        &[
            "sync",
            "pull",
            "--remote",
            remote.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(!missing.status.success());
    assert_eq!(json(&missing)["error"]["code"], "RESOURCE_NOT_FOUND");
    assert_eq!(std::fs::read(&db_path).unwrap(), original);

    std::fs::create_dir_all(&remote).unwrap();
    let remote_db = remote.join(".git.sqlite");
    std::fs::write(&remote_db, b"not sqlite").unwrap();
    let malformed = common::run_cmd(
        &dir,
        &bin,
        &[
            "sync",
            "pull",
            "--remote",
            remote.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(!malformed.status.success());
    assert_eq!(std::fs::read(&db_path).unwrap(), original);

    let incompatible = dir.join("incompatible.sqlite");
    rusqlite::Connection::open(&incompatible)
        .unwrap()
        .execute_batch("CREATE TABLE schema_migrations (version INTEGER, name TEXT, checksum TEXT, applied_at TEXT); INSERT INTO schema_migrations VALUES (999, 'future', 'x', 'now');")
        .unwrap();
    std::fs::copy(&incompatible, &remote_db).unwrap();
    let rejected = common::run_cmd(
        &dir,
        &bin,
        &[
            "sync",
            "pull",
            "--remote",
            remote.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(!rejected.status.success());
    assert_eq!(std::fs::read(&db_path).unwrap(), original);
}

#[test]
fn sync_is_rejected_while_admission_lock_is_held_and_repeats_cleanly() {
    let (dir, bin) = common::setup_test_project("sync_lock_and_repeat");
    init(&dir, &bin);
    let remote = dir.join("remote");
    let pushed = common::run_cmd(
        &dir,
        &bin,
        &[
            "sync",
            "push",
            "--remote",
            remote.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(pushed.status.success());

    let lock_path = dir.join(".git/carryctx/locks/command.lock");
    std::fs::create_dir_all(&lock_path).unwrap();
    std::fs::write(
        lock_path.join("meta.json"),
        format!(
            r#"{{"operation_id":"held","pid":{},"hostname":"test","acquired_at":"now"}}"#,
            std::process::id()
        ),
    )
    .unwrap();
    let blocked = common::run_cmd(
        &dir,
        &bin,
        &[
            "sync",
            "pull",
            "--remote",
            remote.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(blocked.status.code(), Some(3));
    assert!(blocked.stdout.is_empty());
    assert_eq!(json(&blocked)["error"]["code"], "STATE_CONFLICT");
    std::fs::remove_dir_all(lock_path).unwrap();

    for _ in 0..3 {
        let result = common::run_cmd(
            &dir,
            &bin,
            &[
                "sync",
                "pull",
                "--remote",
                remote.to_str().unwrap(),
                "--json",
            ],
        );
        assert!(result.status.success(), "repeat pull failed: {:?}", result);
    }

    let dir_a = dir.clone();
    let bin_a = bin.clone();
    let remote_a = remote.clone();
    let first = std::thread::spawn(move || {
        common::run_cmd(
            &dir_a,
            &bin_a,
            &[
                "sync",
                "pull",
                "--remote",
                remote_a.to_str().unwrap(),
                "--json",
            ],
        )
    });
    let dir_b = dir.clone();
    let bin_b = bin.clone();
    let remote_b = remote.clone();
    let second = std::thread::spawn(move || {
        common::run_cmd(
            &dir_b,
            &bin_b,
            &[
                "sync",
                "pull",
                "--remote",
                remote_b.to_str().unwrap(),
                "--json",
            ],
        )
    });
    let results = [first.join().unwrap(), second.join().unwrap()];
    assert!(results.iter().any(|output| output.status.success()));
    assert!(results.iter().any(|output| {
        output.status.code() == Some(3) && json(output)["error"]["code"] == "STATE_CONFLICT"
    }));
}

#[test]
fn sync_pull_rejects_a_valid_database_from_another_project() {
    let (dir, bin) = common::setup_test_project("sync_project_mismatch");
    init(&dir, &bin);
    let (other_dir, other_bin) = common::setup_test_project("sync_project_mismatch_other");
    init(&other_dir, &other_bin);
    let remote = dir.join("remote");

    let pushed = common::run_cmd(
        &other_dir,
        &other_bin,
        &[
            "sync",
            "push",
            "--remote",
            remote.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(pushed.status.success(), "push failed: {:?}", pushed);
    let db_path = dir.join(".git/carryctx/state.sqlite");
    let original = std::fs::read(&db_path).unwrap();

    let pulled = common::run_cmd(
        &dir,
        &bin,
        &[
            "sync",
            "pull",
            "--remote",
            remote.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(!pulled.status.success());
    assert_eq!(json(&pulled)["error"]["code"], "SYNC_PROJECT_MISMATCH");
    assert_eq!(std::fs::read(&db_path).unwrap(), original);
}

#[test]
fn sync_pull_rejects_current_history_with_missing_schema_or_foreign_keys() {
    let (dir, bin) = common::setup_test_project("sync_invalid_structure");
    init(&dir, &bin);
    let db_path = dir.join(".git/carryctx/state.sqlite");
    let original = std::fs::read(&db_path).unwrap();
    let remote = dir.join("remote");
    std::fs::create_dir_all(&remote).unwrap();
    let remote_db = remote.join(".git.sqlite");

    std::fs::copy(&db_path, &remote_db).unwrap();
    rusqlite::Connection::open(&remote_db)
        .unwrap()
        .execute_batch("DROP TABLE teams;")
        .unwrap();
    let missing_table = common::run_cmd(
        &dir,
        &bin,
        &[
            "sync",
            "pull",
            "--remote",
            remote.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(!missing_table.status.success());
    assert_eq!(std::fs::read(&db_path).unwrap(), original);

    std::fs::copy(&db_path, &remote_db).unwrap();
    rusqlite::Connection::open(&remote_db)
        .unwrap()
        .execute_batch(
            "PRAGMA foreign_keys=OFF;
             INSERT INTO events (id, project_id, type, aggregate_type, aggregate_id, payload_json, occurred_at)
             VALUES ('orphan', 'missing-project', 'test', 'test', 'orphan', '{}', 'now');",
        )
        .unwrap();
    let foreign_key_failure = common::run_cmd(
        &dir,
        &bin,
        &[
            "sync",
            "pull",
            "--remote",
            remote.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(!foreign_key_failure.status.success());
    assert_eq!(std::fs::read(&db_path).unwrap(), original);
}

#[test]
fn malformed_sync_journal_cannot_cleanup_an_out_of_scope_file() {
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
            kind: "project.sync.pull".into(),
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
    let error = project_mgmt::recover_sync_journals(&xdg, &common_dir).unwrap_err();
    assert_eq!(error.code, "DATABASE_ERROR");
    assert_eq!(std::fs::read(&protected).unwrap(), b"must remain");
}
