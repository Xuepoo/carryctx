//! Fault-injection coverage for interrupted project restore and sync pull
//! (CTX-0058).
//!
//! A real SIGKILL is not required: each test constructs the exact on-disk state
//! a crash would leave at a given phase (active database, candidate, original,
//! journal, sidecars) and then invokes the same recovery entry point the CLI
//! runs before opening a writable connection:
//!
//! - `project_mgmt::recover_restore_journals` for `kind == "project.restore"`
//! - `project_mgmt::recover_sync_journals` for `kind == "project.sync.pull"`
//!
//! Path-confining journal rejection is already covered by
//! `project_restore_test.rs::malformed_restore_journal_cannot_cleanup_an_out_of_scope_file`
//! and `sync_test.rs::malformed_sync_journal_cannot_cleanup_an_out_of_scope_file`;
//! it is not duplicated here.

mod common;

use std::path::{Path, PathBuf};

use carryctx_cli::adapter::filesystem::{self, JournalEntry};
use carryctx_cli::adapter::xdg::XdgPaths;
use carryctx_cli::application::project_mgmt;

const GARBAGE: &[u8] = b"this is not a sqlite database";

struct Fixture {
    common_dir: PathBuf,
    state_dir: PathBuf,
    journal_dir: PathBuf,
    db: PathBuf,
    valid_db: Vec<u8>,
    xdg: XdgPaths,
}

impl Fixture {
    /// Initialize a disposable repository with the real binary and capture the
    /// resulting `state.sqlite` bytes as the canonical "valid database"
    /// fixture, then strip any WAL/SHM sidecars so crafted states are exact.
    fn new(name: &str) -> Self {
        let (dir, bin) = common::setup_test_project(name);
        let init = common::run_cmd(&dir, &bin, &["init", "--force"]);
        assert!(init.status.success(), "init failed: {init:?}");

        let common_dir = dir.join(".git");
        let state_dir = common_dir.join("carryctx");
        let journal_dir = state_dir.join("journals");
        let db = state_dir.join("state.sqlite");
        let valid_db = std::fs::read(&db).expect("initialized state.sqlite");
        let _ = std::fs::remove_file(state_dir.join("state.sqlite-wal"));
        let _ = std::fs::remove_file(state_dir.join("state.sqlite-shm"));

        Self {
            common_dir,
            state_dir,
            journal_dir,
            db,
            valid_db,
            xdg: XdgPaths::new(),
        }
    }

    fn write_valid(&self, path: &Path) {
        std::fs::write(path, &self.valid_db).expect("write valid database");
    }

    fn write_garbage(&self, path: &Path) {
        std::fs::write(path, GARBAGE).expect("write garbage database");
    }

    fn write_sidecars(path: &Path) {
        let name = path.file_name().unwrap().to_string_lossy();
        std::fs::write(path.with_file_name(format!("{name}-wal")), b"wal").unwrap();
        std::fs::write(path.with_file_name(format!("{name}-shm")), b"shm").unwrap();
    }

    fn restore_candidate(&self, operation_id: &str) -> PathBuf {
        self.state_dir
            .join(format!("state.sqlite.restore_{operation_id}"))
    }

    fn restore_original(&self, operation_id: &str) -> PathBuf {
        self.state_dir
            .join(format!("state.sqlite.original_{operation_id}"))
    }

    fn sync_candidate(&self, operation_id: &str) -> PathBuf {
        self.state_dir
            .join(format!("state.sqlite.sync_pull_{operation_id}"))
    }

    fn sync_original(&self, operation_id: &str) -> PathBuf {
        self.state_dir
            .join(format!("state.sqlite.sync_original_{operation_id}"))
    }

    fn write_journal(&self, kind: &str, status: &str, metadata: serde_json::Value) -> String {
        let operation_id = ulid::Ulid::generate().to_string();
        filesystem::write_journal(
            &self.journal_dir,
            &JournalEntry {
                operation_id: operation_id.clone(),
                kind: kind.into(),
                status: status.into(),
                created_at: "2026-01-01T00:00:00Z".into(),
                metadata,
            },
        )
        .expect("write journal");
        operation_id
    }

    fn journal_path(&self, operation_id: &str) -> PathBuf {
        self.journal_dir.join(format!("{operation_id}.json"))
    }

    fn remove_active_db(&self) {
        let _ = std::fs::remove_file(&self.db);
        let _ = std::fs::remove_file(self.state_dir.join("state.sqlite-wal"));
        let _ = std::fs::remove_file(self.state_dir.join("state.sqlite-shm"));
    }

    fn recover_restore(&self) -> Result<(), carryctx_cli::error::CarryCtxError> {
        project_mgmt::recover_restore_journals(&self.xdg, &self.common_dir)
    }

    fn recover_sync(&self) -> Result<(), carryctx_cli::error::CarryCtxError> {
        project_mgmt::recover_sync_journals(&self.xdg, &self.common_dir)
    }
}

fn assert_removed(path: &Path) {
    assert!(!path.exists(), "expected {} to be removed", path.display());
}

fn assert_sidecars_removed(path: &Path) {
    let name = path.file_name().unwrap().to_string_lossy();
    assert_removed(&path.with_file_name(format!("{name}-wal")));
    assert_removed(&path.with_file_name(format!("{name}-shm")));
}

// ---------------------------------------------------------------------------
// Restore recovery (`kind == "project.restore"`)
// ---------------------------------------------------------------------------

#[test]
fn restore_crash_before_swap_keeps_active_database_and_cleans_candidate() {
    let fixture = Fixture::new("recovery_restore_before_swap");
    let operation_id = ulid::Ulid::generate().to_string();
    let candidate = fixture.restore_candidate(&operation_id);
    fixture.write_valid(&candidate);
    let journal = fixture.write_journal(
        "project.restore",
        "prepared",
        serde_json::json!({
            "databasePath": fixture.db.clone(),
            "candidatePath": candidate.clone(),
            "originalPath": fixture.restore_original(&operation_id),
        }),
    );

    fixture
        .recover_restore()
        .expect("active database is valid so the candidate is discarded");

    assert_eq!(std::fs::read(&fixture.db).unwrap(), fixture.valid_db);
    assert_removed(&candidate);
    assert_removed(&fixture.journal_path(&journal));
}

#[test]
fn restore_promotes_valid_candidate_when_active_database_missing() {
    let fixture = Fixture::new("recovery_restore_promote_candidate");
    let operation_id = ulid::Ulid::generate().to_string();
    let candidate = fixture.restore_candidate(&operation_id);
    let original = fixture.restore_original(&operation_id);
    fixture.write_valid(&candidate);
    fixture.write_valid(&original);
    let journal = fixture.write_journal(
        "project.restore",
        "prepared",
        serde_json::json!({
            "databasePath": fixture.db.clone(),
            "candidatePath": candidate.clone(),
            "originalPath": original.clone(),
        }),
    );
    fixture.remove_active_db();

    fixture
        .recover_restore()
        .expect("valid candidate must be promoted");

    assert_eq!(std::fs::read(&fixture.db).unwrap(), fixture.valid_db);
    assert_removed(&candidate);
    assert_removed(&original);
    assert_removed(&fixture.journal_path(&journal));
}

#[test]
fn restore_falls_back_to_valid_original_when_candidate_invalid() {
    let fixture = Fixture::new("recovery_restore_original_fallback");
    let operation_id = ulid::Ulid::generate().to_string();
    let candidate = fixture.restore_candidate(&operation_id);
    let original = fixture.restore_original(&operation_id);
    fixture.write_garbage(&candidate);
    fixture.write_valid(&original);
    let journal = fixture.write_journal(
        "project.restore",
        "prepared",
        serde_json::json!({
            "databasePath": fixture.db.clone(),
            "candidatePath": candidate.clone(),
            "originalPath": original.clone(),
        }),
    );
    fixture.remove_active_db();

    fixture
        .recover_restore()
        .expect("invalid candidate must fall back to the original");

    assert_eq!(std::fs::read(&fixture.db).unwrap(), fixture.valid_db);
    assert_removed(&candidate);
    assert_removed(&original);
    assert_removed(&fixture.journal_path(&journal));
}

#[test]
fn restore_unrecoverable_garbage_temps_error_and_preserve_journal() {
    let fixture = Fixture::new("recovery_restore_unrecoverable_garbage");
    let operation_id = ulid::Ulid::generate().to_string();
    let candidate = fixture.restore_candidate(&operation_id);
    let original = fixture.restore_original(&operation_id);
    fixture.write_garbage(&candidate);
    fixture.write_garbage(&original);
    let journal = fixture.write_journal(
        "project.restore",
        "prepared",
        serde_json::json!({
            "databasePath": fixture.db.clone(),
            "candidatePath": candidate.clone(),
            "originalPath": original.clone(),
        }),
    );
    fixture.remove_active_db();

    let error = fixture
        .recover_restore()
        .expect_err("no valid database source exists");
    assert_eq!(error.code, "DATABASE_ERROR");
    assert!(
        fixture.journal_path(&journal).exists(),
        "unrecoverable journal must be preserved for the operator"
    );
    assert!(
        candidate.exists() && original.exists(),
        "invalid temps must not be silently dropped"
    );
}

#[test]
fn restore_unrecoverable_missing_temps_error_and_preserve_journal() {
    let fixture = Fixture::new("recovery_restore_unrecoverable_missing");
    let operation_id = ulid::Ulid::generate().to_string();
    let candidate = fixture.restore_candidate(&operation_id);
    let original = fixture.restore_original(&operation_id);
    let journal = fixture.write_journal(
        "project.restore",
        "prepared",
        serde_json::json!({
            "databasePath": fixture.db.clone(),
            "candidatePath": candidate.clone(),
            "originalPath": original.clone(),
        }),
    );
    fixture.remove_active_db();

    let error = fixture
        .recover_restore()
        .expect_err("missing candidate and original are unrecoverable");
    assert_eq!(error.code, "DATABASE_ERROR");
    assert!(fixture.journal_path(&journal).exists());
    assert_removed(&candidate);
    assert_removed(&original);
}

#[test]
fn restore_completed_journal_is_removed() {
    let fixture = Fixture::new("recovery_restore_completed");
    let operation_id = ulid::Ulid::generate().to_string();
    let candidate = fixture.restore_candidate(&operation_id);
    fixture.write_valid(&candidate);
    let journal = fixture.write_journal(
        "project.restore",
        "completed",
        serde_json::json!({
            "databasePath": fixture.db.clone(),
            "candidatePath": candidate,
            "originalPath": fixture.restore_original(&operation_id),
        }),
    );

    fixture
        .recover_restore()
        .expect("completed journals are swept");

    assert_removed(&fixture.journal_path(&journal));
}

#[test]
fn restore_cleanup_removes_wal_and_shm_sidecars() {
    let fixture = Fixture::new("recovery_restore_sidecars");
    let operation_id = ulid::Ulid::generate().to_string();
    let candidate = fixture.restore_candidate(&operation_id);
    let original = fixture.restore_original(&operation_id);
    fixture.write_valid(&candidate);
    fixture.write_valid(&original);
    Fixture::write_sidecars(&candidate);
    Fixture::write_sidecars(&original);
    let journal = fixture.write_journal(
        "project.restore",
        "prepared",
        serde_json::json!({
            "databasePath": fixture.db.clone(),
            "candidatePath": candidate.clone(),
            "originalPath": original.clone(),
        }),
    );

    fixture
        .recover_restore()
        .expect("active database is valid so temps are swept");

    assert_eq!(std::fs::read(&fixture.db).unwrap(), fixture.valid_db);
    assert_removed(&candidate);
    assert_removed(&original);
    assert_sidecars_removed(&candidate);
    assert_sidecars_removed(&original);
    assert_removed(&fixture.journal_path(&journal));
}

// ---------------------------------------------------------------------------
// Sync recovery (`kind == "project.sync.pull"`)
// ---------------------------------------------------------------------------

#[test]
fn sync_crash_before_swap_keeps_active_database_and_cleans_temps() {
    let fixture = Fixture::new("recovery_sync_before_swap");
    let operation_id = ulid::Ulid::generate().to_string();
    let candidate = fixture.sync_candidate(&operation_id);
    let original = fixture.sync_original(&operation_id);
    fixture.write_valid(&candidate);
    fixture.write_valid(&original);
    let journal = fixture.write_journal(
        "project.sync.pull",
        "prepared",
        serde_json::json!({
            "databasePath": fixture.db.clone(),
            "candidatePath": candidate.clone(),
            "originalPath": original.clone(),
        }),
    );

    fixture
        .recover_sync()
        .expect("active database is valid so staged copy is discarded");

    assert_eq!(std::fs::read(&fixture.db).unwrap(), fixture.valid_db);
    assert_removed(&candidate);
    assert_removed(&original);
    assert_removed(&fixture.journal_path(&journal));
}

#[test]
fn sync_promotes_valid_candidate_when_active_database_missing() {
    let fixture = Fixture::new("recovery_sync_promote_candidate");
    let operation_id = ulid::Ulid::generate().to_string();
    let candidate = fixture.sync_candidate(&operation_id);
    let original = fixture.sync_original(&operation_id);
    fixture.write_valid(&candidate);
    fixture.write_valid(&original);
    let journal = fixture.write_journal(
        "project.sync.pull",
        "prepared",
        serde_json::json!({
            "databasePath": fixture.db.clone(),
            "candidatePath": candidate.clone(),
            "originalPath": original.clone(),
        }),
    );
    fixture.remove_active_db();

    fixture
        .recover_sync()
        .expect("valid candidate must be promoted");

    assert_eq!(std::fs::read(&fixture.db).unwrap(), fixture.valid_db);
    assert_removed(&candidate);
    assert_removed(&original);
    assert_removed(&fixture.journal_path(&journal));
}

#[test]
fn sync_falls_back_to_valid_original_when_candidate_invalid() {
    let fixture = Fixture::new("recovery_sync_original_fallback");
    let operation_id = ulid::Ulid::generate().to_string();
    let candidate = fixture.sync_candidate(&operation_id);
    let original = fixture.sync_original(&operation_id);
    fixture.write_garbage(&candidate);
    fixture.write_valid(&original);
    let journal = fixture.write_journal(
        "project.sync.pull",
        "prepared",
        serde_json::json!({
            "databasePath": fixture.db.clone(),
            "candidatePath": candidate.clone(),
            "originalPath": original.clone(),
        }),
    );
    fixture.remove_active_db();

    fixture
        .recover_sync()
        .expect("invalid candidate must fall back to the original");

    assert_eq!(std::fs::read(&fixture.db).unwrap(), fixture.valid_db);
    assert_removed(&candidate);
    assert_removed(&original);
    assert_removed(&fixture.journal_path(&journal));
}

#[test]
fn sync_unrecoverable_garbage_temps_error_and_preserve_journal() {
    let fixture = Fixture::new("recovery_sync_unrecoverable_garbage");
    let operation_id = ulid::Ulid::generate().to_string();
    let candidate = fixture.sync_candidate(&operation_id);
    let original = fixture.sync_original(&operation_id);
    fixture.write_garbage(&candidate);
    fixture.write_garbage(&original);
    let journal = fixture.write_journal(
        "project.sync.pull",
        "prepared",
        serde_json::json!({
            "databasePath": fixture.db.clone(),
            "candidatePath": candidate.clone(),
            "originalPath": original.clone(),
        }),
    );
    fixture.remove_active_db();

    let error = fixture
        .recover_sync()
        .expect_err("no valid database source exists");
    assert_eq!(error.code, "DATABASE_ERROR");
    assert!(
        fixture.journal_path(&journal).exists(),
        "unrecoverable journal must be preserved for the operator"
    );
    assert!(
        candidate.exists() && original.exists(),
        "invalid temps must not be silently dropped"
    );
}

#[test]
fn sync_unrecoverable_missing_temps_error_and_preserve_journal() {
    let fixture = Fixture::new("recovery_sync_unrecoverable_missing");
    let operation_id = ulid::Ulid::generate().to_string();
    let candidate = fixture.sync_candidate(&operation_id);
    let original = fixture.sync_original(&operation_id);
    let journal = fixture.write_journal(
        "project.sync.pull",
        "prepared",
        serde_json::json!({
            "databasePath": fixture.db.clone(),
            "candidatePath": candidate.clone(),
            "originalPath": original.clone(),
        }),
    );
    fixture.remove_active_db();

    let error = fixture
        .recover_sync()
        .expect_err("missing candidate and original are unrecoverable");
    assert_eq!(error.code, "DATABASE_ERROR");
    assert!(fixture.journal_path(&journal).exists());
    assert_removed(&candidate);
    assert_removed(&original);
}

#[test]
fn sync_completed_journal_removes_temps_and_journal() {
    let fixture = Fixture::new("recovery_sync_completed");
    let operation_id = ulid::Ulid::generate().to_string();
    let candidate = fixture.sync_candidate(&operation_id);
    let original = fixture.sync_original(&operation_id);
    fixture.write_valid(&candidate);
    fixture.write_valid(&original);
    let journal = fixture.write_journal(
        "project.sync.pull",
        "completed",
        serde_json::json!({
            "databasePath": fixture.db.clone(),
            "candidatePath": candidate.clone(),
            "originalPath": original.clone(),
        }),
    );

    fixture
        .recover_sync()
        .expect("completed journals are swept");

    assert_removed(&candidate);
    assert_removed(&original);
    assert_removed(&fixture.journal_path(&journal));
}

#[test]
fn sync_cleanup_removes_wal_and_shm_sidecars() {
    let fixture = Fixture::new("recovery_sync_sidecars");
    let operation_id = ulid::Ulid::generate().to_string();
    let candidate = fixture.sync_candidate(&operation_id);
    let original = fixture.sync_original(&operation_id);
    fixture.write_valid(&candidate);
    fixture.write_valid(&original);
    Fixture::write_sidecars(&candidate);
    Fixture::write_sidecars(&original);
    let journal = fixture.write_journal(
        "project.sync.pull",
        "prepared",
        serde_json::json!({
            "databasePath": fixture.db.clone(),
            "candidatePath": candidate.clone(),
            "originalPath": original.clone(),
        }),
    );

    fixture
        .recover_sync()
        .expect("active database is valid so temps are swept");

    assert_eq!(std::fs::read(&fixture.db).unwrap(), fixture.valid_db);
    assert_removed(&candidate);
    assert_removed(&original);
    assert_sidecars_removed(&candidate);
    assert_sidecars_removed(&original);
    assert_removed(&fixture.journal_path(&journal));
}
