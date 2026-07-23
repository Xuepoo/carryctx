pub struct NewWorktree {
    pub id: String,
    pub project_id: String,
    pub path: String,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub task_id: Option<String>,
}

#[derive(serde::Serialize)]
pub struct WorktreeRecord {
    pub id: String,
    pub project_id: String,
    pub path: String,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub task_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

pub trait WorktreeRepository {
    fn upsert(
        &self,
        input: &NewWorktree,
        now: &str,
    ) -> Result<WorktreeRecord, crate::error::CarryCtxError>;
    fn find_by_id(
        &self,
        project_id: &str,
        id: &str,
    ) -> Result<Option<WorktreeRecord>, crate::error::CarryCtxError>;
    fn find_by_path(
        &self,
        project_id: &str,
        path: &str,
    ) -> Result<Option<WorktreeRecord>, crate::error::CarryCtxError>;
    fn find_by_task_id(
        &self,
        project_id: &str,
        task_id: &str,
    ) -> Result<Option<WorktreeRecord>, crate::error::CarryCtxError>;
    fn list(&self, project_id: &str) -> Result<Vec<WorktreeRecord>, crate::error::CarryCtxError>;
    fn unbind_task(
        &self,
        id: &str,
        project_id: &str,
        now: &str,
    ) -> Result<WorktreeRecord, crate::error::CarryCtxError>;
}
