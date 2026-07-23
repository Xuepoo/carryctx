use std::path::Path;

use crate::adapter::filesystem::{self, JournalEntry};
use crate::adapter::git::GitCli;
use crate::error::CarryCtxError;
use crate::repository::{
    EventRepository, NewEvent, NewWorktree, TaskRepository, WorktreeRecord, WorktreeRepository,
};

pub struct BindWorktreeInput {
    pub project_id: String,
    pub path: String,
    pub task_id: Option<String>,
}

pub fn bind_worktree(
    worktree_repo: &dyn WorktreeRepository,
    task_repo: &dyn TaskRepository,
    event_repo: &dyn EventRepository,
    git_cli: &GitCli,
    input: &BindWorktreeInput,
    now: &str,
) -> Result<WorktreeRecord, CarryCtxError> {
    let path = Path::new(&input.path);
    let discovery = git_cli.discover(path)?;

    let mut task_id: Option<String> = None;
    if let Some(ref t) = input.task_id {
        let task = task_repo
            .find_by_display_id(&input.project_id, t)?
            .or_else(|| task_repo.find_by_id(&input.project_id, t).ok().flatten())
            .ok_or_else(|| CarryCtxError::resource_not_found(format!("Task '{}' not found", t)))?;

        let existing_bound = worktree_repo.find_by_task_id(&input.project_id, &task.id)?;
        if let Some(ref wt) = existing_bound {
            if wt.path != discovery.repository_root.to_string_lossy() {
                return Err(CarryCtxError::state_conflict(format!(
                    "Task '{}' is already bound to worktree '{}'",
                    task.display_id, wt.path
                )));
            }
        }

        task_id = Some(task.id);
    }

    let existing = worktree_repo.find_by_path(
        &input.project_id,
        &discovery.repository_root.to_string_lossy(),
    )?;
    let worktree_id = existing
        .as_ref()
        .map(|w| w.id.clone())
        .unwrap_or_else(|| ulid::Ulid::generate().to_string());

    let record = worktree_repo.upsert(
        &NewWorktree {
            id: worktree_id,
            project_id: input.project_id.clone(),
            path: discovery.repository_root.to_string_lossy().to_string(),
            branch: discovery.branch.clone(),
            head: discovery.head.clone(),
            task_id,
        },
        now,
    )?;

    event_repo.append(&NewEvent {
        id: ulid::Ulid::generate().to_string(),
        project_id: input.project_id.clone(),
        event_type: "worktree.bound".into(),
        actor_agent_id: None,
        session_id: None,
        task_id: record.task_id.clone(),
        payload: serde_json::json!({
            "worktree_id": record.id,
            "path": record.path,
            "task_id": record.task_id,
            "branch": record.branch,
            "head": record.head,
        }),
        occurred_at: now.to_string(),
    })?;

    Ok(record)
}

pub fn unbind_worktree(
    worktree_repo: &dyn WorktreeRepository,
    event_repo: &dyn EventRepository,
    project_id: &str,
    path_or_id: &str,
    now: &str,
) -> Result<WorktreeRecord, CarryCtxError> {
    let worktree = worktree_repo
        .find_by_id(project_id, path_or_id)?
        .or_else(|| {
            worktree_repo
                .find_by_path(project_id, path_or_id)
                .ok()
                .flatten()
        })
        .ok_or_else(|| {
            CarryCtxError::resource_not_found(format!("Worktree '{}' not found", path_or_id))
        })?;

    let updated = worktree_repo.unbind_task(&worktree.id, project_id, now)?;

    event_repo.append(&NewEvent {
        id: ulid::Ulid::generate().to_string(),
        project_id: project_id.to_string(),
        event_type: "worktree.unbound".into(),
        actor_agent_id: None,
        session_id: None,
        task_id: None,
        payload: serde_json::json!({
            "worktree_id": updated.id,
            "path": updated.path,
        }),
        occurred_at: now.to_string(),
    })?;

    Ok(updated)
}

pub struct CreateWorktreeInput {
    pub project_id: String,
    pub repository_root: String,
    pub path: String,
    pub branch: String,
    pub base: Option<String>,
    pub task_id: Option<String>,
}

pub fn create_worktree(
    worktree_repo: &dyn WorktreeRepository,
    task_repo: &dyn TaskRepository,
    event_repo: &dyn EventRepository,
    git_cli: &GitCli,
    xdg_paths: &crate::adapter::xdg::XdgPaths,
    input: &CreateWorktreeInput,
    now: &str,
) -> Result<WorktreeRecord, CarryCtxError> {
    let worktree_path = Path::new(&input.path);
    if worktree_path.exists() {
        return Err(CarryCtxError::invalid_arguments(format!(
            "Worktree path '{}' already exists",
            input.path
        )));
    }

    let branch_exists = git_cli.has_branch(Path::new(&input.repository_root), &input.branch)?;
    if branch_exists {
        return Err(CarryCtxError::state_conflict(format!(
            "Branch '{}' already exists",
            input.branch
        )));
    }

    let operation_id = ulid::Ulid::generate().to_string();
    let git_project = git_cli.discover(Path::new(&input.repository_root))?;
    let journal_dir = xdg_paths.journal_dir(&git_project.git_common_dir);

    let journal_entry = JournalEntry {
        operation_id: operation_id.clone(),
        kind: "worktree.create".into(),
        status: "running".into(),
        created_at: now.to_string(),
        metadata: serde_json::json!({
            "path": input.path,
            "branch": input.branch,
            "base": input.base,
        }),
    };
    filesystem::write_journal(&journal_dir, &journal_entry)?;

    let create_result = git_cli.create_worktree(
        Path::new(&input.repository_root),
        worktree_path,
        &input.branch,
        input.base.as_deref(),
    );

    if let Err(ref e) = create_result {
        let failed_entry = JournalEntry {
            operation_id,
            kind: "worktree.create".into(),
            status: "failed".into(),
            created_at: now.to_string(),
            metadata: serde_json::json!({
                "error": e.to_string(),
            }),
        };
        let _ = filesystem::write_journal(&journal_dir, &failed_entry);
        return Err(CarryCtxError::git_error(format!(
            "Failed to create worktree: {}",
            e
        )));
    }

    let bind_result = bind_worktree(
        worktree_repo,
        task_repo,
        event_repo,
        git_cli,
        &BindWorktreeInput {
            project_id: input.project_id.clone(),
            path: input.path.clone(),
            task_id: input.task_id.clone(),
        },
        now,
    );

    let completed_entry = JournalEntry {
        operation_id: operation_id.clone(),
        kind: "worktree.create".into(),
        status: if bind_result.is_ok() {
            "completed"
        } else {
            "failed"
        }
        .into(),
        created_at: now.to_string(),
        metadata: serde_json::json!({
            "path": input.path,
            "branch": input.branch,
            "success": bind_result.is_ok(),
        }),
    };
    let _ = filesystem::write_journal(&journal_dir, &completed_entry);

    if bind_result.is_ok() {
        let _ = filesystem::remove_journal(&journal_dir, &operation_id);
    }

    bind_result
}

pub fn list_worktrees(
    worktree_repo: &dyn WorktreeRepository,
    git_cli: &GitCli,
    project_id: &str,
    repository_root: Option<&str>,
) -> Result<Vec<WorktreeRecord>, CarryCtxError> {
    let mut records = worktree_repo.list(project_id)?;

    if let Some(root) = repository_root {
        if let Ok(git_trees) = git_cli.list_worktrees(Path::new(root)) {
            let db_paths: std::collections::HashSet<String> =
                records.iter().map(|w| w.path.clone()).collect();

            for gt in &git_trees {
                if !gt.detached && !db_paths.contains(&gt.path) {
                    records.push(WorktreeRecord {
                        id: String::new(),
                        project_id: project_id.to_string(),
                        path: gt.path.clone(),
                        branch: gt.branch.clone(),
                        head: gt.head.clone(),
                        task_id: None,
                        created_at: String::new(),
                        updated_at: String::new(),
                    });
                }
            }
        }
    }

    Ok(records)
}

pub fn show_worktree(
    worktree_repo: &dyn WorktreeRepository,
    git_cli: &GitCli,
    project_id: &str,
    path_or_id: &str,
) -> Result<WorktreeRecord, CarryCtxError> {
    let mut record = worktree_repo
        .find_by_id(project_id, path_or_id)?
        .or_else(|| {
            worktree_repo
                .find_by_path(project_id, path_or_id)
                .ok()
                .flatten()
        })
        .ok_or_else(|| {
            CarryCtxError::resource_not_found(format!("Worktree '{}' not found", path_or_id))
        })?;

    if let Ok(snapshot) = git_cli.get_snapshot(Path::new(&record.path)) {
        record.branch = snapshot.branch;
        record.head = Some(snapshot.head);
    }

    Ok(record)
}
