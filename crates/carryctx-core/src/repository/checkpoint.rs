use crate::domain::checkpoint::{Checkpoint, CheckpointCorrection};

pub trait CheckpointRepository {
    fn create(&self, checkpoint: &Checkpoint) -> Result<Checkpoint, crate::error::CarryCtxError>;
    fn find_by_id(
        &self,
        project_id: &str,
        id: &str,
    ) -> Result<Option<Checkpoint>, crate::error::CarryCtxError>;
    fn find_latest_for_task(
        &self,
        project_id: &str,
        task_id: &str,
    ) -> Result<Option<Checkpoint>, crate::error::CarryCtxError>;
    fn list(
        &self,
        project_id: &str,
        task_id: Option<&str>,
    ) -> Result<Vec<Checkpoint>, crate::error::CarryCtxError>;
    fn correct(&self, correction: &CheckpointCorrection)
    -> Result<(), crate::error::CarryCtxError>;
}
