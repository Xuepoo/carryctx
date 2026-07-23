pub trait ScopeRepository {
    fn add(
        &self,
        project_id: &str,
        task_id: &str,
        pattern: &str,
        now: &str,
    ) -> Result<(), crate::error::CarryCtxError>;
    fn remove(
        &self,
        project_id: &str,
        task_id: &str,
        pattern: &str,
    ) -> Result<(), crate::error::CarryCtxError>;
    fn list_for_task(
        &self,
        project_id: &str,
        task_id: &str,
    ) -> Result<Vec<crate::domain::collaboration::TaskScope>, crate::error::CarryCtxError>;
    fn list_active_scopes(
        &self,
        project_id: &str,
    ) -> Result<Vec<crate::domain::collaboration::TaskScope>, crate::error::CarryCtxError>;
}

pub trait DecisionRepository {
    fn create(
        &self,
        decision: &crate::domain::collaboration::Decision,
    ) -> Result<crate::domain::collaboration::Decision, crate::error::CarryCtxError>;
    fn find_by_id(
        &self,
        project_id: &str,
        id: &str,
    ) -> Result<Option<crate::domain::collaboration::Decision>, crate::error::CarryCtxError>;
    fn list(
        &self,
        project_id: &str,
    ) -> Result<Vec<crate::domain::collaboration::Decision>, crate::error::CarryCtxError>;
    fn search(
        &self,
        project_id: &str,
        query: &str,
    ) -> Result<Vec<crate::domain::collaboration::Decision>, crate::error::CarryCtxError>;
    fn supersede(
        &self,
        decision_id: &str,
        project_id: &str,
        superseded_by: &str,
        now: &str,
    ) -> Result<(), crate::error::CarryCtxError>;
}

pub trait HandoffRepository {
    fn create(
        &self,
        handoff: &crate::domain::collaboration::Handoff,
    ) -> Result<crate::domain::collaboration::Handoff, crate::error::CarryCtxError>;
    fn find_by_id(
        &self,
        project_id: &str,
        id: &str,
    ) -> Result<Option<crate::domain::collaboration::Handoff>, crate::error::CarryCtxError>;
    fn list(
        &self,
        project_id: &str,
    ) -> Result<Vec<crate::domain::collaboration::Handoff>, crate::error::CarryCtxError>;
    fn update_status(
        &self,
        id: &str,
        project_id: &str,
        status: crate::domain::collaboration::HandoffStatus,
        now: &str,
    ) -> Result<(), crate::error::CarryCtxError>;
}
