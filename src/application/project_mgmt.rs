use std::path::Path;

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
    let backup_path = backup_dir.join(format!("state_{timestamp}.sqlite"));

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

pub fn restore_project(
    backup_path: &Path,
    project_path: &Path,
    _uow: &UnitOfWork,
) -> Result<(), CarryCtxError> {
    if !backup_path.exists() {
        return Err(CarryCtxError::resource_not_found(format!(
            "Backup file '{}' not found.",
            backup_path.display()
        )));
    }

    let xdg = XdgPaths::new();
    let git = GitCli::new();
    let gp = git.discover(project_path)?;
    let db_path = xdg.project_db(&gp.git_common_dir);

    let pre_restore_backup_dir = xdg.backup_dir(&gp.git_common_dir);
    filesystem::ensure_dir(&pre_restore_backup_dir)?;
    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let pre_backup_path = pre_restore_backup_dir.join(format!("pre_restore_{timestamp}.sqlite"));

    if db_path.exists() {
        let current_db = ProjectDatabase::open_readonly(&db_path)?;
        current_db.create_backup(&pre_backup_path)?;
    }

    std::fs::copy(backup_path, &db_path)
        .map_err(|e| CarryCtxError::database_error(format!("Failed to restore backup: {e}")))?;

    let restored_db = ProjectDatabase::open_readonly(&db_path)?;
    let integrity: String = restored_db
        .connection()
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|e| CarryCtxError::database_error(format!("Integrity check failed: {e}")))?;

    if integrity != "ok" {
        if pre_backup_path.exists() {
            let _ = std::fs::copy(&pre_backup_path, &db_path);
        }
        return Err(CarryCtxError::database_error(format!(
            "Restored database integrity check failed: {integrity}"
        )));
    }

    let event_repo = SqliteEventRepository::new(restored_db.connection());
    let _ = event_repo.append(&NewEvent {
        id: new_id(),
        project_id: "".into(),
        event_type: "project.restored".into(),
        actor_agent_id: None,
        session_id: None,
        task_id: None,
        payload: serde_json::json!({
            "backupPath": backup_path.to_string_lossy(),
            "preRestoreBackupPath": pre_backup_path.to_string_lossy(),
        }),
        occurred_at: now(),
    });

    Ok(())
}

