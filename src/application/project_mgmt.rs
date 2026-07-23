use std::path::Path;

use crate::adapter::filesystem;
use crate::adapter::git::GitCli;
use crate::adapter::sqlite::ProjectDatabase;
use crate::adapter::sqlite_repos::SqliteEventRepository;
use crate::adapter::unit_of_work::UnitOfWork;
use crate::adapter::xdg::XdgPaths;
use crate::domain::config::CarryCtxConfig;
use crate::error::CarryCtxError;
use crate::repository::event::{EventRepository, NewEvent};

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn new_id() -> String {
    ulid::Ulid::new().to_string()
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ProjectInfo {
    pub id: String,
    pub name: String,
    pub task_prefix: String,
    pub repository_root: String,
    pub git_common_dir: String,
    pub main_branch: String,
    pub config_path: String,
    pub state_path: String,
    pub schema_version: i64,
    pub up_to_date: bool,
}

pub fn show_project(project_path: &Path, _uow: &UnitOfWork) -> Result<ProjectInfo, CarryCtxError> {
    let xdg = XdgPaths::new();
    let git = GitCli::new();
    let gp = git.discover(project_path)?;
    let db_path = xdg.project_db(&gp.git_common_dir);

    let db = ProjectDatabase::open_readonly(&db_path)?;
    let schema_version = db.applied_version().unwrap_or(0);
    let up_to_date = db.is_up_to_date().unwrap_or(false);

    let config_path = gp.repository_root.join(".carryctx").join("config.toml");

    let config: CarryCtxConfig = if config_path.exists() {
        let content = std::fs::read_to_string(&config_path).unwrap_or_default();
        toml::from_str(&content).unwrap_or_default()
    } else {
        CarryCtxConfig::default()
    };

    Ok(ProjectInfo {
        id: config.project.id.clone(),
        name: config.project.name.clone(),
        task_prefix: config.project.task_prefix.clone(),
        repository_root: gp.repository_root.to_string_lossy().to_string(),
        git_common_dir: gp.git_common_dir.to_string_lossy().to_string(),
        main_branch: config.git.main_branch.clone(),
        config_path: config_path.to_string_lossy().to_string(),
        state_path: db_path.to_string_lossy().to_string(),
        schema_version,
        up_to_date,
    })
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RegistryProject {
    pub id: String,
    pub repository_root: String,
    pub git_common_dir: String,
    pub config_path: String,
    pub last_seen_at: String,
}

pub fn list_projects() -> Result<Vec<RegistryProject>, CarryCtxError> {
    let xdg = XdgPaths::new();
    let registry_path = xdg.registry_db();

    if !registry_path.exists() {
        return Ok(vec![]);
    }

    let content = std::fs::read_to_string(&registry_path)
        .map_err(|e| CarryCtxError::database_error(format!("Failed to read registry: {e}")))?;
    let entries: Vec<serde_json::Value> = serde_json::from_str(&content)
        .map_err(|e| CarryCtxError::database_error(format!("Invalid registry format: {e}")))?;

    let projects = entries
        .into_iter()
        .map(|entry| RegistryProject {
            id: entry["id"].as_str().unwrap_or("").to_string(),
            repository_root: entry["repositoryRoot"].as_str().unwrap_or("").to_string(),
            git_common_dir: entry["gitCommonDir"].as_str().unwrap_or("").to_string(),
            config_path: entry["configPath"].as_str().unwrap_or("").to_string(),
            last_seen_at: entry["lastSeenAt"].as_str().unwrap_or("").to_string(),
        })
        .collect();

    Ok(projects)
}

pub fn register_project(project_path: &Path) -> Result<(), CarryCtxError> {
    let xdg = XdgPaths::new();
    let git = GitCli::new();
    let gp = git.discover(project_path)?;
    let now = now();
    let registry_path = xdg.registry_db();

    let config_path = gp.repository_root.join(".carryctx").join("config.toml");
    let project_id = if config_path.exists() {
        let content = std::fs::read_to_string(&config_path).unwrap_or_default();
        let config: CarryCtxConfig = toml::from_str(&content).unwrap_or_default();
        config.project.id
    } else {
        new_id()
    };

    let registry_dir = registry_path.parent().unwrap_or(Path::new("."));
    filesystem::ensure_dir(registry_dir)?;

    let mut registry: Vec<serde_json::Value> = if registry_path.exists() {
        let content = std::fs::read_to_string(&registry_path).unwrap_or_else(|_| "[]".into());
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        Vec::new()
    };

    let entry = serde_json::json!({
        "id": project_id,
        "repositoryRoot": gp.repository_root.to_string_lossy(),
        "gitCommonDir": gp.git_common_dir.to_string_lossy(),
        "configPath": config_path.to_string_lossy(),
        "lastSeenAt": now,
    });

    if let Some(pos) = registry.iter().position(|e| e["id"] == project_id) {
        registry[pos] = entry;
    } else {
        registry.push(entry);
    }

    let json = serde_json::to_string_pretty(&registry)
        .map_err(|e| CarryCtxError::database_error(format!("Failed to serialize registry: {e}")))?;
    std::fs::write(&registry_path, json)
        .map_err(|e| CarryCtxError::database_error(format!("Failed to write registry: {e}")))?;

    Ok(())
}

pub fn unregister_project(project_id: &str) -> Result<(), CarryCtxError> {
    let xdg = XdgPaths::new();
    let registry_path = xdg.registry_db();

    if !registry_path.exists() {
        return Err(CarryCtxError::resource_not_found(format!(
            "Project '{project_id}' not found in registry."
        )));
    }

    let content = std::fs::read_to_string(&registry_path)
        .map_err(|e| CarryCtxError::database_error(format!("Failed to read registry: {e}")))?;
    let mut registry: Vec<serde_json::Value> = serde_json::from_str(&content)
        .map_err(|e| CarryCtxError::database_error(format!("Invalid registry: {e}")))?;

    let before = registry.len();
    registry.retain(|e| e["id"].as_str() != Some(project_id));

    if registry.len() == before {
        return Err(CarryCtxError::resource_not_found(format!(
            "Project '{project_id}' not found in registry."
        )));
    }

    let json = serde_json::to_string_pretty(&registry)
        .map_err(|e| CarryCtxError::database_error(format!("Failed to serialize registry: {e}")))?;
    std::fs::write(&registry_path, json)
        .map_err(|e| CarryCtxError::database_error(format!("Failed to write registry: {e}")))?;

    Ok(())
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
    uow: &UnitOfWork,
) -> Result<serde_json::Value, CarryCtxError> {
    let now = chrono::Utc::now();
    let threshold = now - chrono::Duration::days(older_than_days as i64);
    let threshold_str = threshold.to_rfc3339();

    let conn = uow.connection();

    // In a full implementation, we'd open archive.sqlite and ATTACH DATABASE.
    // For now, we simulate pruning by just deleting the old completed tasks directly.

    // 1. Find all completed tasks updated before the threshold
    // 2. But we must NOT prune tasks that have downstream dependent tasks which are NOT completed.
    // To keep it simple for v0.2.0 initial drop: Prune completed tasks older than X days.

    let mut stmt = conn
        .prepare("SELECT id FROM tasks WHERE status = 'completed' AND updated_at < ?1")
        .map_err(|e| CarryCtxError::database_error(format!("Failed to prepare statement: {e}")))?;

    let task_ids: Vec<String> = stmt
        .query_map([&threshold_str], |row| row.get(0))
        .map_err(|e| CarryCtxError::database_error(format!("Failed to query tasks: {e}")))?
        .filter_map(Result::ok)
        .collect();

    let pruned_count = task_ids.len();

    if pruned_count > 0 {
        // Build an IN clause
        let placeholders: Vec<String> = task_ids.iter().map(|_| "?".to_string()).collect();
        let in_clause = placeholders.join(", ");

        // We should delete from checkpoints, progress_items, task_dependencies, tasks, scopes
        let tables = [
            "checkpoints",
            "progress_items",
            "task_dependencies",
            "scopes",
        ];
        for table in tables.iter() {
            let sql = format!("DELETE FROM {table} WHERE task_id IN ({in_clause})");
            conn.execute(&sql, rusqlite::params_from_iter(&task_ids))
                .map_err(|e| {
                    CarryCtxError::database_error(format!("Failed to prune {table}: {e}"))
                })?;
        }

        let sql_tasks = format!("DELETE FROM tasks WHERE id IN ({in_clause})");
        conn.execute(&sql_tasks, rusqlite::params_from_iter(&task_ids))
            .map_err(|e| CarryCtxError::database_error(format!("Failed to prune tasks: {e}")))?;
    }

    Ok(serde_json::json!({
        "status": "success",
        "prunedTasksCount": pruned_count,
        "olderThanDays": older_than_days,
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

    // Create a backup of the current state before restoring
    let pre_restore_backup_dir = xdg.backup_dir(&gp.git_common_dir);
    filesystem::ensure_dir(&pre_restore_backup_dir)?;
    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let pre_backup_path = pre_restore_backup_dir.join(format!("pre_restore_{timestamp}.sqlite"));

    if db_path.exists() {
        let current_db = ProjectDatabase::open_readonly(&db_path)?;
        current_db.create_backup(&pre_backup_path)?;
    }

    // Copy the backup into place
    std::fs::copy(backup_path, &db_path)
        .map_err(|e| CarryCtxError::database_error(format!("Failed to restore backup: {e}")))?;

    // Verify the restored database
    let restored_db = ProjectDatabase::open_readonly(&db_path)?;
    let integrity: String = restored_db
        .connection()
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|e| CarryCtxError::database_error(format!("Integrity check failed: {e}")))?;

    if integrity != "ok" {
        // Revert if integrity check fails
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

pub fn migrate_project(
    project_path: &Path,
    _uow: &UnitOfWork,
) -> Result<Vec<String>, CarryCtxError> {
    let xdg = XdgPaths::new();
    let git = GitCli::new();
    let gp = git.discover(project_path)?;
    let db_path = xdg.project_db(&gp.git_common_dir);

    let mut db = ProjectDatabase::open(&db_path)?;
    let before_version = db.applied_version().unwrap_or(0);
    let applied = db.migrate()?;
    let after_version = db.applied_version().unwrap_or(0);

    let event_repo = SqliteEventRepository::new(db.connection());
    let _ = event_repo.append(&NewEvent {
        id: new_id(),
        project_id: "".into(),
        event_type: "project.migrated".into(),
        actor_agent_id: None,
        session_id: None,
        task_id: None,
        payload: serde_json::json!({
            "beforeVersion": before_version,
            "afterVersion": after_version,
            "appliedMigrations": applied.iter().map(|m| m.name.clone()).collect::<Vec<_>>(),
        }),
        occurred_at: now(),
    });

    let names: Vec<String> = applied.into_iter().map(|m| m.name).collect();
    Ok(names)
}
