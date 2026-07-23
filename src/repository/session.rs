use crate::domain::session::SessionState;

pub struct NewSession {
    pub id: String,
    pub project_id: String,
    pub agent_id: String,
    pub task_id: Option<String>,
    pub worktree_id: Option<String>,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub cwd: Option<String>,
    pub provider: Option<String>,
}

#[derive(serde::Serialize)]
pub struct SessionRecord {
    pub id: String,
    pub project_id: String,
    pub agent_id: String,
    pub task_id: Option<String>,
    pub worktree_id: Option<String>,
    pub state: SessionState,
    pub provider: Option<String>,
    pub metadata: serde_json::Value,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub cwd: Option<String>,
    pub summary: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub last_activity_at: String,
}

pub trait SessionRepository {
    fn create(
        &self,
        input: &NewSession,
        now: &str,
    ) -> Result<SessionRecord, crate::error::CarryCtxError>;
    fn find_by_id(
        &self,
        project_id: &str,
        id: &str,
    ) -> Result<Option<SessionRecord>, crate::error::CarryCtxError>;
    fn list(&self, project_id: &str) -> Result<Vec<SessionRecord>, crate::error::CarryCtxError>;
    fn find_active(
        &self,
        project_id: &str,
        agent_id: &str,
        worktree_id: Option<&str>,
    ) -> Result<Vec<SessionRecord>, crate::error::CarryCtxError>;
    fn update_state(
        &self,
        id: &str,
        project_id: &str,
        state: SessionState,
        now: &str,
        summary: Option<&str>,
    ) -> Result<SessionRecord, crate::error::CarryCtxError>;
    fn touch_activity(
        &self,
        id: &str,
        project_id: &str,
        now: &str,
    ) -> Result<(), crate::error::CarryCtxError>;
    fn mark_overdue_stale(
        &self,
        project_id: &str,
        stale_before: &str,
        now: &str,
    ) -> Result<u64, crate::error::CarryCtxError>;
}
