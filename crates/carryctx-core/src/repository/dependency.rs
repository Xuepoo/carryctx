use crate::domain::dependency::{DependencyEdge, DependencyKind};

pub trait DependencyRepository {
    fn add(
        &self,
        project_id: &str,
        task_id: &str,
        prerequisite_id: &str,
        kind: DependencyKind,
    ) -> Result<(), crate::error::CarryCtxError>;
    fn remove(
        &self,
        project_id: &str,
        task_id: &str,
        prerequisite_id: &str,
    ) -> Result<(), crate::error::CarryCtxError>;
    fn list_for_task(
        &self,
        project_id: &str,
        task_id: &str,
    ) -> Result<Vec<DependencyEdge>, crate::error::CarryCtxError>;
    fn list_all_for_project(
        &self,
        project_id: &str,
    ) -> Result<Vec<DependencyEdge>, crate::error::CarryCtxError>;
    fn find_edge(
        &self,
        project_id: &str,
        task_id: &str,
        prerequisite_id: &str,
    ) -> Result<Option<DependencyEdge>, crate::error::CarryCtxError>;
}
