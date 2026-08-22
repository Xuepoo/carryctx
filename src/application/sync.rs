use crate::adapter::filesystem;
use crate::adapter::filesystem::AdmissionLock;
use crate::adapter::git::GitCli;
use crate::adapter::sqlite::ProjectDatabase;
use crate::adapter::xdg::XdgPaths;
use crate::application::project_mgmt;
use crate::error::{CarryCtxError, ExitCode};
use std::fs;
use std::path::{Path, PathBuf};

pub fn sync_push(
    project_path: &Path,
    remote_path: &str,
) -> Result<serde_json::Value, CarryCtxError> {
    let git = GitCli::new();
    let gp = git.discover(project_path)?;
    let xdg = XdgPaths::new();
    let _admission_lock = AdmissionLock::acquire(
        &xdg.admission_lock_dir(&gp.git_common_dir),
        &ulid::Ulid::generate().to_string(),
        std::process::id(),
        &std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".into()),
        &chrono::Utc::now().to_rfc3339(),
    )?;
    let db_path = xdg.project_db(&gp.git_common_dir);

    if !db_path.exists() {
        return Err(CarryCtxError::resource_not_found(
            "Project database not found to push.",
        ));
    }

    let remote = Path::new(remote_path);
    filesystem::ensure_dir(remote)?;

    let target_db = remote.join(format!(
        "{}.sqlite",
        gp.git_common_dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
    ));
    checkpoint_database(&db_path)?;
    let snapshot_path = sibling_path(&target_db, &format!("push_{}", ulid::Ulid::generate()));
    let snapshot_result = (|| {
        let database = ProjectDatabase::open_readonly(&db_path)?;
        database.create_backup(&snapshot_path)?;
        project_mgmt::validate_database_for_sync(&snapshot_path)?;
        fs::rename(&snapshot_path, &target_db).map_err(|e| {
            CarryCtxError::new(
                "SYNC_ERROR",
                format!("Failed to publish database snapshot to remote: {e}"),
                ExitCode::General,
            )
        })?;
        remove_sidecars(&target_db);
        Ok::<(), CarryCtxError>(())
    })();
    if let Err(error) = snapshot_result {
        remove_database_files(&snapshot_path);
        return Err(error);
    }

    Ok(serde_json::json!({
        "status": "pushed",
        "remote": target_db.to_string_lossy(),
        "bytes": fs::metadata(&target_db).map(|m| m.len()).unwrap_or(0),
    }))
}

pub fn sync_pull(
    project_path: &Path,
    remote_path: &str,
) -> Result<serde_json::Value, CarryCtxError> {
    let git = GitCli::new();
    let gp = git.discover(project_path)?;
    let xdg = XdgPaths::new();
    let _admission_lock = AdmissionLock::acquire(
        &xdg.admission_lock_dir(&gp.git_common_dir),
        &ulid::Ulid::generate().to_string(),
        std::process::id(),
        &std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".into()),
        &chrono::Utc::now().to_rfc3339(),
    )?;
    let db_path = xdg.project_db(&gp.git_common_dir);

    let remote = Path::new(remote_path);
    let target_db = remote.join(format!(
        "{}.sqlite",
        gp.git_common_dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
    ));

    if !target_db.exists() {
        return Err(CarryCtxError::resource_not_found(format!(
            "Remote database not found at {}",
            target_db.display()
        )));
    }

    project_mgmt::validate_database_for_sync(&target_db)?;
    let local_project_id = ProjectDatabase::open_readonly(&db_path)?
        .connection()
        .query_row("SELECT id FROM projects", [], |row| row.get::<_, String>(0))
        .map_err(|e| {
            CarryCtxError::database_error(format!("Local project identity lookup failed: {e}"))
        })?;
    let remote_project_id = ProjectDatabase::open_readonly(&target_db)?
        .connection()
        .query_row("SELECT id FROM projects", [], |row| row.get::<_, String>(0))
        .map_err(|e| {
            CarryCtxError::database_error(format!("Remote project identity lookup failed: {e}"))
        })?;
    if local_project_id != remote_project_id {
        return Err(CarryCtxError::new(
            "SYNC_PROJECT_MISMATCH",
            "Remote database belongs to a different CarryCtx project.",
            ExitCode::Validation,
        ));
    }
    filesystem::ensure_dir(db_path.parent().unwrap())?;

    let operation_id = ulid::Ulid::generate().to_string();
    let candidate_path = sibling_path(&db_path, &format!("sync_pull_{operation_id}"));
    let original_path = sibling_path(&db_path, &format!("sync_original_{operation_id}"));
    let journal_dir = xdg.journal_dir(&gp.git_common_dir);
    let pre_backup_path = xdg.backup_dir(&gp.git_common_dir).join(format!(
        "pre_sync_pull_{}_{}.sqlite",
        chrono::Utc::now().format("%Y%m%d_%H%M%S"),
        operation_id
    ));

    if db_path.exists() {
        filesystem::ensure_dir(pre_backup_path.parent().unwrap())?;
        let current = ProjectDatabase::open_readonly(&db_path)?;
        current.create_backup(&pre_backup_path)?;
        project_mgmt::validate_database_for_sync(&pre_backup_path)?;
        drop(current);
        checkpoint_database(&db_path)?;
        remove_sidecars(&db_path);
    }

    filesystem::write_journal(
        &journal_dir,
        &filesystem::JournalEntry {
            operation_id: operation_id.clone(),
            kind: "project.sync.pull".into(),
            status: "prepared".into(),
            created_at: chrono::Utc::now().to_rfc3339(),
            metadata: serde_json::json!({
                "databasePath": db_path.to_string_lossy(),
                "candidatePath": candidate_path.to_string_lossy(),
                "originalPath": original_path.to_string_lossy(),
                "remotePath": target_db.to_string_lossy(),
                "prePullBackupPath": pre_backup_path.to_string_lossy(),
            }),
        },
    )?;

    let replacement_result = (|| {
        fs::copy(&target_db, &candidate_path).map_err(|e| {
            CarryCtxError::new(
                "SYNC_ERROR",
                format!("Failed to stage remote database: {e}"),
                ExitCode::General,
            )
        })?;
        project_mgmt::validate_database_for_sync(&candidate_path)?;

        if db_path.exists() {
            fs::hard_link(&db_path, &original_path).map_err(|e| {
                CarryCtxError::database_error(format!("Failed to preserve local database: {e}"))
            })?;
        }
        fs::rename(&candidate_path, &db_path).map_err(|e| {
            CarryCtxError::database_error(format!(
                "Failed to atomically replace local database: {e}"
            ))
        })?;
        Ok::<(), CarryCtxError>(())
    })();

    if let Err(error) = replacement_result {
        remove_database_files(&candidate_path);
        remove_database_files(&original_path);
        let _ = filesystem::remove_journal(&journal_dir, &operation_id);
        return Err(error);
    }

    filesystem::write_journal(
        &journal_dir,
        &filesystem::JournalEntry {
            operation_id: operation_id.clone(),
            kind: "project.sync.pull".into(),
            status: "completed".into(),
            created_at: chrono::Utc::now().to_rfc3339(),
            metadata: serde_json::json!({
                "databasePath": db_path.to_string_lossy(),
                "candidatePath": candidate_path.to_string_lossy(),
                "originalPath": original_path.to_string_lossy(),
                "prePullBackupPath": pre_backup_path.to_string_lossy(),
            }),
        },
    )?;
    remove_database_files(&original_path);
    filesystem::remove_journal(&journal_dir, &operation_id)?;

    Ok(serde_json::json!({
        "status": "pulled",
        "local": db_path.to_string_lossy(),
        "bytes": fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0),
    }))
}

fn checkpoint_database(path: &Path) -> Result<(), CarryCtxError> {
    let database = ProjectDatabase::open(path)?;
    database
        .connection()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .map_err(|e| CarryCtxError::database_error(format!("Database checkpoint failed: {e}")))
}

fn sibling_path(path: &Path, suffix: &str) -> PathBuf {
    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!("{file_name}.{suffix}"))
}

fn remove_database_files(path: &Path) {
    let _ = fs::remove_file(path);
    remove_sidecars(path);
}

fn remove_sidecars(path: &Path) {
    let _ = fs::remove_file(path.with_file_name(format!(
        "{}-wal",
        path.file_name().unwrap_or_default().to_string_lossy()
    )));
    let _ = fs::remove_file(path.with_file_name(format!(
        "{}-shm",
        path.file_name().unwrap_or_default().to_string_lossy()
    )));
}
