use crate::domain::ids::format_display_id;
use crate::domain::progress::{ProgressAction, ProgressType, evaluate_progress_transition};
use crate::error::CarryCtxError;
use crate::repository::{
    EventRepository, NewEvent, NewProgressItem, ProgressFilter, ProgressItemRecord,
    ProgressRepository, TaskRepository,
};

pub struct CreateProgressInput {
    pub project_id: String,
    pub task_id: String,
    pub source_session_id: Option<String>,
    pub item_type: ProgressType,
    pub content: String,
}

pub fn create_progress(
    progress_repo: &dyn ProgressRepository,
    task_repo: &dyn TaskRepository,
    event_repo: &dyn EventRepository,
    input: &CreateProgressInput,
    now: &str,
) -> Result<ProgressItemRecord, CarryCtxError> {
    if input.content.trim().is_empty() {
        return Err(CarryCtxError::validation_error(
            "Progress content cannot be empty.",
        ));
    }

    let task = task_repo
        .find_by_id(&input.project_id, &input.task_id)?
        .or_else(|| {
            task_repo
                .find_by_display_id(&input.project_id, &input.task_id)
                .ok()
                .flatten()
        })
        .ok_or_else(|| {
            CarryCtxError::resource_not_found(format!("Task '{}' not found", input.task_id))
        })?;

    let seq = progress_repo.allocate_display_id(&input.project_id)?;
    let display_id = format_display_id("PX", seq);
    let position = progress_repo.get_next_position(&input.project_id, &task.id)?;

    let item = progress_repo.create(
        &NewProgressItem {
            id: ulid::Ulid::new().to_string(),
            display_id,
            project_id: input.project_id.clone(),
            task_id: task.id.clone(),
            source_session_id: input.source_session_id.clone(),
            item_type: input.item_type,
            content: input.content.trim().to_string(),
            position,
        },
        now,
    )?;

    event_repo.append(&NewEvent {
        id: ulid::Ulid::new().to_string(),
        project_id: input.project_id.clone(),
        event_type: "progress.created".into(),
        actor_agent_id: None,
        session_id: None,
        task_id: Some(task.id.clone()),
        payload: serde_json::json!({
            "id": item.id,
            "display_id": item.display_id,
            "task_id": task.id,
            "item_type": item.item_type,
            "status": item.status,
            "content": item.content,
        }),
        occurred_at: now.to_string(),
    })?;

    Ok(item)
}

pub struct EditProgressInput {
    pub project_id: String,
    pub ref_or_id: String,
    pub content: String,
}

pub fn edit_progress(
    progress_repo: &dyn ProgressRepository,
    event_repo: &dyn EventRepository,
    input: &EditProgressInput,
    now: &str,
) -> Result<ProgressItemRecord, CarryCtxError> {
    if input.content.trim().is_empty() {
        return Err(CarryCtxError::validation_error(
            "Progress content cannot be empty.",
        ));
    }

    let existing = resolve_progress(progress_repo, &input.project_id, &input.ref_or_id)?;

    let updated = progress_repo.edit(&existing.id, &input.project_id, input.content.trim(), now)?;

    event_repo.append(&NewEvent {
        id: ulid::Ulid::new().to_string(),
        project_id: input.project_id.clone(),
        event_type: "progress.edited".into(),
        actor_agent_id: None,
        session_id: None,
        task_id: Some(existing.task_id.clone()),
        payload: serde_json::json!({
            "id": existing.id,
            "before": { "content": existing.content },
            "after": { "content": updated.content },
        }),
        occurred_at: now.to_string(),
    })?;

    Ok(updated)
}

pub fn complete_progress(
    progress_repo: &dyn ProgressRepository,
    event_repo: &dyn EventRepository,
    project_id: &str,
    ref_or_id: &str,
    now: &str,
) -> Result<ProgressItemRecord, CarryCtxError> {
    transition_progress_item(
        progress_repo,
        event_repo,
        project_id,
        ref_or_id,
        ProgressAction::Complete,
        now,
    )
}

pub fn reopen_progress(
    progress_repo: &dyn ProgressRepository,
    event_repo: &dyn EventRepository,
    project_id: &str,
    ref_or_id: &str,
    now: &str,
) -> Result<ProgressItemRecord, CarryCtxError> {
    transition_progress_item(
        progress_repo,
        event_repo,
        project_id,
        ref_or_id,
        ProgressAction::Reopen,
        now,
    )
}

pub fn remove_progress(
    progress_repo: &dyn ProgressRepository,
    event_repo: &dyn EventRepository,
    project_id: &str,
    ref_or_id: &str,
    now: &str,
) -> Result<ProgressItemRecord, CarryCtxError> {
    transition_progress_item(
        progress_repo,
        event_repo,
        project_id,
        ref_or_id,
        ProgressAction::Remove,
        now,
    )
}

fn transition_progress_item(
    progress_repo: &dyn ProgressRepository,
    event_repo: &dyn EventRepository,
    project_id: &str,
    ref_or_id: &str,
    action: ProgressAction,
    now: &str,
) -> Result<ProgressItemRecord, CarryCtxError> {
    let existing = resolve_progress(progress_repo, project_id, ref_or_id)?;

    let new_status = evaluate_progress_transition(existing.status, action)?;

    let updated = progress_repo.update_status(&existing.id, project_id, new_status, now)?;

    let event_type = match action {
        ProgressAction::Complete => "progress.completed",
        ProgressAction::Reopen => "progress.reopened",
        ProgressAction::Remove => "progress.removed",
    };

    event_repo.append(&NewEvent {
        id: ulid::Ulid::new().to_string(),
        project_id: project_id.to_string(),
        event_type: event_type.into(),
        actor_agent_id: None,
        session_id: None,
        task_id: Some(existing.task_id.clone()),
        payload: serde_json::json!({
            "id": existing.id,
            "before_status": existing.status,
            "after_status": updated.status,
        }),
        occurred_at: now.to_string(),
    })?;

    Ok(updated)
}

pub struct ReorderProgressInput {
    pub project_id: String,
    pub task_id: String,
    pub ordered_refs: Vec<String>,
}

pub fn reorder_progress(
    progress_repo: &dyn ProgressRepository,
    task_repo: &dyn TaskRepository,
    event_repo: &dyn EventRepository,
    input: &ReorderProgressInput,
    now: &str,
) -> Result<(), CarryCtxError> {
    let task = task_repo
        .find_by_id(&input.project_id, &input.task_id)?
        .or_else(|| {
            task_repo
                .find_by_display_id(&input.project_id, &input.task_id)
                .ok()
                .flatten()
        })
        .ok_or_else(|| {
            CarryCtxError::resource_not_found(format!("Task '{}' not found", input.task_id))
        })?;

    let mut resolved_ids = Vec::new();
    for r in &input.ordered_refs {
        let item = resolve_progress(progress_repo, &input.project_id, r)?;
        resolved_ids.push(item.id.clone());
    }

    progress_repo.reorder(&input.project_id, &task.id, &resolved_ids)?;

    event_repo.append(&NewEvent {
        id: ulid::Ulid::new().to_string(),
        project_id: input.project_id.clone(),
        event_type: "progress.reordered".into(),
        actor_agent_id: None,
        session_id: None,
        task_id: Some(task.id.clone()),
        payload: serde_json::json!({
            "task_id": task.id,
            "ordered_ids": resolved_ids,
        }),
        occurred_at: now.to_string(),
    })?;

    Ok(())
}

pub fn list_progress(
    progress_repo: &dyn ProgressRepository,
    filter: &ProgressFilter,
) -> Result<Vec<ProgressItemRecord>, CarryCtxError> {
    progress_repo.list(filter)
}

fn resolve_progress(
    progress_repo: &dyn ProgressRepository,
    project_id: &str,
    ref_or_id: &str,
) -> Result<ProgressItemRecord, CarryCtxError> {
    let item = progress_repo
        .find_by_display_id(project_id, ref_or_id)?
        .or_else(|| {
            progress_repo
                .find_by_id(project_id, ref_or_id)
                .ok()
                .flatten()
        })
        .ok_or_else(|| {
            CarryCtxError::resource_not_found(format!("Progress item '{}' not found", ref_or_id))
        })?;
    Ok(item)
}
