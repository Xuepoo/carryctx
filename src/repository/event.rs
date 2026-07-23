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
}
