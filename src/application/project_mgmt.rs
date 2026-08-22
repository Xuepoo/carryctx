use std::fs;
use std::path::{Path, PathBuf};

use crate::adapter::filesystem;
use crate::adapter::git::GitCli;
use crate::adapter::sqlite::ProjectDatabase;
use crate::adapter::sqlite_repos::SqliteEventRepository;
use crate::adapter::unit_of_work::UnitOfWork;
use crate::adapter::xdg::XdgPaths;
use crate::error::CarryCtxError;
use crate::repository::event::{EventRepository, NewEvent};

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn hostname() -> String {
    std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".into())
}

fn new_id() -> String {
    ulid::Ulid::generate().to_string()
}

pub fn backup_project(project_path: &Path, _uow: &UnitOfWork) -> Result<String, CarryCtxError> {
    let xdg = XdgPaths::new();
    let git = GitCli::new();
    let gp = git.discover(project_path)?;
    let db_path = xdg.project_db(&gp.git_common_dir);
    let backup_dir = xdg.backup_dir(&gp.git_common_dir);

    filesystem::ensure_dir(&backup_dir)?;

    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let backup_path = backup_dir.join(format!(
        "state_{timestamp}_{}.sqlite",
        ulid::Ulid::generate()
    ));

    let db = ProjectDatabase::open_readonly(&db_path)?;
    db.create_backup(&backup_path)?;

    let event_repo = SqliteEventRepository::new(db.connection());
    let _ = event_repo.append(&NewEvent {
        id: new_id(),
        project_id: "".into(),
        event_type: "project.backup_created".into(),
        actor_agent_id: None,
        session_id: None,
        task_id: None,
        payload: serde_json::json!({
            "backupPath": backup_path.to_string_lossy(),
        }),
        occurred_at: now(),
    });

    Ok(backup_path.to_string_lossy().to_string())
}

pub fn prune_project(
    older_than_days: u32,
    archive_db_path: Option<&Path>,
    uow: &UnitOfWork,
) -> Result<serde_json::Value, CarryCtxError> {
    let now = chrono::Utc::now();
    let threshold = now - chrono::Duration::days(older_than_days as i64);
    let threshold_str = threshold.to_rfc3339();

    let conn = uow.connection();

    // 1. Find all completed tasks updated before the threshold
    let mut stmt = conn
        .prepare("SELECT id FROM tasks WHERE status = 'completed' AND updated_at < ?1")
        .map_err(|e| CarryCtxError::database_error(format!("Failed to prepare statement: {e}")))?;

    let task_ids: Vec<String> = stmt
        .query_map([&threshold_str], |row| row.get(0))
        .map_err(|e| CarryCtxError::database_error(format!("Failed to query tasks: {e}")))?
        .filter_map(Result::ok)
        .collect();

    let pruned_count = task_ids.len();
    let mut archived_path_str = String::new();

    if pruned_count > 0 {
        let placeholders: Vec<String> = task_ids.iter().map(|_| "?".to_string()).collect();
        let in_clause = placeholders.join(", ");

        // 2. Clear parent_task_id references to pruned tasks
        let update_parent_sql =
            format!("UPDATE tasks SET parent_task_id = NULL WHERE parent_task_id IN ({in_clause})");
        let _ = conn.execute(&update_parent_sql, rusqlite::params_from_iter(&task_ids));

        // 3. Unlink task_id references in optional tables
        let unlink_tables = ["events", "sessions", "worktrees"];
        for table in unlink_tables.iter() {
            let sql = format!("UPDATE {table} SET task_id = NULL WHERE task_id IN ({in_clause})");
            let _ = conn.execute(&sql, rusqlite::params_from_iter(&task_ids));
        }

        // 4. If archive DB is provided, attach and copy records before deletion
        if let Some(archive_path) = archive_db_path {
            if let Some(parent) = archive_path.parent() {
                filesystem::ensure_dir(parent)?;
            }
            if !archive_path.exists() {
                let _ = ProjectDatabase::create_fresh(archive_path)?;
            }

            let path_clean = archive_path.to_string_lossy().replace('\'', "''");
            let attach_sql = format!("ATTACH DATABASE '{path_clean}' AS archive");
            conn.execute(&attach_sql, []).map_err(|e| {
                CarryCtxError::database_error(format!("Failed to attach archive database: {e}"))
            })?;

            // Copy projects row
            let _ = conn.execute(
                "INSERT OR IGNORE INTO archive.projects SELECT * FROM main.projects",
                [],
            );

            // Copy tasks
            let archive_tasks_sql = format!(
                "INSERT OR IGNORE INTO archive.tasks SELECT * FROM main.tasks WHERE id IN ({in_clause})"
            );
            let _ = conn.execute(&archive_tasks_sql, rusqlite::params_from_iter(&task_ids));

            // Copy dependencies
            let archive_deps_sql = format!(
                "INSERT OR IGNORE INTO archive.task_dependencies SELECT * FROM main.task_dependencies WHERE task_id IN ({in_clause}) OR prerequisite_task_id IN ({in_clause})"
            );
            let _ = conn.execute(
                &archive_deps_sql,
                rusqlite::params_from_iter(task_ids.iter().chain(task_ids.iter())),
            );

            // Copy child tables
            let child_tables = ["checkpoints", "progress_items", "scopes", "decisions"];
            for table in child_tables.iter() {
                let sql = format!(
                    "INSERT OR IGNORE INTO archive.{table} SELECT * FROM main.{table} WHERE task_id IN ({in_clause})"
                );
                let _ = conn.execute(&sql, rusqlite::params_from_iter(&task_ids));
            }

            let _ = conn.execute("DETACH DATABASE archive", []);
            archived_path_str = archive_path.to_string_lossy().to_string();
        }

        // 5. Delete task dependencies in main DB
        let del_deps_sql = format!(
            "DELETE FROM task_dependencies WHERE task_id IN ({in_clause}) OR prerequisite_task_id IN ({in_clause})"
        );
        let _ = conn.execute(
            &del_deps_sql,
            rusqlite::params_from_iter(task_ids.iter().chain(task_ids.iter())),
        );

        // 6. Delete child tables in main DB
        let child_tables = ["checkpoints", "progress_items", "scopes", "decisions"];
        for table in child_tables.iter() {
            let sql = format!("DELETE FROM {table} WHERE task_id IN ({in_clause})");
            conn.execute(&sql, rusqlite::params_from_iter(&task_ids))
                .map_err(|e| {
                    CarryCtxError::database_error(format!("Failed to prune {table}: {e}"))
                })?;
        }

        // 7. Delete tasks in main DB
        let sql_tasks = format!("DELETE FROM tasks WHERE id IN ({in_clause})");
        conn.execute(&sql_tasks, rusqlite::params_from_iter(&task_ids))
            .map_err(|e| CarryCtxError::database_error(format!("Failed to prune tasks: {e}")))?;
    }

    Ok(serde_json::json!({
        "status": "success",
        "prunedTasksCount": pruned_count,
        "olderThanDays": older_than_days,
        "archivePath": if archived_path_str.is_empty() { serde_json::Value::Null } else { serde_json::json!(archived_path_str) },
    }))
}

pub fn restore_project(backup_path: &Path, project_path: &Path) -> Result<(), CarryCtxError> {
    if !backup_path.is_file() {
        return Err(CarryCtxError::resource_not_found(format!(
            "Backup file '{}' not found.",
            backup_path.display()
        )));
    }

    let xdg = XdgPaths::new();
    let git = GitCli::new();
    let gp = git.discover(project_path)?;
    let db_path = xdg.project_db(&gp.git_common_dir);
    let _admission_lock = filesystem::AdmissionLock::acquire(
        &xdg.admission_lock_dir(&gp.git_common_dir),
        &ulid::Ulid::generate().to_string(),
        std::process::id(),
        &hostname(),
        &now(),
    )?;
    let operation_id = ulid::Ulid::generate().to_string();
    restore_project_locked(
        backup_path,
        &db_path,
        &xdg,
        &gp.git_common_dir,
        &operation_id,
    )
}

/// Recover an interrupted restore before any writable project connection opens.
pub fn recover_restore_journals(
    xdg: &XdgPaths,
    git_common_dir: &Path,
) -> Result<(), CarryCtxError> {
    let journal_dir = xdg.journal_dir(git_common_dir);
    let state_dir = xdg.project_state_dir(git_common_dir);
    for entry in filesystem::list_journals(&journal_dir)? {
        if entry.kind != "project.restore" {
            continue;
        }
        let database_path = trusted_journal_path(&state_dir, &entry, "databasePath")?;
        let original_path = trusted_journal_path_optional(&state_dir, &entry, "originalPath")?;
        let candidate_path = trusted_journal_path_optional(&state_dir, &entry, "candidatePath")?;
        if entry.status == "completed" {
            filesystem::remove_journal(&journal_dir, &entry.operation_id)?;
            continue;
        }
        if !database_path.exists() {
            if candidate_path
                .as_ref()
                .is_some_and(|path| path.exists() && validate_database(path).is_ok())
            {
                if let Some(candidate_path) = &candidate_path {
                    fs::rename(candidate_path, &database_path).map_err(|e| {
                        CarryCtxError::database_error(format!(
                            "Failed to recover restore candidate: {e}"
                        ))
                    })?;
                }
            } else if original_path
                .as_ref()
                .is_some_and(|path| path.exists() && validate_database(path).is_ok())
            {
                if let Some(original_path) = &original_path {
                    fs::rename(original_path, &database_path).map_err(|e| {
                        CarryCtxError::database_error(format!(
                            "Failed to recover original database: {e}"
                        ))
                    })?;
                }
            } else {
                return Err(CarryCtxError::database_error(
                    "Interrupted restore has no valid candidate or original database.",
                ));
            }
        }
        if let Some(candidate_path) = candidate_path {
            remove_database_files(&candidate_path);
        }
        if let Some(original_path) = original_path {
            remove_database_files(&original_path);
        }
        filesystem::remove_journal(&journal_dir, &entry.operation_id)?;
    }
    Ok(())
}

/// Recover interrupted state replacements before opening a writable database.
pub fn recover_sync_journals(xdg: &XdgPaths, git_common_dir: &Path) -> Result<(), CarryCtxError> {
    let journal_dir = xdg.journal_dir(git_common_dir);
    let state_dir = xdg.project_state_dir(git_common_dir);
    for entry in filesystem::list_journals(&journal_dir)? {
        if entry.kind != "project.sync.pull" {
            continue;
        }
        let database_path = trusted_journal_path(&state_dir, &entry, "databasePath")?;
        let candidate_path = trusted_journal_path(&state_dir, &entry, "candidatePath")?;
        let original_path = trusted_journal_path(&state_dir, &entry, "originalPath")?;
        let active_valid = database_path.exists() && validate_database(&database_path).is_ok();
        let candidate_valid = candidate_path.exists() && validate_database(&candidate_path).is_ok();
        let original_valid = original_path.exists() && validate_database(&original_path).is_ok();

        if entry.status == "completed" {
            remove_database_files(&candidate_path);
            remove_database_files(&original_path);
            filesystem::remove_journal(&journal_dir, &entry.operation_id)?;
            continue;
        }

        if !active_valid {
            let replacement = if candidate_valid {
                Some(candidate_path.as_path())
            } else if original_valid {
                Some(original_path.as_path())
            } else {
                None
            };
            let Some(replacement) = replacement else {
                return Err(CarryCtxError::database_error(
                    "Interrupted sync has no valid active, candidate, or original database.",
                ));
            };
            remove_database_files(&database_path);
            fs::rename(replacement, &database_path).map_err(|e| {
                CarryCtxError::database_error(format!("Failed to recover sync database: {e}"))
            })?;
        }

        remove_database_files(&candidate_path);
        remove_database_files(&original_path);
        filesystem::remove_journal(&journal_dir, &entry.operation_id)?;
    }
    Ok(())
}

fn trusted_journal_path(
    state_dir: &Path,
    entry: &filesystem::JournalEntry,
    key: &str,
) -> Result<PathBuf, CarryCtxError> {
    let value = entry.metadata[key]
        .as_str()
        .ok_or_else(|| CarryCtxError::database_error(format!("Journal is missing {key}.")))?;
    let path = PathBuf::from(value);
    let state_dir = state_dir.canonicalize().map_err(|e| {
        CarryCtxError::database_error(format!("Cannot resolve CarryCtx state directory: {e}"))
    })?;
    let parent = path.parent().ok_or_else(|| {
        CarryCtxError::database_error("Journal path has no trusted parent directory.")
    })?;
    let canonical_parent = parent.canonicalize().map_err(|e| {
        CarryCtxError::database_error(format!("Cannot resolve journal path parent: {e}"))
    })?;
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| CarryCtxError::database_error("Journal path has no valid filename."))?;
    let database_name = "state.sqlite";
    let trusted_name = filename == database_name
        || (filename.starts_with("state.sqlite.")
            && (filename.contains("sync_pull_")
                || filename.contains("sync_original_")
                || filename.contains("restore_")
                || filename.contains("original_")));
    if canonical_parent != state_dir || !trusted_name {
        return Err(CarryCtxError::database_error(
            "Journal path is outside the trusted CarryCtx state directory.",
        ));
    }
    Ok(state_dir.join(filename))
}

fn trusted_journal_path_optional(
    state_dir: &Path,
    entry: &filesystem::JournalEntry,
    key: &str,
) -> Result<Option<PathBuf>, CarryCtxError> {
    match entry.metadata[key].as_str() {
        Some(_) => trusted_journal_path(state_dir, entry, key).map(Some),
        None => Ok(None),
    }
}

fn restore_project_locked(
    backup_path: &Path,
    db_path: &Path,
    xdg: &XdgPaths,
    git_common_dir: &Path,
    operation_id: &str,
) -> Result<(), CarryCtxError> {
    // Validate external input before touching the active database.
    validate_database(backup_path)?;

    let pre_restore_backup_dir = xdg.backup_dir(git_common_dir);
    filesystem::ensure_dir(&pre_restore_backup_dir)?;
    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let pre_backup_path = pre_restore_backup_dir.join(format!(
        "pre_restore_{timestamp}_{}.sqlite",
        ulid::Ulid::generate()
    ));

    if db_path.exists() {
        let current_db = ProjectDatabase::open_readonly(db_path)?;
        current_db.create_backup(&pre_backup_path)?;
        validate_database(&pre_backup_path)?;
        drop(current_db);
        checkpoint_database(db_path)?;
    }

    let candidate_path = sibling_path(db_path, &format!("restore_{operation_id}"));
    let original_path = sibling_path(db_path, &format!("original_{operation_id}"));
    let journal_dir = xdg.journal_dir(git_common_dir);
    filesystem::write_journal(
        &journal_dir,
        &filesystem::JournalEntry {
            operation_id: operation_id.to_string(),
            kind: "project.restore".into(),
            status: "prepared".into(),
            created_at: now(),
            metadata: serde_json::json!({
                "backupPath": backup_path.to_string_lossy(),
                "databasePath": db_path.to_string_lossy(),
                "candidatePath": candidate_path.to_string_lossy(),
                "originalPath": original_path.to_string_lossy(),
            }),
        },
    )?;
    copy_candidate(backup_path, &candidate_path)?;

    let candidate_result = (|| {
        let candidate = ProjectDatabase::open(&candidate_path)?;
        let project_id: String = candidate
            .connection()
            .query_row("SELECT id FROM projects LIMIT 1", [], |row| row.get(0))
            .map_err(|e| {
                CarryCtxError::database_error(format!("Candidate validation failed: {e}"))
            })?;
        let event_repo = SqliteEventRepository::new(candidate.connection());
        event_repo.append(&NewEvent {
            id: new_id(),
            project_id,
            event_type: "project.restored".into(),
            actor_agent_id: None,
            session_id: None,
            task_id: None,
            payload: serde_json::json!({
                "backupPath": backup_path.to_string_lossy(),
                "preRestoreBackupPath": pre_backup_path.to_string_lossy(),
            }),
            occurred_at: now(),
        })?;
        candidate
            .connection()
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .map_err(|e| {
                CarryCtxError::database_error(format!("Candidate checkpoint failed: {e}"))
            })?;
        drop(candidate);
        validate_database(&candidate_path)
    })();
    if let Err(error) = candidate_result {
        remove_database_files(&candidate_path);
        return Err(error);
    }

    if db_path.exists() {
        fs::hard_link(db_path, &original_path).map_err(|e| {
            remove_database_files(&candidate_path);
            CarryCtxError::database_error(format!("Failed to preserve active database: {e}"))
        })?;
    }
    if let Err(error) = fs::rename(&candidate_path, db_path) {
        remove_database_files(&candidate_path);
        let _ = fs::remove_file(&original_path);
        return Err(CarryCtxError::database_error(format!(
            "Failed to atomically swap restored database: {error}"
        )));
    }

    let _ = fs::remove_file(&original_path);

    filesystem::write_journal(
        &journal_dir,
        &filesystem::JournalEntry {
            operation_id: operation_id.to_string(),
            kind: "project.restore".into(),
            status: "completed".into(),
            created_at: now(),
            metadata: serde_json::json!({
                "backupPath": backup_path.to_string_lossy(),
                "databasePath": db_path.to_string_lossy(),
                "preRestoreBackupPath": pre_backup_path.to_string_lossy(),
            }),
        },
    )?;
    filesystem::remove_journal(&journal_dir, operation_id)?;

    Ok(())
}

fn validate_database(path: &Path) -> Result<(), CarryCtxError> {
    let database = ProjectDatabase::open_readonly(path)?;
    let required_schema = [
        (
            "schema_migrations",
            &["version", "name", "checksum", "applied_at"] as &[&str],
        ),
        (
            "projects",
            &[
                "id",
                "name",
                "task_prefix",
                "repository_root",
                "git_common_dir",
                "main_branch",
                "schema_version",
                "created_at",
                "updated_at",
            ],
        ),
        (
            "operations",
            &[
                "id",
                "kind",
                "state",
                "payload_json",
                "failure_code",
                "created_at",
                "updated_at",
            ],
        ),
        (
            "events",
            &[
                "id",
                "project_id",
                "type",
                "aggregate_type",
                "aggregate_id",
                "payload_json",
                "occurred_at",
            ],
        ),
        ("sequences", &["project_id", "kind", "next_value"]),
        (
            "agents",
            &[
                "id",
                "project_id",
                "name",
                "provider",
                "role",
                "status",
                "created_at",
                "updated_at",
            ],
        ),
        (
            "tasks",
            &[
                "id",
                "project_id",
                "display_id",
                "title",
                "description",
                "status",
                "priority",
                "parent_task_id",
                "required_role",
                "team_id",
                "created_at",
                "updated_at",
            ],
        ),
        (
            "task_dependencies",
            &[
                "id",
                "project_id",
                "task_id",
                "prerequisite_task_id",
                "kind",
                "created_at",
            ],
        ),
        (
            "progress_items",
            &[
                "id",
                "project_id",
                "display_id",
                "task_id",
                "type",
                "status",
                "content",
                "created_at",
                "updated_at",
            ],
        ),
        (
            "worktrees",
            &[
                "id",
                "project_id",
                "task_id",
                "normalized_path",
                "git_common_dir",
                "branch",
                "bound_at",
                "updated_at",
            ],
        ),
        (
            "sessions",
            &[
                "id",
                "project_id",
                "agent_id",
                "task_id",
                "state",
                "provider",
                "working_directory",
                "started_at",
                "last_activity_at",
                "updated_at",
            ],
        ),
        (
            "checkpoints",
            &["id", "project_id", "task_id", "created_at"],
        ),
        (
            "checkpoint_corrections",
            &["id", "checkpoint_id", "project_id", "corrected_at"],
        ),
        (
            "scopes",
            &[
                "id",
                "project_id",
                "task_id",
                "pattern",
                "kind",
                "created_at",
            ],
        ),
        (
            "decisions",
            &[
                "id",
                "project_id",
                "task_id",
                "display_id",
                "title",
                "rationale",
                "created_at",
                "updated_at",
            ],
        ),
        (
            "handoffs",
            &[
                "id",
                "project_id",
                "task_id",
                "from_agent_id",
                "to_agent_id",
                "state",
                "display_id",
                "summary",
                "created_at",
                "updated_at",
            ],
        ),
        (
            "graph_nodes",
            &[
                "id",
                "node_type",
                "name",
                "metadata",
                "created_at",
                "updated_at",
            ],
        ),
        (
            "graph_edges",
            &[
                "source_id",
                "target_id",
                "relation_type",
                "created_at",
                "metadata",
            ],
        ),
        (
            "teams",
            &["id", "project_id", "name", "created_at", "updated_at"],
        ),
        (
            "team_members",
            &[
                "project_id",
                "team_id",
                "agent_id",
                "created_at",
                "updated_at",
            ],
        ),
    ];
    for (table, columns) in required_schema {
        let mut statement = database
            .connection()
            .prepare(&format!("PRAGMA table_info({table})"))
            .map_err(|e| CarryCtxError::database_error(format!("Schema validation failed: {e}")))?;
        let found: std::collections::HashSet<String> = statement
            .query_map([], |row| row.get(1))
            .map_err(|e| CarryCtxError::database_error(format!("Schema validation failed: {e}")))?
            .collect::<Result<_, _>>()
            .map_err(|e| CarryCtxError::database_error(format!("Schema validation failed: {e}")))?;
        if columns.iter().any(|column| !found.contains(*column)) {
            return Err(CarryCtxError::database_error(format!(
                "Database is missing required schema in table {table}."
            )));
        }
    }
    let project_count: i64 = database
        .connection()
        .query_row("SELECT COUNT(*) FROM projects", [], |row| row.get(0))
        .map_err(|e| {
            CarryCtxError::database_error(format!("Project row validation failed: {e}"))
        })?;
    if project_count != 1 {
        return Err(CarryCtxError::database_error(
            "Database must contain exactly one project row.",
        ));
    }
    let valid_project: bool = database.connection().query_row(
        "SELECT length(trim(id)) > 0 AND length(trim(name)) > 0 AND length(trim(task_prefix)) > 0 AND length(trim(repository_root)) > 0 AND length(trim(git_common_dir)) > 0 AND length(trim(main_branch)) > 0 AND schema_version > 0 AND length(trim(created_at)) > 0 AND length(trim(updated_at)) > 0 FROM projects",
        [], |row| row.get(0)).map_err(|e| CarryCtxError::database_error(format!("Project row validation failed: {e}")))?;
    if !valid_project {
        return Err(CarryCtxError::database_error(
            "Database project row is malformed.",
        ));
    }
    database.validate_schema_compatibility()?;
    let integrity: String = database
        .connection()
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|e| CarryCtxError::database_error(format!("Integrity check failed: {e}")))?;
    if integrity != "ok" {
        return Err(CarryCtxError::new(
            "BACKUP_INTEGRITY_FAILED",
            format!("Database integrity check failed: {integrity}"),
            crate::error::ExitCode::Database,
        ));
    }
    let foreign_key_violations: i64 = database
        .connection()
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .map_err(|e| CarryCtxError::database_error(format!("Foreign key check failed: {e}")))?;
    if foreign_key_violations > 0 {
        return Err(CarryCtxError::new(
            "BACKUP_INTEGRITY_FAILED",
            format!("Foreign key check found {foreign_key_violations} violation(s)."),
            crate::error::ExitCode::Database,
        ));
    }
    Ok(())
}

pub(crate) fn validate_database_for_sync(path: &Path) -> Result<(), CarryCtxError> {
    validate_database(path)
}

fn checkpoint_database(path: &Path) -> Result<(), CarryCtxError> {
    let database = ProjectDatabase::open(path)?;
    database
        .connection()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .map_err(|e| CarryCtxError::database_error(format!("Database checkpoint failed: {e}")))
}

fn remove_database_files(path: &Path) {
    let _ = fs::remove_file(path);
    let _ = fs::remove_file(path.with_file_name(format!(
        "{}-wal",
        path.file_name().unwrap_or_default().to_string_lossy()
    )));
    let _ = fs::remove_file(path.with_file_name(format!(
        "{}-shm",
        path.file_name().unwrap_or_default().to_string_lossy()
    )));
}

fn copy_candidate(backup_path: &Path, candidate_path: &Path) -> Result<(), CarryCtxError> {
    if let Err(error) = fs::copy(backup_path, candidate_path) {
        remove_database_files(candidate_path);
        return Err(CarryCtxError::database_error(format!(
            "Failed to restore backup: {error}"
        )));
    }
    Ok(())
}

fn sibling_path(path: &Path, suffix: &str) -> PathBuf {
    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!("{file_name}.{suffix}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_candidate_copy_removes_partial_candidate_files() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("missing.sqlite");
        let candidate = root.path().join("candidate.sqlite");
        fs::write(&candidate, b"partial").unwrap();
        let error = copy_candidate(&source, &candidate).unwrap_err();
        assert_eq!(error.code, "DATABASE_ERROR");
        assert!(!candidate.exists());
    }
}
