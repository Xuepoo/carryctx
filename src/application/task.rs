use crate::adapter::sqlite_repos::{
    SqliteAgentRepository, SqliteDependencyRepository, SqliteEventRepository, SqliteTaskRepository,
    SqliteTeamRepository,
};
use crate::adapter::unit_of_work::UnitOfWork;
use crate::domain::dependency::{DependencyEdge, DependencyKind, validate_dependency_edge};
use crate::domain::ids::{format_display_id, validate_task_prefix};
use crate::domain::task::{
    TaskPriority, TaskStatus, TransitionAction, TransitionFacts, evaluate_transition,
    initial_status, prerequisite_settled, validate_description, validate_title,
};
use crate::error::CarryCtxError;
use crate::repository::TeamRepository;
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

/// Canonicalize an actor reference for audit events: resolve the agent name
/// or ULID once at use-case entry and store the internal id everywhere.
///
/// Resolution is best-effort by design: an unknown reference (e.g. a raw
/// `--agent` value that was never registered) is stored verbatim rather than
/// rejected, so existing flows that pass unregistered names keep working.
/// Only a genuine database error propagates. This mirrors `claim_task`, which
/// already resolved its actor, so the event stream no longer mixes identities
/// and agent-filtered queries cannot miss rows.
pub(super) fn canonical_actor_id(
    project_id: &str,
    actor_ref: Option<&str>,
    repo: &SqliteAgentRepository,
) -> Result<Option<String>, CarryCtxError> {
    let Some(actor_ref) = actor_ref.map(str::trim).filter(|r| !r.is_empty()) else {
        return Ok(None);
    };
    if let Some(agent) = repo.find_by_name(project_id, actor_ref)? {
        return Ok(Some(agent.id));
    }
    if let Some(agent) = repo.find_by_id(project_id, actor_ref)? {
        return Ok(Some(agent.id));
    }
    Ok(Some(actor_ref.to_string()))
}

/// Enforce the documented length caps for fields that are duplicated into
/// event payloads (`MAX_TITLE_CHARS` / `MAX_DESCRIPTION_CHARS`).
fn validate_text_lengths(
    title: Option<&str>,
    description: Option<&str>,
) -> Result<(), CarryCtxError> {
    if let Some(title) = title {
        validate_title(title.trim())?;
    }
    if let Some(description) = description.map(str::trim).filter(|d| !d.is_empty()) {
        validate_description(description)?;
    }
    Ok(())
}

fn task_event_payload(task: &TaskRecord) -> serde_json::Value {
    serde_json::json!({
        "id": task.id,
        "displayId": task.display_id,
        "title": task.title,
        "description": task.description,
        "status": task.status,
        "priority": task.priority,
    })
}

/// Create a new task
pub fn create_task(
    project_id: &str,
    title: &str,
    description: Option<&str>,
    prefix: Option<&str>,
    status: Option<TaskStatus>,
    priority: Option<TaskPriority>,
    owner_agent_id: Option<&str>,
    required_role: Option<&str>,
    team_id: Option<&str>,
    depends_on: &[String],
    actor_agent_id: Option<&str>,
    uow: &UnitOfWork,
) -> Result<TaskRecord, CarryCtxError> {
    if title.trim().is_empty() {
        return Err(CarryCtxError::validation_error(
            "Task title cannot be empty.",
        ));
    }
    validate_text_lengths(Some(title), description)?;

    // Config-provided prefixes are persisted into the display-id space, so
    // they must pass the same validation the domain documents (uppercase
    // ASCII, 1-10 chars) instead of entering unvalidated.
    if let Some(prefix) = prefix {
        if let Err(msg) = validate_task_prefix(prefix) {
            return Err(CarryCtxError::validation_error(format!(
                "Invalid task prefix '{prefix}': {msg}"
            )));
        }
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

    // Check incomplete strong dependencies. The gate uses the shared domain
    // predicate: cancelled prerequisites are settled, matching the
    // claim/transition SQL so a task cannot be Planned at birth yet
    // immediately claimable.
    let incomplete_strong: Vec<&TaskRecord> = prerequisites
        .iter()
        .filter(|p| !prerequisite_settled(p.status))
        .collect();

    if !incomplete_strong.is_empty() && status == Some(TaskStatus::Ready) {
        return Err(CarryCtxError::state_conflict(format!(
            "Cannot create task in 'ready' status with {} incomplete strong prerequisite(s).",
            incomplete_strong.len()
        )));
    }

    // Only planned/ready are valid creation statuses: minting a task directly
    // in an active or terminal state would bypass dependency gating entirely
    // (e.g. an already-completed task with open blockers).
    if let Some(requested) = status {
        if !matches!(requested, TaskStatus::Planned | TaskStatus::Ready) {
            return Err(CarryCtxError::validation_error(format!(
                "Cannot create a task in '{requested:?}' status. Initial status must be 'planned' or 'ready'; use the lifecycle transitions (claim, start, block, complete, cancel) instead."
            )));
        }
    }

    // Determine initial status
    let final_status =
        status.unwrap_or_else(|| initial_status(incomplete_strong.is_empty(), false));

    let display_seq = task_repo.allocate_display_id(project_id, prefix.unwrap_or("CTX"))?;
    let display_id = format_display_id(prefix.unwrap_or("CTX"), display_seq);

    let agent_repo = SqliteAgentRepository::new(conn);
    let team_repo = SqliteTeamRepository::new(conn);
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
    let resolved_team_id = match team_id {
        Some(reference) if !reference.trim().is_empty() => Some(
            team_repo
                .find_by_id(project_id, reference)?
                .or(team_repo.find_by_name(project_id, reference)?)
                .map(|team| team.id)
                .ok_or_else(|| {
                    CarryCtxError::resource_not_found(format!("Team '{reference}' not found."))
                })?,
        ),
        _ => None,
    };

    let task = task_repo.create(
        &NewTask {
            id: task_id.clone(),
            display_id: display_id.clone(),
            project_id: project_id.to_string(),
            title: title.trim().to_string(),
            description: description.map(|s| s.trim().to_string()),
            status: final_status,
            priority: priority.unwrap_or_default(),
            owner_agent_id: resolved_owner_id,
            parent_task_id: None,
            required_role: required_role
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            team_id: resolved_team_id,
        },
        &now,
    )?;

    event_repo.append(&NewEvent {
        id: new_id(),
        project_id: project_id.to_string(),
        event_type: "task.created".into(),
        actor_agent_id: resolved_actor_id.clone(),
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
            actor_agent_id: resolved_actor_id.clone(),
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

/// List tasks with optional filtering. `limit` overrides the repository
/// default cap; the CLI threads `[task] list_limit` (or `--limit`) through.
pub fn list_tasks(
    _project_id: &str,
    filter: &TaskFilter,
    limit: Option<u64>,
    uow: &UnitOfWork,
) -> Result<Vec<TaskRecord>, CarryCtxError> {
    let conn = uow.connection();
    let repo = SqliteTaskRepository::new(conn);
    match limit {
        Some(limit) => repo.list_capped(filter, limit),
        None => repo.list(filter),
    }
}

/// A single entry in a task's dependency summary: the related task's
/// identity plus its current status, so a caller doesn't need a second
/// round-trip just to see whether a prerequisite is still incomplete.
#[derive(serde::Serialize)]
pub struct DependencySummaryEntry {
    pub id: String,
    pub display_id: String,
    pub title: String,
    pub status: TaskStatus,
    pub kind: DependencyKind,
}

/// `show_task`'s response: the task record plus its full dependency graph
/// in both directions (what it depends on, and what depends on it), so a
/// caller can answer "what's blocking this" and "what does this block"
/// without a separate `graph edges` call into an unrelated ID space.
#[derive(serde::Serialize)]
pub struct TaskWithDependencies {
    #[serde(flatten)]
    pub task: TaskRecord,
    /// Tasks this task depends on (prerequisites).
    pub depends_on: Vec<DependencySummaryEntry>,
    /// Tasks that depend on this task (dependents).
    pub blocks: Vec<DependencySummaryEntry>,
}

/// Show a single task by display_id or id, including its dependency graph
/// in both directions.
pub fn show_task(
    project_id: &str,
    ref_: &str,
    uow: &UnitOfWork,
) -> Result<TaskWithDependencies, CarryCtxError> {
    let conn = uow.connection();
    let task_repo = SqliteTaskRepository::new(conn);
    let dep_repo = SqliteDependencyRepository::new(conn);

    let task = resolve_task(project_id, ref_, &task_repo)?;

    let outgoing = dep_repo.list_for_task(project_id, &task.id)?;
    let all_edges = dep_repo.list_all_for_project(project_id)?;
    let incoming: Vec<&DependencyEdge> = all_edges
        .iter()
        .filter(|e| e.prerequisite_id == task.id)
        .collect();

    let mut depends_on = Vec::with_capacity(outgoing.len());
    for edge in &outgoing {
        if let Some(prereq) = task_repo.find_by_id(project_id, &edge.prerequisite_id)? {
            depends_on.push(DependencySummaryEntry {
                id: prereq.id,
                display_id: prereq.display_id,
                title: prereq.title,
                status: prereq.status,
                kind: edge.kind,
            });
        }
    }

    let mut blocks = Vec::with_capacity(incoming.len());
    for edge in &incoming {
        if let Some(dependent) = task_repo.find_by_id(project_id, &edge.task_id)? {
            blocks.push(DependencySummaryEntry {
                id: dependent.id,
                display_id: dependent.display_id,
                title: dependent.title,
                status: dependent.status,
                kind: edge.kind,
            });
        }
    }

    Ok(TaskWithDependencies {
        task,
        depends_on,
        blocks,
    })
}

/// Edit a task's title, priority, description, or required role.
///
/// Mutability policy:
/// - Terminal tasks (completed/cancelled) are immutable — their record is the
///   audit trail, so retitling or re-prioritizing after completion is
///   rejected with a state conflict.
/// - Optional fields (`description`, `required_role`) can be explicitly
///   cleared by passing an empty string; previously they could never be
///   cleared once set.
pub fn edit_task(
    project_id: &str,
    ref_: &str,
    title: Option<&str>,
    priority: Option<TaskPriority>,
    description: Option<&str>,
    required_role: Option<&str>,
    actor_agent_id: Option<&str>,
    uow: &UnitOfWork,
) -> Result<TaskRecord, CarryCtxError> {
    if let Some(t) = title {
        if t.trim().is_empty() {
            return Err(CarryCtxError::validation_error(
                "Task title cannot be empty.",
            ));
        }
        validate_text_lengths(Some(t), None)?;
    }
    validate_text_lengths(None, description)?;

    let now = now();
    let conn = uow.connection();
    let task_repo = SqliteTaskRepository::new(conn);
    let agent_repo = SqliteAgentRepository::new(conn);
    let event_repo = SqliteEventRepository::new(conn);

    let actor_agent_id = canonical_actor_id(project_id, actor_agent_id, &agent_repo)?;

    let existing = resolve_task(project_id, ref_, &task_repo)?;

    // Terminal tasks are frozen: no title/priority/description/role edits.
    if existing.status.is_terminal() {
        return Err(CarryCtxError::state_conflict(format!(
            "Task '{}' is {:?} and can no longer be edited.",
            existing.display_id, existing.status
        )));
    }

    let before_title = existing.title.clone();
    let before_priority = existing.priority;
    let before_description = existing.description.clone();
    let before_required_role = existing.required_role.clone();

    // An explicit empty string clears an optional field; omitting the flag
    // (`None`) keeps the current value.
    fn apply_optional(provided: Option<&str>, current: &Option<String>) -> Option<String> {
        match provided {
            Some(value) => {
                let trimmed = value.trim();
                (!trimmed.is_empty()).then(|| trimmed.to_owned())
            }
            None => current.clone(),
        }
    }

    let final_title = title
        .map(|t| t.trim().to_string())
        .unwrap_or(existing.title.clone());
    let final_priority = priority.unwrap_or(existing.priority);
    let final_description = apply_optional(description, &existing.description);
    let final_required_role = apply_optional(required_role, &existing.required_role);

    let updated = task_repo.edit(
        &existing.id,
        project_id,
        &final_title,
        final_priority,
        final_description.as_deref(),
        final_required_role.as_deref(),
        &now,
    )?;

    event_repo.append(&NewEvent {
        id: new_id(),
        project_id: project_id.to_string(),
        event_type: "task.edited".into(),
        actor_agent_id,
        session_id: None,
        task_id: Some(existing.id.clone()),
        payload: serde_json::json!({
            "id": existing.id,
            "before": {
                "title": before_title,
                "priority": before_priority,
                "description": before_description,
                "required_role": before_required_role,
            },
            "after": {
                "title": updated.title,
                "priority": updated.priority,
                "description": updated.description,
                "required_role": updated.required_role,
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
    let agent = repo
        .find_by_name(project_id, agent_ref)?
        .or_else(|| repo.find_by_id(project_id, agent_ref).ok().flatten());
    match agent {
        Some(agent) => {
            // Deactivated agents must not act or be assigned work.
            if agent.status != crate::domain::agent::AgentStatus::Active {
                return Err(CarryCtxError::permission_scope(format!(
                    "Agent '{}' is deactivated and cannot act.",
                    agent.name
                )));
            }
            Ok(agent.id)
        }
        None => Err(CarryCtxError::resource_not_found(format!(
            "Agent '{agent_ref}' not found."
        ))),
    }
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

    // Compare-and-set claim: the guarded UPDATE arbitrates concurrent claims
    // at the storage layer (ready + unowned), so exactly one racer wins even
    // if the pre-checks above raced with another claimer.
    let updated = task_repo.update_status_if_ready_unowned(
        &existing.id,
        project_id,
        actor_agent_id.to_string(),
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
    let agent_repo = SqliteAgentRepository::new(conn);
    let event_repo = SqliteEventRepository::new(conn);

    // Resolve the actor once at entry so every audit event written by this
    // use case stores the canonical internal id instead of the raw name-or-id.
    let actor_agent_id = canonical_actor_id(project_id, actor_agent_id, &agent_repo)?;

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

    // A generic Claim transition must record ownership. Without an actor
    // there is nobody to assign the task to — an ownerless InProgress row is
    // exactly the trap this guard closes (the CLI routes claims through
    // `claim_task`, which always resolves its actor).
    let next_owner = if action == TransitionAction::Claim {
        match actor_agent_id.as_deref() {
            Some(actor) => Some(actor.to_string()),
            None => {
                return Err(CarryCtxError::validation_error(
                    "A claim transition requires an authenticated actor; pass --agent or use 'task claim'.",
                ));
            }
        }
    } else if clears_owner {
        None
    } else {
        existing.owner_agent_id.clone()
    };

    let updated =
        task_repo.update_status(&existing.id, project_id, new_status, next_owner, &now)?;

    event_repo.append(&NewEvent {
        id: new_id(),
        project_id: project_id.to_string(),
        event_type: action.past_tense().into(),
        actor_agent_id: actor_agent_id.clone(),
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
                        actor_agent_id: actor_agent_id.clone(),
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
    let agent_repo = SqliteAgentRepository::new(conn);
    let event_repo = SqliteEventRepository::new(conn);

    // Canonical actor for the audit event (see `canonical_actor_id`).
    let actor_agent_id = canonical_actor_id(project_id, actor_agent_id, &agent_repo)?;

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

    // If adding a strong dep to an incomplete task, downgrade status from ready
    // to planned. A settled (completed or cancelled) prerequisite does not
    // block, mirroring `prerequisite_settled` in the domain layer.
    let mut updated_task = task.clone();
    if kind == DependencyKind::Strong
        && !prerequisite_settled(prerequisite.status)
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
        actor_agent_id,
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
    let agent_repo = SqliteAgentRepository::new(conn);
    let event_repo = SqliteEventRepository::new(conn);

    // Canonical actor for the audit event (see `canonical_actor_id`).
    let actor_agent_id = canonical_actor_id(project_id, actor_agent_id, &agent_repo)?;

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
        actor_agent_id,
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
