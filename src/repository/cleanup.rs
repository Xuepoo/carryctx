use crate::domain::cleanup::{CleanupBlocker, CleanupReason, CleanupState};
use crate::error::CarryCtxError;

pub struct NewCleanupRequest {
    pub id: String,
    pub project_id: String,
    pub worktree_id: Option<String>,
    pub worktree_path: String,
    pub branch: Option<String>,
    pub task_id: Option<String>,
    pub reason: CleanupReason,
    pub requested_at: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CleanupRecord {
    pub id: String,
    pub project_id: String,
    pub worktree_id: Option<String>,
    pub worktree_path: String,
    pub branch: Option<String>,
    pub task_id: Option<String>,
    pub reason: CleanupReason,
    pub state: CleanupState,
    pub blocked_reason: Option<CleanupBlocker>,
    pub attempt_count: i64,
    pub requested_at: String,
    pub last_attempt_at: Option<String>,
    pub completed_at: Option<String>,
}

pub trait CleanupRepository {
    fn create(&self, input: &NewCleanupRequest) -> Result<CleanupRecord, CarryCtxError>;

    fn find_by_id(
        &self,
        project_id: &str,
        id: &str,
    ) -> Result<Option<CleanupRecord>, CarryCtxError>;

    fn find_by_task(
        &self,
        project_id: &str,
        task_id: &str,
    ) -> Result<Vec<CleanupRecord>, CarryCtxError>;

    fn find_pending_by_project(
        &self,
        project_id: &str,
    ) -> Result<Vec<CleanupRecord>, CarryCtxError>;

    fn list(
        &self,
        project_id: &str,
        state: Option<CleanupState>,
    ) -> Result<Vec<CleanupRecord>, CarryCtxError>;

    fn update_state(
        &self,
        id: &str,
        project_id: &str,
        state: CleanupState,
        blocked_reason: Option<CleanupBlocker>,
        now: &str,
    ) -> Result<CleanupRecord, CarryCtxError>;

    fn increment_attempt(
        &self,
        id: &str,
        project_id: &str,
        now: &str,
    ) -> Result<CleanupRecord, CarryCtxError>;
}
