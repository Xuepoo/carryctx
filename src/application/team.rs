use crate::adapter::sqlite_repos::{SqliteEventRepository, SqliteTeamRepository};
use crate::adapter::unit_of_work::UnitOfWork;
use crate::domain::team::{Team, TeamMember, TeamStatusProjection, validate_team_name};
use crate::error::CarryCtxError;
use crate::repository::{EventRepository, NewEvent, NewTeam, NewTeamMember, TeamRepository};

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}
fn new_id() -> String {
    ulid::Ulid::generate().to_string()
}

pub fn create_team(
    project_id: &str,
    name: &str,
    commander_agent_id: Option<&str>,
    actor_agent_id: Option<&str>,
    uow: &UnitOfWork,
) -> Result<Team, CarryCtxError> {
    validate_team_name(name)?;
    let timestamp = now();
    let repo = SqliteTeamRepository::new(uow.connection());
    let team = repo.create(
        &NewTeam {
            id: new_id(),
            project_id: project_id.into(),
            name: name.trim().into(),
            commander_agent_id: commander_agent_id.map(str::to_owned),
        },
        &timestamp,
    )?;
    SqliteEventRepository::new(uow.connection()).append(&NewEvent { id: new_id(), project_id: project_id.into(), event_type: "team.created".into(), actor_agent_id: actor_agent_id.map(str::to_owned), session_id: None, task_id: None, payload: serde_json::json!({"team_id": team.id, "name": team.name, "commander_agent_id": team.commander_agent_id}), occurred_at: timestamp.clone() })?;
    if let Some(commander) = commander_agent_id {
        SqliteEventRepository::new(uow.connection()).append(&NewEvent {
            id: new_id(),
            project_id: project_id.into(),
            event_type: "team.member_added".into(),
            actor_agent_id: actor_agent_id.map(str::to_owned),
            session_id: None,
            task_id: None,
            payload: serde_json::json!({"team_id": team.id, "agent_id": commander, "role": null}),
            occurred_at: timestamp,
        })?;
    }
    Ok(team)
}

pub fn add_member(
    project_id: &str,
    team_id: &str,
    agent_id: &str,
    role: Option<&str>,
    actor_agent_id: Option<&str>,
    uow: &UnitOfWork,
) -> Result<TeamMember, CarryCtxError> {
    let timestamp = now();
    let member = SqliteTeamRepository::new(uow.connection()).add_member(
        &NewTeamMember {
            project_id: project_id.into(),
            team_id: team_id.into(),
            agent_id: agent_id.into(),
            role: role.map(str::to_owned),
        },
        &timestamp,
    )?;
    SqliteEventRepository::new(uow.connection()).append(&NewEvent {
        id: new_id(),
        project_id: project_id.into(),
        event_type: "team.member_added".into(),
        actor_agent_id: actor_agent_id.map(str::to_owned),
        session_id: None,
        task_id: None,
        payload: serde_json::json!({"team_id": team_id, "agent_id": agent_id, "role": role}),
        occurred_at: timestamp,
    })?;
    Ok(member)
}

pub fn remove_member(
    project_id: &str,
    team_id: &str,
    agent_id: &str,
    actor_agent_id: Option<&str>,
    uow: &UnitOfWork,
) -> Result<(), CarryCtxError> {
    SqliteTeamRepository::new(uow.connection()).remove_member(project_id, team_id, agent_id)?;
    SqliteEventRepository::new(uow.connection()).append(&NewEvent {
        id: new_id(),
        project_id: project_id.into(),
        event_type: "team.member_removed".into(),
        actor_agent_id: actor_agent_id.map(str::to_owned),
        session_id: None,
        task_id: None,
        payload: serde_json::json!({"team_id": team_id, "agent_id": agent_id}),
        occurred_at: now(),
    })?;
    Ok(())
}

pub fn set_commander(
    project_id: &str,
    team_id: &str,
    agent_id: Option<&str>,
    actor_agent_id: Option<&str>,
    uow: &UnitOfWork,
) -> Result<Team, CarryCtxError> {
    let timestamp = now();
    let team = SqliteTeamRepository::new(uow.connection())
        .set_commander(project_id, team_id, agent_id, &timestamp)?;
    SqliteEventRepository::new(uow.connection()).append(&NewEvent {
        id: new_id(),
        project_id: project_id.into(),
        event_type: "team.commander_changed".into(),
        actor_agent_id: actor_agent_id.map(str::to_owned),
        session_id: None,
        task_id: None,
        payload: serde_json::json!({"team_id": team_id, "commander_agent_id": agent_id}),
        occurred_at: timestamp,
    })?;
    Ok(team)
}

pub fn set_task_team(
    project_id: &str,
    task_id: &str,
    team_id: Option<&str>,
    actor_agent_id: Option<&str>,
    uow: &UnitOfWork,
) -> Result<Option<String>, CarryCtxError> {
    let timestamp = now();
    let previous_team_id = SqliteTeamRepository::new(uow.connection())
        .set_task_team(project_id, task_id, team_id, &timestamp)?;
    SqliteEventRepository::new(uow.connection()).append(&NewEvent {
        id: new_id(),
        project_id: project_id.into(),
        event_type: "task.team_changed".into(),
        actor_agent_id: actor_agent_id.map(str::to_owned),
        session_id: None,
        task_id: Some(task_id.into()),
        payload: serde_json::json!({"team_id": team_id}),
        occurred_at: timestamp,
    })?;
    Ok(previous_team_id)
}

pub fn status(
    project_id: &str,
    team_id: &str,
    conn: &rusqlite::Connection,
) -> Result<TeamStatusProjection, CarryCtxError> {
    SqliteTeamRepository::new(conn).status(project_id, team_id)
}

pub fn list_status(
    project_id: &str,
    conn: &rusqlite::Connection,
) -> Result<Vec<TeamStatusProjection>, CarryCtxError> {
    SqliteTeamRepository::new(conn)
        .list(project_id)?
        .into_iter()
        .map(|team| status(project_id, &team.id, conn))
        .collect()
}

pub fn context(
    project_id: &str,
    team_id: &str,
    agent_id: Option<&str>,
    task_id: Option<&str>,
    session_id: Option<&str>,
    conn: &rusqlite::Connection,
) -> Result<crate::domain::team::TeamContextProjection, CarryCtxError> {
    use crate::repository::TeamRepository;
    SqliteTeamRepository::new(conn).context(project_id, team_id, agent_id, task_id, session_id)
}

pub fn resolve_context_team(
    project_id: &str,
    task_id: Option<&str>,
    agent_id: Option<&str>,
    conn: &rusqlite::Connection,
) -> Result<String, CarryCtxError> {
    use crate::adapter::sqlite_repos::SqliteTaskRepository;
    use crate::repository::{TaskRepository, TeamRepository};

    if let Some(task_id) = task_id {
        let task = SqliteTaskRepository::new(conn)
            .find_by_id(project_id, task_id)?
            .ok_or_else(|| CarryCtxError::resource_not_found("Task has no associated team."))?;
        return task.team_id.ok_or_else(|| {
            CarryCtxError::resource_not_found(format!("Task '{}' has no associated team.", task_id))
        });
    }
    let teams = SqliteTeamRepository::new(conn).list(project_id)?;
    if let Some(agent_id) = agent_id {
        for team in &teams {
            if SqliteTeamRepository::new(conn)
                .status(project_id, &team.id)?
                .members
                .iter()
                .any(|member| member.agent_id == agent_id)
            {
                return Ok(team.id.clone());
            }
        }
        return Err(CarryCtxError::resource_not_found(format!(
            "Agent '{}' is not a member of a team.",
            agent_id
        )));
    }
    if teams.len() == 1 {
        Ok(teams[0].id.clone())
    } else {
        Err(CarryCtxError::resource_not_found(
            "A single Team reference is required.",
        ))
    }
}
