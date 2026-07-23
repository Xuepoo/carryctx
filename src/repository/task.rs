use crate::domain::task::{TaskPriority, TaskStatus};

pub struct NewTask {
    pub id: String,
    pub display_id: String,
    pub project_id: String,
    pub title: String,
    pub description: Option<String>,
    pub status: TaskStatus,
    pub priority: TaskPriority,
    pub owner_agent_id: Option<String>,
    pub parent_task_id: Option<String>,
}

#[derive(Clone, serde::Serialize)]
pub struct TaskRecord {
    pub id: String,
    pub display_id: String,
    pub project_id: String,
    pub title: String,
    pub description: Option<String>,
    pub status: TaskStatus,
    pub priority: TaskPriority,
    pub owner_agent_id: Option<String>,
    pub parent_task_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

pub struct TaskFilter {
    pub project_id: String,
    pub status: Option<TaskStatus>,
    pub owner_agent_id: Option<String>,
    pub ready: bool,
    pub blocked: bool,
    pub mine: Option<String>,
}

pub trait TaskRepository {
    fn allocate_display_id(
        &self,
        project_id: &str,
        prefix: &str,
    ) -> Result<u32, crate::error::CarryCtxError>;
    fn create(&self, input: &NewTask, now: &str)
    -> Result<TaskRecord, crate::error::CarryCtxError>;
    fn find_by_id(
        &self,
        project_id: &str,
        id: &str,
    ) -> Result<Option<TaskRecord>, crate::error::CarryCtxError>;
    fn find_by_display_id(
        &self,
        project_id: &str,
        display_id: &str,
    ) -> Result<Option<TaskRecord>, crate::error::CarryCtxError>;
    fn list(&self, filter: &TaskFilter) -> Result<Vec<TaskRecord>, crate::error::CarryCtxError>;
    fn update_status(
        &self,
        id: &str,
        project_id: &str,
        status: TaskStatus,
        owner_agent_id: Option<String>,
        now: &str,
    ) -> Result<TaskRecord, crate::error::CarryCtxError>;
    fn count_open_progress(
        &self,
        project_id: &str,
        task_id: &str,
    ) -> Result<u64, crate::error::CarryCtxError>;
    fn has_active_session(
        &self,
        project_id: &str,
        task_id: &str,
    ) -> Result<bool, crate::error::CarryCtxError>;
    fn list_incomplete_strong_dependencies(
        &self,
        project_id: &str,
        task_id: &str,
    ) -> Result<Vec<String>, crate::error::CarryCtxError>;
    fn edit(
        &self,
        id: &str,
        project_id: &str,
        title: &str,
        priority: TaskPriority,
        now: &str,
    ) -> Result<TaskRecord, crate::error::CarryCtxError>;
}
