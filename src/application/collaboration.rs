use crate::adapter::sqlite_repos::{
    SqliteDecisionRepository, SqliteEventRepository, SqliteHandoffRepository,
    SqliteScopeRepository, SqliteTaskRepository,
};
use crate::adapter::unit_of_work::UnitOfWork;
use crate::domain::collaboration::{Decision, Handoff, HandoffStatus, ScopeOverlap, TaskScope};
use crate::domain::ids::format_display_id;
use crate::error::CarryCtxError;
use crate::repository::collaboration::{DecisionRepository, HandoffRepository, ScopeRepository};
use crate::repository::event::{EventRepository, NewEvent};
use crate::repository::task::TaskRepository;

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
) -> Result<crate::repository::task::TaskRecord, CarryCtxError> {
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

// ── Scope management ────────────────────────────────────────────────────

pub fn add_scope(
    project_id: &str,
    task_ref: &str,
    pattern: &str,
    uow: &UnitOfWork,
) -> Result<TaskScope, CarryCtxError> {
    if pattern.trim().is_empty() {
        return Err(CarryCtxError::validation_error(
            "Scope pattern cannot be empty.",
        ));
    }

    let now = now();
    let conn = uow.connection();
    let task_repo = SqliteTaskRepository::new(conn);
    let scope_repo = SqliteScopeRepository::new(conn);
    let event_repo = SqliteEventRepository::new(conn);

    let task = resolve_task(project_id, task_ref, &task_repo)?;

    if task.status.is_terminal() {
        return Err(CarryCtxError::invalid_arguments(format!(
            "Cannot add scope to terminal task '{}'.",
            task.display_id
        )));
    }

    let scope_id = new_id();
    scope_repo.add(project_id, &task.id, pattern, &now)?;

    let scope = TaskScope {
        id: scope_id,
        task_id: task.id.clone(),
        pattern: pattern.to_string(),
        created_at: now.clone(),
    };

    event_repo.append(&NewEvent {
        id: new_id(),
        project_id: project_id.to_string(),
        event_type: "scope.added".into(),
        actor_agent_id: None,
        session_id: None,
        task_id: Some(task.id.clone()),
        payload: serde_json::json!({
            "scopeId": scope.id,
            "taskId": task.id,
            "taskDisplayId": task.display_id,
            "pattern": scope.pattern,
        }),
        occurred_at: now,
    })?;

    Ok(scope)
}

pub fn remove_scope(
    project_id: &str,
    task_ref: &str,
    pattern: &str,
    uow: &UnitOfWork,
) -> Result<(), CarryCtxError> {
    let now = now();
    let conn = uow.connection();
    let task_repo = SqliteTaskRepository::new(conn);
    let scope_repo = SqliteScopeRepository::new(conn);
    let event_repo = SqliteEventRepository::new(conn);

    let task = resolve_task(project_id, task_ref, &task_repo)?;

    scope_repo.remove(project_id, &task.id, pattern)?;

    event_repo.append(&NewEvent {
        id: new_id(),
        project_id: project_id.to_string(),
        event_type: "scope.removed".into(),
        actor_agent_id: None,
        session_id: None,
        task_id: Some(task.id.clone()),
        payload: serde_json::json!({
            "taskId": task.id,
            "taskDisplayId": task.display_id,
            "pattern": pattern,
        }),
        occurred_at: now,
    })?;

    Ok(())
}

pub fn list_scopes(
    project_id: &str,
    task_ref: &str,
    uow: &UnitOfWork,
) -> Result<Vec<TaskScope>, CarryCtxError> {
    let conn = uow.connection();
    let task_repo = SqliteTaskRepository::new(conn);
    let scope_repo = SqliteScopeRepository::new(conn);

    let task = resolve_task(project_id, task_ref, &task_repo)?;
    scope_repo.list_for_task(project_id, &task.id)
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ScopeConflict {
    pub pattern_a: String,
    pub task_id_a: String,
    pub task_display_id_a: String,
    pub pattern_b: String,
    pub task_id_b: String,
    pub task_display_id_b: String,
    pub overlap: ScopeOverlap,
}

pub fn detect_conflicts(
    project_id: &str,
    uow: &UnitOfWork,
) -> Result<Vec<ScopeConflict>, CarryCtxError> {
    let conn = uow.connection();
    let scope_repo = SqliteScopeRepository::new(conn);
    let task_repo = SqliteTaskRepository::new(conn);

    let active_scopes = scope_repo.list_active_scopes(project_id)?;
    let mut conflicts = Vec::new();

    for i in 0..active_scopes.len() {
        for j in (i + 1)..active_scopes.len() {
            let a = &active_scopes[i];
            let b = &active_scopes[j];

            if a.task_id == b.task_id {
                continue;
            }

            let overlap = classify_overlap(&a.pattern, &b.pattern);
            if overlap == ScopeOverlap::None {
                continue;
            }

            let task_a = task_repo.find_by_id(project_id, &a.task_id)?;
            let task_b = task_repo.find_by_id(project_id, &b.task_id)?;

            conflicts.push(ScopeConflict {
                pattern_a: a.pattern.clone(),
                task_id_a: a.task_id.clone(),
                task_display_id_a: task_a
                    .as_ref()
                    .map(|t| t.display_id.clone())
                    .unwrap_or_default(),
                pattern_b: b.pattern.clone(),
                task_id_b: b.task_id.clone(),
                task_display_id_b: task_b
                    .as_ref()
                    .map(|t| t.display_id.clone())
                    .unwrap_or_default(),
                overlap,
            });
        }
    }

    Ok(conflicts)
}

fn classify_overlap(a: &str, b: &str) -> ScopeOverlap {
    use globset::{Glob, GlobSetBuilder};

    let a_glob = match Glob::new(a) {
        Ok(g) => g,
        Err(_) => return ScopeOverlap::Possible,
    };
    let b_glob = match Glob::new(b) {
        Ok(g) => g,
        Err(_) => return ScopeOverlap::Possible,
    };

    let mut builder_a = GlobSetBuilder::new();
    builder_a.add(a_glob);
    let set_a = match builder_a.build() {
        Ok(s) => s,
        Err(_) => return ScopeOverlap::Possible,
    };

    let mut builder_b = GlobSetBuilder::new();
    builder_b.add(b_glob);
    let set_b = match builder_b.build() {
        Ok(s) => s,
        Err(_) => return ScopeOverlap::Possible,
    };

    // If one glob explicitly matches the other's pattern string, it's definite
    if set_a.is_match(b) || set_b.is_match(a) {
        return ScopeOverlap::Definite;
    }

    // Check for common prefixes that suggest possible overlap
    let a_parts: Vec<&str> = a.split('/').collect();
    let b_parts: Vec<&str> = b.split('/').collect();
    let min_len = a_parts.len().min(b_parts.len());

    for k in 1..=min_len {
        let a_prefix = a_parts[..k].join("/");
        let b_prefix = b_parts[..k].join("/");
        if a_prefix == b_prefix {
            return ScopeOverlap::Possible;
        }
    }

    ScopeOverlap::None
}

// ── Decision management ─────────────────────────────────────────────────

#[derive(Debug)]
pub struct CreateDecisionInput {
    pub task_id: String,
    pub title: String,
    pub context: Option<String>,
    pub decision: Option<String>,
    pub consequences: Option<String>,
    pub related_tasks: Vec<String>,
    pub related_paths: Vec<String>,
    pub created_by_agent: String,
    pub created_by_session: Option<String>,
}

pub fn create_decision(
    project_id: &str,
    input: &CreateDecisionInput,
    uow: &UnitOfWork,
) -> Result<Decision, CarryCtxError> {
    if input.title.trim().is_empty() {
        return Err(CarryCtxError::validation_error(
            "Decision title cannot be empty.",
        ));
    }

    let now = now();
    let conn = uow.connection();
    let decision_repo = SqliteDecisionRepository::new(conn);
    let event_repo = SqliteEventRepository::new(conn);

    let decision_id = new_id();
    let display_seq = allocate_decision_display_id(project_id, conn)?;
    let display_id = format_display_id("DEC", display_seq);

    let decision = Decision {
        id: decision_id.clone(),
        display_id,
        project_id: project_id.to_string(),
        task_id: input.task_id.clone(),
        title: input.title.trim().to_string(),
        context: input.context.clone(),
        decision: input.decision.clone(),
        consequences: input.consequences.clone(),
        related_tasks: input.related_tasks.clone(),
        related_paths: input.related_paths.clone(),
        created_by_agent: input.created_by_agent.clone(),
        created_by_session: input.created_by_session.clone(),
        superseded_by: None,
        created_at: now.clone(),
        updated_at: now.clone(),
    };

    let saved = decision_repo.create(&decision)?;

    event_repo.append(&NewEvent {
        id: new_id(),
        project_id: project_id.to_string(),
        event_type: "decision.created".into(),
        actor_agent_id: Some(input.created_by_agent.clone()),
        session_id: input.created_by_session.clone(),
        task_id: None,
        payload: serde_json::json!({
            "id": saved.id,
            "displayId": saved.display_id,
            "title": saved.title,
        }),
        occurred_at: now,
    })?;

    Ok(saved)
}

fn allocate_decision_display_id(
    project_id: &str,
    conn: &rusqlite::Connection,
) -> Result<u32, CarryCtxError> {
    let kind = "display_id_decision".to_string();
    let affected = conn
        .execute(
            "INSERT INTO sequences (project_id, kind, next_value) VALUES (?1, ?2, 2)
             ON CONFLICT(project_id, kind) DO UPDATE SET next_value = next_value + 1",
            rusqlite::params![project_id, kind],
        )
        .map_err(|e| CarryCtxError::database_error(format!("SQLite: {e}")))?;
    if affected > 0 {
        let val: i64 = conn
            .query_row(
                "SELECT next_value - 1 FROM sequences WHERE project_id = ?1 AND kind = ?2",
                rusqlite::params![project_id, kind],
                |row| row.get(0),
            )
            .map_err(|e| CarryCtxError::database_error(format!("SQLite: {e}")))?;
        Ok(val as u32)
    } else {
        Err(CarryCtxError::database_error(
            "Failed to allocate decision display id",
        ))
    }
}

pub fn list_decisions(
    project_id: &str,
    query: Option<&str>,
    uow: &UnitOfWork,
) -> Result<Vec<Decision>, CarryCtxError> {
    let conn = uow.connection();
    let repo = SqliteDecisionRepository::new(conn);

    match query {
        Some(q) if !q.is_empty() => repo.search(project_id, q),
        _ => repo.list(project_id),
    }
}

pub fn show_decision(
    project_id: &str,
    decision_id: &str,
    uow: &UnitOfWork,
) -> Result<Decision, CarryCtxError> {
    let conn = uow.connection();
    let repo = SqliteDecisionRepository::new(conn);

    repo.find_by_id(project_id, decision_id)?.ok_or_else(|| {
        CarryCtxError::resource_not_found(format!("Decision '{decision_id}' not found."))
    })
}

pub fn supersede_decision(
    project_id: &str,
    decision_id: &str,
    superseded_by: &str,
    actor_agent_id: &str,
    uow: &UnitOfWork,
) -> Result<(), CarryCtxError> {
    let now = now();
    let conn = uow.connection();
    let repo = SqliteDecisionRepository::new(conn);
    let event_repo = SqliteEventRepository::new(conn);

    let existing = repo.find_by_id(project_id, decision_id)?.ok_or_else(|| {
        CarryCtxError::resource_not_found(format!("Decision '{decision_id}' not found."))
    })?;

    let superseding = repo.find_by_id(project_id, superseded_by)?.ok_or_else(|| {
        CarryCtxError::resource_not_found(format!(
            "Superseding decision '{superseded_by}' not found."
        ))
    })?;

    repo.supersede(decision_id, project_id, superseded_by, &now)?;

    event_repo.append(&NewEvent {
        id: new_id(),
        project_id: project_id.to_string(),
        event_type: "decision.superseded".into(),
        actor_agent_id: Some(actor_agent_id.to_string()),
        session_id: None,
        task_id: None,
        payload: serde_json::json!({
            "decisionId": existing.id,
            "supersededBy": superseding.id,
            "supersededByDisplayId": superseding.display_id,
        }),
        occurred_at: now,
    })?;

    Ok(())
}

// ── Handoff management ──────────────────────────────────────────────────

#[derive(Debug)]
pub struct CreateHandoffInput {
    pub task_id: String,
    pub source_agent_id: String,
    pub source_session_id: Option<String>,
    pub target_agent_id: Option<String>,
    pub summary: Option<String>,
    pub completed_work: Vec<String>,
    pub remaining_work: Vec<String>,
    pub blockers: Vec<String>,
    pub risks: Vec<String>,
    pub next_steps: Vec<String>,
    pub changed_files: Vec<String>,
    pub head: Option<String>,
    pub branch: Option<String>,
}

pub fn create_handoff(
    project_id: &str,
    input: &CreateHandoffInput,
    uow: &UnitOfWork,
) -> Result<Handoff, CarryCtxError> {
    let now = now();
    let conn = uow.connection();
    let handoff_repo = SqliteHandoffRepository::new(conn);
    let event_repo = SqliteEventRepository::new(conn);
    let task_repo = SqliteTaskRepository::new(conn);

    let task = task_repo
        .find_by_id(project_id, &input.task_id)?
        .or_else(|| {
            task_repo
                .find_by_display_id(project_id, &input.task_id)
                .ok()
                .flatten()
        })
        .ok_or_else(|| {
            CarryCtxError::resource_not_found(format!("Task '{}' not found.", input.task_id))
        })?;

    let display_seq = allocate_handoff_display_id(project_id, conn)?;
    let display_id = format_display_id("HF", display_seq);

    let handoff = Handoff {
        id: new_id(),
        display_id,
        project_id: project_id.to_string(),
        task_id: task.id.clone(),
        source_agent_id: input.source_agent_id.clone(),
        source_session_id: input.source_session_id.clone(),
        target_agent_id: input.target_agent_id.clone(),
        summary: input.summary.clone(),
        completed_work: input.completed_work.clone(),
        remaining_work: input.remaining_work.clone(),
        blockers: input.blockers.clone(),
        risks: input.risks.clone(),
        next_steps: input.next_steps.clone(),
        changed_files: input.changed_files.clone(),
        head: input.head.clone(),
        branch: input.branch.clone(),
        status: HandoffStatus::Open,
        created_at: now.clone(),
        updated_at: now.clone(),
    };

    let saved = handoff_repo.create(&handoff)?;

    event_repo.append(&NewEvent {
        id: new_id(),
        project_id: project_id.to_string(),
        event_type: "handoff.created".into(),
        actor_agent_id: Some(input.source_agent_id.clone()),
        session_id: input.source_session_id.clone(),
        task_id: Some(task.id.clone()),
        payload: serde_json::json!({
            "id": saved.id,
            "displayId": saved.display_id,
            "taskId": saved.task_id,
            "sourceAgentId": saved.source_agent_id,
            "targetAgentId": saved.target_agent_id,
        }),
        occurred_at: now,
    })?;

    Ok(saved)
}

fn allocate_handoff_display_id(
    project_id: &str,
    conn: &rusqlite::Connection,
) -> Result<u32, CarryCtxError> {
    let kind = "display_id_handoff".to_string();
    let affected = conn
        .execute(
            "INSERT INTO sequences (project_id, kind, next_value) VALUES (?1, ?2, 2)
             ON CONFLICT(project_id, kind) DO UPDATE SET next_value = next_value + 1",
            rusqlite::params![project_id, kind],
        )
        .map_err(|e| CarryCtxError::database_error(format!("SQLite: {e}")))?;
    if affected > 0 {
        let val: i64 = conn
            .query_row(
                "SELECT next_value - 1 FROM sequences WHERE project_id = ?1 AND kind = ?2",
                rusqlite::params![project_id, kind],
                |row| row.get(0),
            )
            .map_err(|e| CarryCtxError::database_error(format!("SQLite: {e}")))?;
        Ok(val as u32)
    } else {
        Err(CarryCtxError::database_error(
            "Failed to allocate handoff display id",
        ))
    }
}

pub fn list_handoffs(project_id: &str, uow: &UnitOfWork) -> Result<Vec<Handoff>, CarryCtxError> {
    let conn = uow.connection();
    let repo = SqliteHandoffRepository::new(conn);
    repo.list(project_id)
}

pub fn show_handoff(
    project_id: &str,
    handoff_id: &str,
    uow: &UnitOfWork,
) -> Result<Handoff, CarryCtxError> {
    let conn = uow.connection();
    let repo = SqliteHandoffRepository::new(conn);
    repo.find_by_id(project_id, handoff_id)?.ok_or_else(|| {
        CarryCtxError::resource_not_found(format!("Handoff '{handoff_id}' not found."))
    })
}

fn transition_handoff(
    project_id: &str,
    handoff_id: &str,
    target_status: HandoffStatus,
    actor_agent_id: &str,
    event_type: &str,
    uow: &UnitOfWork,
) -> Result<Handoff, CarryCtxError> {
    let now = now();
    let conn = uow.connection();
    let repo = SqliteHandoffRepository::new(conn);
    let event_repo = SqliteEventRepository::new(conn);

    let handoff = repo.find_by_id(project_id, handoff_id)?.ok_or_else(|| {
        CarryCtxError::resource_not_found(format!("Handoff '{handoff_id}' not found."))
    })?;

    repo.update_status(handoff_id, project_id, target_status, &now)?;

    event_repo.append(&NewEvent {
        id: new_id(),
        project_id: project_id.to_string(),
        event_type: event_type.into(),
        actor_agent_id: Some(actor_agent_id.to_string()),
        session_id: None,
        task_id: Some(handoff.task_id.clone()),
        payload: serde_json::json!({
            "handoffId": handoff.id,
            "displayId": handoff.display_id,
            "previousStatus": handoff.status,
            "newStatus": target_status,
        }),
        occurred_at: now,
    })?;

    let updated = repo.find_by_id(project_id, handoff_id)?.ok_or_else(|| {
        CarryCtxError::resource_not_found(format!("Handoff '{handoff_id}' not found."))
    })?;

    Ok(updated)
}

pub fn accept_handoff(
    project_id: &str,
    handoff_id: &str,
    actor_agent_id: &str,
    uow: &UnitOfWork,
) -> Result<Handoff, CarryCtxError> {
    transition_handoff(
        project_id,
        handoff_id,
        HandoffStatus::Accepted,
        actor_agent_id,
        "handoff.accepted",
        uow,
    )
}

pub fn reject_handoff(
    project_id: &str,
    handoff_id: &str,
    actor_agent_id: &str,
    uow: &UnitOfWork,
) -> Result<Handoff, CarryCtxError> {
    transition_handoff(
        project_id,
        handoff_id,
        HandoffStatus::Rejected,
        actor_agent_id,
        "handoff.rejected",
        uow,
    )
}

pub fn close_handoff(
    project_id: &str,
    handoff_id: &str,
    actor_agent_id: &str,
    uow: &UnitOfWork,
) -> Result<Handoff, CarryCtxError> {
    transition_handoff(
        project_id,
        handoff_id,
        HandoffStatus::Closed,
        actor_agent_id,
        "handoff.closed",
        uow,
    )
}
