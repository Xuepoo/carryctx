use crate::domain::session::{SessionState, evaluate_session_transition};
use crate::error::CarryCtxError;
use crate::repository::{EventRepository, NewEvent, NewSession, SessionRecord, SessionRepository};

pub struct StartSessionInput {
    pub project_id: String,
    pub agent_id: String,
    pub task_id: Option<String>,
    pub worktree_id: Option<String>,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub cwd: Option<String>,
    pub provider: Option<String>,
}

pub fn start_session(
    session_repo: &dyn SessionRepository,
    event_repo: &dyn EventRepository,
    input: &StartSessionInput,
    now: &str,
) -> Result<SessionRecord, CarryCtxError> {
    let active_sessions = session_repo.find_active(
        &input.project_id,
        &input.agent_id,
        input.worktree_id.as_deref(),
    )?;

    for s in &active_sessions {
        evaluate_session_transition(s.state, SessionState::Ended)?;
        session_repo.update_state(
            &s.id,
            &s.project_id,
            SessionState::Ended,
            now,
            Some("Auto-ended by new session"),
        )?;
        event_repo.append(&NewEvent {
            id: ulid::Ulid::generate().to_string(),
            project_id: input.project_id.clone(),
            event_type: "session.ended".into(),
            actor_agent_id: Some(input.agent_id.clone()),
            session_id: Some(s.id.clone()),
            task_id: s.task_id.clone(),
            payload: serde_json::json!({
                "reason": "superseded",
                "superseded_by": ulid::Ulid::generate().to_string()
            }),
            occurred_at: now.to_string(),
        })?;
    }

    let session_id = ulid::Ulid::generate().to_string();
    let session = session_repo.create(
        &NewSession {
            id: session_id,
            project_id: input.project_id.clone(),
            agent_id: input.agent_id.clone(),
            task_id: input.task_id.clone(),
            worktree_id: input.worktree_id.clone(),
            branch: input.branch.clone(),
            head: input.head.clone(),
            cwd: input.cwd.clone(),
            provider: input.provider.clone(),
        },
        now,
    )?;

    event_repo.append(&NewEvent {
        id: ulid::Ulid::generate().to_string(),
        project_id: input.project_id.clone(),
        event_type: "session.started".into(),
        actor_agent_id: Some(input.agent_id.clone()),
        session_id: Some(session.id.clone()),
        task_id: session.task_id.clone(),
        payload: serde_json::json!({
            "session_id": session.id,
            "agent_id": session.agent_id,
            "task_id": session.task_id,
            "worktree_id": session.worktree_id,
            "provider": session.provider,
        }),
        occurred_at: now.to_string(),
    })?;

    Ok(session)
}

pub struct ResumeSessionInput {
    pub project_id: String,
    pub session_id: String,
    pub agent_id: String,
}

pub fn resume_session(
    session_repo: &dyn SessionRepository,
    event_repo: &dyn EventRepository,
    input: &ResumeSessionInput,
    now: &str,
) -> Result<SessionRecord, CarryCtxError> {
    let session = session_repo
        .find_by_id(&input.project_id, &input.session_id)?
        .ok_or_else(|| {
            CarryCtxError::resource_not_found(format!("Session '{}' not found", input.session_id))
        })?;

    if session.state != SessionState::Active && session.state != SessionState::Paused {
        return Err(CarryCtxError::invalid_arguments(format!(
            "Cannot resume session in state '{:?}'",
            session.state
        )));
    }

    evaluate_session_transition(session.state, SessionState::Active)?;

    // Update state from Paused/Active to Active
    session_repo.update_state(&session.id, &session.project_id, SessionState::Active, now, None)?;

    event_repo.append(&NewEvent {
        id: ulid::Ulid::generate().to_string(),
        project_id: input.project_id.clone(),
        event_type: "session.resumed".into(),
        actor_agent_id: Some(input.agent_id.clone()),
        session_id: Some(session.id.clone()),
        task_id: session.task_id.clone(),
        payload: serde_json::json!({
            "session_id": session.id,
            "previous_state": session.state,
        }),
        occurred_at: now.to_string(),
    })?;

    let updated = session_repo
        .find_by_id(&input.project_id, &input.session_id)?
        .ok_or_else(|| {
            CarryCtxError::resource_not_found(format!(
                "Session '{}' disappeared after update",
                input.session_id
            ))
        })?;

    Ok(updated)
}

pub struct EndSessionInput {
    pub project_id: String,
    pub session_id: String,
    pub agent_id: String,
    pub summary: Option<String>,
}

pub fn end_session(
    session_repo: &dyn SessionRepository,
    event_repo: &dyn EventRepository,
    input: &EndSessionInput,
    now: &str,
) -> Result<SessionRecord, CarryCtxError> {
    let session = session_repo
        .find_by_id(&input.project_id, &input.session_id)?
        .ok_or_else(|| {
            CarryCtxError::resource_not_found(format!("Session '{}' not found", input.session_id))
        })?;

    evaluate_session_transition(session.state, SessionState::Ended)?;

    let updated = session_repo.update_state(
        &session.id,
        &session.project_id,
        SessionState::Ended,
        now,
        input.summary.as_deref(),
    )?;

    event_repo.append(&NewEvent {
        id: ulid::Ulid::generate().to_string(),
        project_id: input.project_id.clone(),
        event_type: "session.ended".into(),
        actor_agent_id: Some(input.agent_id.clone()),
        session_id: Some(session.id.clone()),
        task_id: session.task_id.clone(),
        payload: serde_json::json!({
            "session_id": session.id,
            "previous_state": session.state,
            "summary": input.summary,
        }),
        occurred_at: now.to_string(),
    })?;

    Ok(updated)
}

pub struct PauseSessionInput {
    pub project_id: String,
    pub session_id: String,
    pub agent_id: String,
}

pub fn pause_session(
    session_repo: &dyn SessionRepository,
    event_repo: &dyn EventRepository,
    input: &PauseSessionInput,
    now: &str,
) -> Result<SessionRecord, CarryCtxError> {
    let session = session_repo
        .find_by_id(&input.project_id, &input.session_id)?
        .ok_or_else(|| {
            CarryCtxError::resource_not_found(format!("Session '{}' not found", input.session_id))
        })?;

    evaluate_session_transition(session.state, SessionState::Paused)?;

    let updated = session_repo.update_state(
        &session.id,
        &session.project_id,
        SessionState::Paused,
        now,
        None,
    )?;

    event_repo.append(&NewEvent {
        id: ulid::Ulid::generate().to_string(),
        project_id: input.project_id.clone(),
        event_type: "session.paused".into(),
        actor_agent_id: Some(input.agent_id.clone()),
        session_id: Some(session.id.clone()),
        task_id: session.task_id.clone(),
        payload: serde_json::json!({
            "session_id": session.id,
            "previous_state": session.state,
        }),
        occurred_at: now.to_string(),
    })?;

    Ok(updated)
}

pub fn list_sessions(
    session_repo: &dyn SessionRepository,
    project_id: &str,
) -> Result<Vec<SessionRecord>, CarryCtxError> {
    session_repo.list(project_id)
}

pub fn show_session(
    session_repo: &dyn SessionRepository,
    project_id: &str,
    session_id: &str,
) -> Result<SessionRecord, CarryCtxError> {
    session_repo
        .find_by_id(project_id, session_id)?
        .ok_or_else(|| {
            CarryCtxError::resource_not_found(format!("Session '{}' not found", session_id))
        })
}

pub fn mark_stale_sessions(
    session_repo: &dyn SessionRepository,
    project_id: &str,
    stale_before: &str,
    now: &str,
) -> Result<u64, CarryCtxError> {
    let count = session_repo.mark_overdue_stale(project_id, stale_before, now)?;
    Ok(count)
}
