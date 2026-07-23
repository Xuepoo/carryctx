use crate::domain::agent::Agent;

pub struct NewAgent {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub provider: String,
    pub role: Option<String>,
    pub metadata: serde_json::Value,
}

pub struct AgentFilter {
    pub project_id: String,
    pub status: Option<crate::domain::agent::AgentStatus>,
}

pub trait AgentRepository {
    fn register(&self, input: &NewAgent, now: &str) -> Result<Agent, crate::error::CarryCtxError>;
    fn list(&self, filter: &AgentFilter) -> Result<Vec<Agent>, crate::error::CarryCtxError>;
    fn find_by_name(
        &self,
        project_id: &str,
        name: &str,
    ) -> Result<Option<Agent>, crate::error::CarryCtxError>;
    fn find_by_id(
        &self,
        project_id: &str,
        id: &str,
    ) -> Result<Option<Agent>, crate::error::CarryCtxError>;
    fn rename(
        &self,
        id: &str,
        project_id: &str,
        new_name: &str,
        now: &str,
    ) -> Result<Agent, crate::error::CarryCtxError>;
    fn deactivate(
        &self,
        id: &str,
        project_id: &str,
        now: &str,
    ) -> Result<Agent, crate::error::CarryCtxError>;
    fn has_nonterminal_tasks(
        &self,
        project_id: &str,
        agent_id: &str,
    ) -> Result<bool, crate::error::CarryCtxError>;
}
