use crate::adapter::sqlite_repos::{
    SqliteAgentRepository, SqliteDependencyRepository, SqliteEventRepository, SqliteTaskRepository,
};
use crate::adapter::unit_of_work::UnitOfWork;
use crate::domain::dependency::{DependencyEdge, DependencyKind, validate_dependency_edge};
use crate::domain::ids::format_display_id;
use crate::domain::task::{
    TaskPriority, TaskStatus, TransitionAction, TransitionFacts, evaluate_transition,
    initial_status,
};
use crate::error::CarryCtxError;
use crate::repository::agent::AgentRepository;
use crate::repository::dependency::DependencyRepository;
use crate::repository::event::{EventRepository, NewEvent};
use crate::repository::task::{NewTask, TaskFilter, TaskRecord, TaskRepository};

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn new_id() -> String {
    ulid::Ulid::generate().to_string()
}

fn resolve_task(
    project_id: &str,
    ref_: &str,
    repo: &SqliteTaskRepository,
) -> Result<TaskRecord, CarryCtxError> {
    if let Some(task) = repo.find_by_display_id(project_id, ref_)? {
        return Ok(task);
    }
    if let Some(task) = repo.find_by_id(project_id, ref_)? {
        return Ok(task);
    }
    Err(CarryCtxError::resource_not_found(format!(
        "Task '{ref_}' not found."
    )))
}

fn task_event_payload(task: &TaskRecord) -> serde_json::Value {
    serde_json::json!({
        "id": task.id,
        "displayId": task.display_id,
        "title": task.title,
        "status": task.status,
        "priority": task.priority,
    })
}

/// Create a new task
pub fn create_task(
    project_id: &str,
    title: &str,
    prefix: Option<&str>,
    status: Option<TaskStatus>,
    priority: Option<TaskPriority>,
    owner_agent_id: Option<&str>,
    depends_on: &[String],
    actor_agent_id: Option<&str>,
    uow: &UnitOfWork,
) -> Result<TaskRecord, CarryCtxError> {
    if title.trim().is_empty() {
        return Err(CarryCtxError::validation_error(
            "Task title cannot be empty.",
        ));
    }

    let now = now();
    let task_id = new_id();
    let conn = uow.connection();
    let task_repo = SqliteTaskRepository::new(conn);
    let dep_repo = SqliteDependencyRepository::new(conn);
    let event_repo = SqliteEventRepository::new(conn);

    // Resolve prerequisites
    let mut prerequisites = Vec::new();
    for dep_ref in depends_on {
        let found = resolve_task(project_id, dep_ref, &task_repo)?;
        prerequisites.push(found);
    }

    // Validate no cycles
    let all_edges = dep_repo.list_all_for_project(project_id)?;
    for prereq in &prerequisites {
        if let Err(msg) = validate_dependency_edge(&all_edges, &task_id, &prereq.id) {
            return Err(CarryCtxError::dependency_cycle()
                .with_details(serde_json::json!({ "message": msg })));
        }
    }

    // Check incomplete strong dependencies
    let incomplete_strong: Vec<&TaskRecord> = prerequisites
        .iter()
        .filter(|p| p.status != TaskStatus::Completed)
        .collect();

    if !incomplete_strong.is_empty() && status == Some(TaskStatus::Ready) {
        return Err(CarryCtxError::state_conflict(format!(
            "Cannot create task in 'ready' status with {} incomplete strong prerequisite(s).",
            incomplete_strong.len()
        )));
    }

    // Determine initial status
    let final_status =
        status.unwrap_or_else(|| initial_status(incomplete_strong.is_empty(), false));

    let display_seq = task_repo.allocate_display_id(project_id, prefix.unwrap_or("CTX"))?;
    let display_id = format_display_id(prefix.unwrap_or("CTX"), display_seq);

    let agent_repo = SqliteAgentRepository::new(conn);
    let resolved_owner_id = match owner_agent_id {
        Some(ref_) if !ref_.trim().is_empty() => {
            Some(resolve_agent_id(project_id, ref_, &agent_repo)?)
        }
        _ => None,
    };
    let resolved_actor_id = match actor_agent_id {
        Some(ref_) if !ref_.trim().is_empty() => {
            Some(resolve_agent_id(project_id, ref_, &agent_repo)?)
        }
        _ => None,
    };

    let task = task_repo.create(
        &NewTask {
            id: task_id.clone(),
            display_id: display_id.clone(),
            project_id: project_id.to_string(),
            title: title.trim().to_string(),
            description: None,
            status: final_status,
            priority: priority.unwrap_or_default(),
            owner_agent_id: resolved_owner_id,
            parent_task_id: None,
        },
        &now,
    )?;

    event_repo.append(&NewEvent {
        id: new_id(),
        project_id: project_id.to_string(),
        event_type: "task.created".into(),
        actor_agent_id: resolved_actor_id,
        session_id: None,
        task_id: Some(task.id.clone()),
        payload: task_event_payload(&task),
        occurred_at: now.clone(),
    })?;

    // Create dependency edges
    for prereq in &prerequisites {
        let dep_kind = DependencyKind::Strong;
        dep_repo.add(project_id, &task.id, &prereq.id, dep_kind)?;

        event_repo.append(&NewEvent {
            id: new_id(),
            project_id: project_id.to_string(),
            event_type: "task.dependency_added".into(),
            actor_agent_id: actor_agent_id.map(|s| s.to_string()),
            session_id: None,
            task_id: Some(task.id.clone()),
            payload: serde_json::json!({
                "taskId": task.id,
                "taskDisplayId": task.display_id,
                "prerequisiteTaskId": prereq.id,
                "prerequisiteDisplayId": prereq.display_id,
                "kind": "strong",
            }),
            occurred_at: now.clone(),
        })?;
    }

    Ok(task)
}

/// List tasks with optional filtering
pub fn list_tasks(
    _project_id: &str,
    filter: &TaskFilter,
    uow: &UnitOfWork,
) -> Result<Vec<TaskRecord>, CarryCtxError> {
    let conn = uow.connection();
    let repo = SqliteTaskRepository::new(conn);
    repo.list(filter)
}

/// Show a single task by display_id or id
pub fn show_task(
    project_id: &str,
    ref_: &str,
    uow: &UnitOfWork,
) -> Result<TaskRecord, CarryCtxError> {
    let conn = uow.connection();
    let repo = SqliteTaskRepository::new(conn);
    resolve_task(project_id, ref_, &repo)
}

/// Edit a task's title, priority, or metadata
pub fn edit_task(
    project_id: &str,
    ref_: &str,
    title: Option<&str>,
    priority: Option<TaskPriority>,
    actor_agent_id: Option<&str>,
    uow: &UnitOfWork,
) -> Result<TaskRecord, CarryCtxError> {
    if let Some(t) = title {
        if t.trim().is_empty() {
            return Err(CarryCtxError::validation_error(
                "Task title cannot be empty.",
            ));
        }
    }

    let now = now();
    let conn = uow.connection();
    let task_repo = SqliteTaskRepository::new(conn);
    let event_repo = SqliteEventRepository::new(conn);

    let existing = resolve_task(project_id, ref_, &task_repo)?;

    let before_title = existing.title.clone();
    let before_priority = existing.priority;

    let final_title = title
        .map(|t| t.trim().to_string())
        .unwrap_or(existing.title.clone());
    let final_priority = priority.unwrap_or(existing.priority);

    let updated = task_repo.edit(&existing.id, project_id, &final_title, final_priority, &now)?;

    event_repo.append(&NewEvent {
        id: new_id(),
        project_id: project_id.to_string(),
        event_type: "task.edited".into(),
        actor_agent_id: actor_agent_id.map(|s| s.to_string()),
        session_id: None,
        task_id: Some(existing.id.clone()),
        payload: serde_json::json!({
            "id": existing.id,
            "before": {
                "title": before_title,
                "priority": before_priority,
            },
            "after": {
                "title": updated.title,
                "priority": updated.priority,
            },
        }),
        occurred_at: now,
    })?;

    Ok(updated)
}

fn resolve_agent_id(
    project_id: &str,
    agent_ref: &str,
    repo: &SqliteAgentRepository,
) -> Result<String, CarryCtxError> {
    if let Some(agent) = repo.find_by_name(project_id, agent_ref)? {
        return Ok(agent.id);
    }
    if let Some(agent) = repo.find_by_id(project_id, agent_ref)? {
        return Ok(agent.id);
    }
    Err(CarryCtxError::resource_not_found(format!(
        "Agent '{agent_ref}' not found."
    )))
}

/// Claim a task: assign to the calling agent and set status to in_progress
pub fn claim_task(
    project_id: &str,
    ref_: &str,
    actor_agent_ref: &str,
    uow: &UnitOfWork,
) -> Result<TaskRecord, CarryCtxError> {
    let now = now();
    let conn = uow.connection();
    let task_repo = SqliteTaskRepository::new(conn);
    let agent_repo = SqliteAgentRepository::new(conn);
    let event_repo = SqliteEventRepository::new(conn);

    let actor_agent_id = resolve_agent_id(project_id, actor_agent_ref, &agent_repo)?;

    let existing = resolve_task(project_id, ref_, &task_repo)?;

    // Pre-conditions from TS reference
    if existing.status != TaskStatus::Ready || existing.owner_agent_id.is_some() {
        if let Some(ref owner) = existing.owner_agent_id {
            if owner != &actor_agent_id {
                return Err(CarryCtxError::task_already_claimed(
                    &existing.display_id,
                    owner,
                ));
            }
        }
        return Err(CarryCtxError::invalid_task_transition(
            &format!("{:?}", existing.status),
            "claim",
        ));
    }

    let incomplete_deps =
        task_repo.list_incomplete_strong_dependencies(project_id, &existing.id)?;
    if !incomplete_deps.is_empty() {
        return Err(CarryCtxError::dependency_incomplete(&existing.display_id));
    }

    let updated = task_repo.update_status(
        &existing.id,
        project_id,
        TaskStatus::InProgress,
        Some(actor_agent_id.to_string()),
        &now,
    )?;

    event_repo.append(&NewEvent {
        id: new_id(),
        project_id: project_id.to_string(),
        event_type: "task.claimed".into(),
        actor_agent_id: Some(actor_agent_id.to_string()),
        session_id: None,
        task_id: Some(existing.id.clone()),
        payload: serde_json::json!({
            "id": existing.id,
            "ownerAgentId": actor_agent_id,
        }),
        occurred_at: now,
    })?;

    Ok(updated)
}

/// Transition a task to a new status based on an action
pub fn transition_task(
    project_id: &str,
    ref_: &str,
    action: TransitionAction,
    reason: Option<&str>,
    strict_completion: bool,
    actor_agent_id: Option<&str>,
    uow: &UnitOfWork,
) -> Result<(TaskRecord, Vec<String>), CarryCtxError> {
    let now = now();
    let conn = uow.connection();
    let task_repo = SqliteTaskRepository::new(conn);
    let dep_repo = SqliteDependencyRepository::new(conn);
    let event_repo = SqliteEventRepository::new(conn);

    let existing = resolve_task(project_id, ref_, &task_repo)?;

    let incomplete_deps =
        task_repo.list_incomplete_strong_dependencies(project_id, &existing.id)?;
    let count_open_progress = task_repo.count_open_progress(project_id, &existing.id)?;
    let has_active_session = task_repo.has_active_session(project_id, &existing.id)?;

    let facts = TransitionFacts {
        has_owner: existing.owner_agent_id.is_some(),
        strong_dependencies_complete: incomplete_deps.is_empty(),
        has_active_session,
        has_open_progress: count_open_progress > 0,
        strict_completion,
        reason: reason.map(|s| s.to_string()),
        task_display_id: existing.display_id.clone(),
        owner: existing.owner_agent_id.clone(),
    };

    let outcome = evaluate_transition(existing.status, action, &facts);
    let (new_status, clears_owner, warnings) = outcome.allowed()?;

    let next_owner = if clears_owner {
        None
    } else {
        existing.owner_agent_id.clone()
    };

    let updated =
        task_repo.update_status(&existing.id, project_id, new_status, next_owner, &now)?;

    let event_type = format!("task.{}ed", action.name());
    event_repo.append(&NewEvent {
        id: new_id(),
        project_id: project_id.to_string(),
        event_type,
        actor_agent_id: actor_agent_id.map(|s| s.to_string()),
        session_id: None,
        task_id: Some(existing.id.clone()),
        payload: serde_json::json!({
            "id": existing.id,
            "beforeStatus": existing.status,
            "afterStatus": updated.status,
            "reason": reason,
        }),
        occurred_at: now.clone(),
    })?;

    // If this task just became Completed, any tasks that depend on it may now be
    // unblocked. Promote each dependent still sitting in Planned (with no owner
    // and no other incomplete strong dependency) to Ready.
    if updated.status == TaskStatus::Completed {
        let all_edges = dep_repo.list_all_for_project(project_id)?;
        let dependents: Vec<String> = all_edges
            .iter()
            .filter(|e| e.prerequisite_id == existing.id && e.kind == DependencyKind::Strong)
            .map(|e| e.task_id.clone())
            .collect();

        for dependent_id in dependents {
            if let Some(dependent) = task_repo.find_by_id(project_id, &dependent_id)? {
                if dependent.status != TaskStatus::Planned || dependent.owner_agent_id.is_some() {
                    continue;
                }
                let remaining_incomplete =
                    task_repo.list_incomplete_strong_dependencies(project_id, &dependent.id)?;
                if remaining_incomplete.is_empty() {
                    task_repo.update_status(
                        &dependent.id,
                        project_id,
                        TaskStatus::Ready,
                        None,
                        &now,
                    )?;
                    event_repo.append(&NewEvent {
                        id: new_id(),
                        project_id: project_id.to_string(),
                        event_type: "task.unblocked".into(),
                        actor_agent_id: actor_agent_id.map(|s| s.to_string()),
                        session_id: None,
                        task_id: Some(dependent.id.clone()),
                        payload: serde_json::json!({
                            "id": dependent.id,
                            "displayId": dependent.display_id,
                            "beforeStatus": "planned",
                            "afterStatus": "ready",
                            "unblockedBy": existing.id,
                        }),
                        occurred_at: now.clone(),
                    })?;
                }
            }
        }
    }

    Ok((updated, warnings))
}

/// Add a dependency edge from task to prerequisite
pub fn add_dependency(
    project_id: &str,
    task_ref: &str,
    prerequisite_ref: &str,
    kind: DependencyKind,
    actor_agent_id: Option<&str>,
    uow: &UnitOfWork,
) -> Result<TaskRecord, CarryCtxError> {
    let now = now();
    let conn = uow.connection();
    let task_repo = SqliteTaskRepository::new(conn);
    let dep_repo = SqliteDependencyRepository::new(conn);
    let event_repo = SqliteEventRepository::new(conn);

    let task = resolve_task(project_id, task_ref, &task_repo)?;
    let prerequisite = resolve_task(project_id, prerequisite_ref, &task_repo)?;

    let all_edges = dep_repo.list_all_for_project(project_id)?;
    let new_edge = DependencyEdge {
        task_id: task.id.clone(),
        prerequisite_id: prerequisite.id.clone(),
        kind,
    };

    let all_edges_ref: Vec<DependencyEdge> = all_edges
        .iter()
        .map(|e| DependencyEdge {
            task_id: e.task_id.clone(),
            prerequisite_id: e.prerequisite_id.clone(),
            kind: e.kind,
        })
        .collect();

    if let Err(msg) =
        validate_dependency_edge(&all_edges_ref, &new_edge.task_id, &new_edge.prerequisite_id)
    {
        return Err(
            CarryCtxError::dependency_cycle().with_details(serde_json::json!({ "message": msg }))
        );
    }

    dep_repo.add(project_id, &task.id, &prerequisite.id, kind)?;

    // If adding a strong dep to an incomplete task, downgrade status from ready to planned
    let mut updated_task = task.clone();
    if kind == DependencyKind::Strong
        && prerequisite.status != TaskStatus::Completed
        && task.status == TaskStatus::Ready
        && task.owner_agent_id.is_none()
    {
        updated_task =
            task_repo.update_status(&task.id, project_id, TaskStatus::Planned, None, &now)?;
    }

    event_repo.append(&NewEvent {
        id: new_id(),
        project_id: project_id.to_string(),
        event_type: "task.dependency_added".into(),
        actor_agent_id: actor_agent_id.map(|s| s.to_string()),
        session_id: None,
        task_id: Some(task.id.clone()),
        payload: serde_json::json!({
            "taskId": task.id,
            "taskDisplayId": task.display_id,
            "prerequisiteTaskId": prerequisite.id,
            "prerequisiteDisplayId": prerequisite.display_id,
            "kind": kind,
        }),
        occurred_at: now,
    })?;

    Ok(updated_task)
}

/// Remove a dependency edge
pub fn remove_dependency(
    project_id: &str,
    task_ref: &str,
    prerequisite_ref: &str,
    actor_agent_id: Option<&str>,
    uow: &UnitOfWork,
) -> Result<TaskRecord, CarryCtxError> {
    let now = now();
    let conn = uow.connection();
    let task_repo = SqliteTaskRepository::new(conn);
    let dep_repo = SqliteDependencyRepository::new(conn);
    let event_repo = SqliteEventRepository::new(conn);

    let task = resolve_task(project_id, task_ref, &task_repo)?;
    let prerequisite = resolve_task(project_id, prerequisite_ref, &task_repo)?;

    dep_repo.remove(project_id, &task.id, &prerequisite.id)?;

    // If the task was planned and had no owner, check if it can go back to ready
    let mut updated_task = task.clone();
    let remaining_incomplete =
        task_repo.list_incomplete_strong_dependencies(project_id, &task.id)?;
    if task.status == TaskStatus::Planned
        && task.owner_agent_id.is_none()
        && remaining_incomplete.is_empty()
    {
        updated_task =
            task_repo.update_status(&task.id, project_id, TaskStatus::Ready, None, &now)?;
    }

    event_repo.append(&NewEvent {
        id: new_id(),
        project_id: project_id.to_string(),
        event_type: "task.dependency_removed".into(),
        actor_agent_id: actor_agent_id.map(|s| s.to_string()),
        session_id: None,
        task_id: Some(task.id.clone()),
        payload: serde_json::json!({
            "taskId": task.id,
            "taskDisplayId": task.display_id,
            "prerequisiteTaskId": prerequisite.id,
            "prerequisiteDisplayId": prerequisite.display_id,
        }),
        occurred_at: now,
    })?;

    Ok(updated_task)
}
