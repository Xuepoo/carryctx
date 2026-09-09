use crate::error::CarryCtxError;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Team {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub commander_agent_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TeamMember {
    pub project_id: String,
    pub team_id: String,
    pub agent_id: String,
    pub role: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TeamStatusTask {
    pub display_id: String,
    pub status: String,
    pub team_id: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TeamStatusMember {
    pub agent_id: String,
    pub name: String,
    pub kind: Option<String>,
    pub role: Option<String>,
    pub active_session_id: Option<String>,
    pub tasks: Vec<TeamStatusTask>,
    pub active_task_count: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TeamStatusCounts {
    pub total: usize,
    pub commanders: usize,
    pub subagents: usize,
    pub unassigned: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TeamStatusProjection {
    pub team: Team,
    pub members: Vec<TeamStatusMember>,
    pub counts: TeamStatusCounts,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TeamContextProjection {
    pub team: serde_json::Value,
    pub view: String,
    pub members: Vec<serde_json::Value>,
    pub tasks: Vec<serde_json::Value>,
    pub dependencies: Vec<serde_json::Value>,
    pub scopes: Vec<serde_json::Value>,
    pub progress: Vec<serde_json::Value>,
    pub scope_conflicts: Vec<serde_json::Value>,
    pub blockers: Vec<serde_json::Value>,
    pub conflicts: Vec<serde_json::Value>,
    pub latest_checkpoints: Vec<serde_json::Value>,
    pub decisions: Vec<serde_json::Value>,
    pub handoffs: Vec<serde_json::Value>,
    pub recent_events: Vec<serde_json::Value>,
    pub rebuild: serde_json::Value,
}

pub fn validate_team_name(name: &str) -> Result<(), CarryCtxError> {
    if name.trim().is_empty() {
        Err(CarryCtxError::validation_error(
            "Team name cannot be empty.",
        ))
    } else {
        Ok(())
    }
}
