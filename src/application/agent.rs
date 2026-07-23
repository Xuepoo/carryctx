use crate::adapter::sqlite_repos::{SqliteAgentRepository, SqliteEventRepository};
use crate::adapter::unit_of_work::UnitOfWork;
use crate::domain::agent::{Agent, AgentStatus, validate_agent_name};
use crate::error::CarryCtxError;
use crate::repository::agent::{AgentFilter, AgentRepository, NewAgent};
use crate::repository::event::{EventRepository, NewEvent};

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn new_id() -> String {
    ulid::Ulid::generate().to_string()
}

fn resolve_agent(
    project_id: &str,
    ref_: &str,
    repo: &SqliteAgentRepository,
) -> Result<Agent, CarryCtxError> {
    let by_name = repo.find_by_name(project_id, ref_)?;
    if let Some(agent) = by_name {
        return Ok(agent);
    }
    repo.find_by_id(project_id, ref_)?
        .ok_or_else(|| CarryCtxError::resource_not_found(format!("Agent '{ref_}' not found.")))
}

pub fn register_agent(
    project_id: &str,
    name: &str,
    provider: Option<&str>,
    metadata: serde_json::Value,
    uow: &UnitOfWork,
) -> Result<Agent, CarryCtxError> {
    validate_agent_name(name)?;
    let now = now();
    let agent_id = new_id();
    let provider = provider.unwrap_or("custom");

    let conn = uow.connection();
    let agent_repo = SqliteAgentRepository::new(conn);
    let event_repo = SqliteEventRepository::new(conn);

    let agent = agent_repo.register(
        &NewAgent {
            id: agent_id.clone(),
            project_id: project_id.to_string(),
            name: name.to_string(),
            provider: provider.to_string(),
            role: None,
            metadata: if metadata.is_object() {
                let mut m = serde_json::json!({ "provider": provider });
                if let Some(obj) = metadata.as_object() {
                    for (k, v) in obj {
                        m[k] = v.clone();
                    }
                }
                m
            } else {
                serde_json::json!({ "provider": provider })
            },
        },
        &now,
    )?;

    event_repo.append(&NewEvent {
        id: new_id(),
        project_id: project_id.to_string(),
        event_type: "agent.registered".into(),
        actor_agent_id: Some(agent_id.clone()),
        session_id: None,
        task_id: None,
        payload: serde_json::json!({
            "id": agent.id,
            "name": agent.name,
            "metadata": agent.metadata,
        }),
        occurred_at: now,
    })?;

    Ok(agent)
}

pub fn list_agents(
    _project_id: &str,
    filter: &AgentFilter,
    uow: &UnitOfWork,
) -> Result<Vec<Agent>, CarryCtxError> {
    let conn = uow.connection();
    let repo = SqliteAgentRepository::new(conn);
    repo.list(filter)
}

pub fn show_agent(project_id: &str, ref_: &str, uow: &UnitOfWork) -> Result<Agent, CarryCtxError> {
    let conn = uow.connection();
    let repo = SqliteAgentRepository::new(conn);
    resolve_agent(project_id, ref_, &repo)
}

pub fn rename_agent(
    project_id: &str,
    target_ref: &str,
    new_name: &str,
    uow: &UnitOfWork,
) -> Result<Agent, CarryCtxError> {
    validate_agent_name(new_name)?;
    let now = now();

    let conn = uow.connection();
    let agent_repo = SqliteAgentRepository::new(conn);
    let event_repo = SqliteEventRepository::new(conn);

    let existing = resolve_agent(project_id, target_ref, &agent_repo)?;

    let updated = agent_repo.rename(&existing.id, project_id, new_name, &now)?;

    event_repo.append(&NewEvent {
        id: new_id(),
        project_id: project_id.to_string(),
        event_type: "agent.renamed".into(),
        actor_agent_id: Some(existing.id.clone()),
        session_id: None,
        task_id: None,
        payload: serde_json::json!({
            "id": existing.id,
            "beforeName": existing.name,
            "afterName": new_name,
        }),
        occurred_at: now,
    })?;

    Ok(updated)
}

pub fn deactivate_agent(
    project_id: &str,
    target_ref: &str,
    uow: &UnitOfWork,
) -> Result<Agent, CarryCtxError> {
    let now = now();

    let conn = uow.connection();
    let agent_repo = SqliteAgentRepository::new(conn);
    let event_repo = SqliteEventRepository::new(conn);

    let existing = resolve_agent(project_id, target_ref, &agent_repo)?;

    if existing.status == AgentStatus::Deactivated {
        return Err(CarryCtxError::state_conflict(format!(
            "Agent '{}' is already deactivated.",
            existing.name
        )));
    }

    let updated = agent_repo.deactivate(&existing.id, project_id, &now)?;

    event_repo.append(&NewEvent {
        id: new_id(),
        project_id: project_id.to_string(),
        event_type: "agent.deactivated".into(),
        actor_agent_id: Some(existing.id.clone()),
        session_id: None,
        task_id: None,
        payload: serde_json::json!({
            "id": existing.id,
            "name": existing.name,
        }),
        occurred_at: now,
    })?;

    Ok(updated)
}
