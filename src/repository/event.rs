pub struct NewEvent {
    pub id: String,
    pub project_id: String,
    pub event_type: String,
    pub actor_agent_id: Option<String>,
    pub session_id: Option<String>,
    pub task_id: Option<String>,
    pub payload: serde_json::Value,
    pub occurred_at: String,
}

#[derive(serde::Serialize)]
pub struct EventRecord {
    pub id: String,
    pub project_id: String,
    pub event_type: String,
    pub actor_agent_id: Option<String>,
    pub session_id: Option<String>,
    pub task_id: Option<String>,
    pub payload: serde_json::Value,
    pub occurred_at: String,
}

pub struct EventFilter {
    pub project_id: String,
    pub task_id: Option<String>,
    pub agent_id: Option<String>,
    pub session_id: Option<String>,
    pub event_type: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub limit: Option<u64>,
}

pub trait EventRepository {
    fn append(&self, event: &NewEvent) -> Result<EventRecord, crate::error::CarryCtxError>;
    fn find_by_id(
        &self,
        project_id: &str,
        id: &str,
    ) -> Result<Option<EventRecord>, crate::error::CarryCtxError>;
    fn list(&self, filter: &EventFilter) -> Result<Vec<EventRecord>, crate::error::CarryCtxError>;
    /// Return every event of one type for one task. Unlike `list`, this is
    /// deliberately unbounded because it is used for authorization history,
    /// not general-purpose event pagination.
    ///
    /// The default keeps existing repository implementations source-compatible.
    /// Storage adapters with a specialized unbounded query should override it.
    fn list_task_events_by_type(
        &self,
        project_id: &str,
        task_id: &str,
        event_type: &str,
    ) -> Result<Vec<EventRecord>, crate::error::CarryCtxError> {
        self.list(&EventFilter {
            project_id: project_id.to_owned(),
            task_id: Some(task_id.to_owned()),
            agent_id: None,
            session_id: None,
            event_type: Some(event_type.to_owned()),
            since: None,
            until: None,
            limit: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ExistingRepository;

    impl EventRepository for ExistingRepository {
        fn append(&self, _event: &NewEvent) -> Result<EventRecord, crate::error::CarryCtxError> {
            unreachable!()
        }

        fn find_by_id(
            &self,
            _project_id: &str,
            _id: &str,
        ) -> Result<Option<EventRecord>, crate::error::CarryCtxError> {
            unreachable!()
        }

        fn list(
            &self,
            filter: &EventFilter,
        ) -> Result<Vec<EventRecord>, crate::error::CarryCtxError> {
            assert_eq!(filter.project_id, "project");
            assert_eq!(filter.task_id.as_deref(), Some("task"));
            assert_eq!(filter.event_type.as_deref(), Some("task.completed"));
            assert!(filter.limit.is_none());
            Ok(Vec::new())
        }
    }

    #[test]
    fn default_task_event_history_method_preserves_existing_implementations() {
        let repository: &dyn EventRepository = &ExistingRepository;

        repository
            .list_task_events_by_type("project", "task", "task.completed")
            .unwrap();
    }
}
