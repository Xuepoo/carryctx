use crate::domain::team::{Team, TeamContextProjection, TeamMember, TeamStatusProjection};
use crate::error::CarryCtxError;

pub struct NewTeam {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub commander_agent_id: Option<String>,
}

pub struct NewTeamMember {
    pub project_id: String,
    pub team_id: String,
    pub agent_id: String,
    pub role: Option<String>,
}

pub trait TeamRepository {
    fn create(&self, team: &NewTeam, now: &str) -> Result<Team, CarryCtxError>;
    fn find_by_id(&self, project_id: &str, id: &str) -> Result<Option<Team>, CarryCtxError>;
    fn find_by_name(&self, project_id: &str, name: &str) -> Result<Option<Team>, CarryCtxError>;
    fn list(&self, project_id: &str) -> Result<Vec<Team>, CarryCtxError>;
    fn status(
        &self,
        project_id: &str,
        team_id: &str,
    ) -> Result<TeamStatusProjection, CarryCtxError>;
    fn add_member(&self, member: &NewTeamMember, now: &str) -> Result<TeamMember, CarryCtxError>;
    fn remove_member(
        &self,
        project_id: &str,
        team_id: &str,
        agent_id: &str,
    ) -> Result<(), CarryCtxError>;
    fn set_commander(
        &self,
        project_id: &str,
        team_id: &str,
        agent_id: Option<&str>,
        now: &str,
    ) -> Result<Team, CarryCtxError>;
    fn set_task_team(
        &self,
        project_id: &str,
        task_id: &str,
        team_id: Option<&str>,
        now: &str,
    ) -> Result<Option<String>, CarryCtxError>;
    fn context(
        &self,
        project_id: &str,
        team_id: &str,
        agent_id: Option<&str>,
        task_id: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<TeamContextProjection, CarryCtxError>;
}
