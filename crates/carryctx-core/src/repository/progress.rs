use crate::domain::progress::{ProgressStatus, ProgressType};

pub struct NewProgressItem {
    pub id: String,
    pub display_id: String,
    pub project_id: String,
    pub task_id: String,
    pub source_session_id: Option<String>,
    pub item_type: ProgressType,
    pub content: String,
    pub position: u32,
}

#[derive(serde::Serialize)]
pub struct ProgressItemRecord {
    pub id: String,
    pub display_id: String,
    pub project_id: String,
    pub task_id: String,
    pub source_session_id: Option<String>,
    pub item_type: ProgressType,
    pub status: ProgressStatus,
    pub content: String,
    pub position: u32,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
    pub removed_at: Option<String>,
}

pub struct ProgressFilter {
    pub project_id: String,
    pub task_id: String,
    pub include_removed: bool,
}

pub trait ProgressRepository {
    fn allocate_display_id(&self, project_id: &str) -> Result<u32, crate::error::CarryCtxError>;
    fn get_next_position(
        &self,
        project_id: &str,
        task_id: &str,
    ) -> Result<u32, crate::error::CarryCtxError>;
    fn create(
        &self,
        input: &NewProgressItem,
        now: &str,
    ) -> Result<ProgressItemRecord, crate::error::CarryCtxError>;
    fn find_by_id(
        &self,
        project_id: &str,
        id: &str,
    ) -> Result<Option<ProgressItemRecord>, crate::error::CarryCtxError>;
    fn find_by_display_id(
        &self,
        project_id: &str,
        display_id: &str,
    ) -> Result<Option<ProgressItemRecord>, crate::error::CarryCtxError>;
    fn list(
        &self,
        filter: &ProgressFilter,
    ) -> Result<Vec<ProgressItemRecord>, crate::error::CarryCtxError>;
    fn edit(
        &self,
        id: &str,
        project_id: &str,
        content: &str,
        now: &str,
    ) -> Result<ProgressItemRecord, crate::error::CarryCtxError>;
    fn update_status(
        &self,
        id: &str,
        project_id: &str,
        status: ProgressStatus,
        now: &str,
    ) -> Result<ProgressItemRecord, crate::error::CarryCtxError>;
    fn reorder(
        &self,
        project_id: &str,
        task_id: &str,
        ordered_ids: &[String],
    ) -> Result<(), crate::error::CarryCtxError>;
}
