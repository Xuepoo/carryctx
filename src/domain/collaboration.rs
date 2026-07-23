/// Scope overlap classification
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum ScopeOverlap {
    None,
    Definite,
    Possible,
}

/// A task path scope (glob pattern)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TaskScope {
    pub id: String,
    pub task_id: String,
    pub pattern: String,
    pub created_at: String,
}

/// A technical decision record
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Decision {
    pub id: String,
    pub display_id: String,
    pub project_id: String,
    pub title: String,
    pub context: Option<String>,
    pub decision: Option<String>,
    pub consequences: Option<String>,
    pub related_tasks: Vec<String>,
    pub related_paths: Vec<String>,
    pub created_by_agent: String,
    pub created_by_session: Option<String>,
    pub superseded_by: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// A handoff record for task transfer between agents
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Handoff {
    pub id: String,
    pub display_id: String,
    pub project_id: String,
    pub task_id: String,
    pub source_agent_id: String,
    pub source_session_id: Option<String>,
    pub target_agent_id: Option<String>,
    pub summary: Option<String>,
    pub completed_work: Vec<String>,
    pub remaining_work: Vec<String>,
    pub blockers: Vec<String>,
    pub risks: Vec<String>,
    pub next_steps: Vec<String>,
    pub changed_files: Vec<String>,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub status: HandoffStatus,
    pub created_at: String,
    pub updated_at: String,
}

/// Handoff lifecycle status
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffStatus {
    Open,
    Accepted,
    Rejected,
    Closed,
}
