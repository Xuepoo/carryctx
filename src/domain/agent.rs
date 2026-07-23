use crate::error::CarryCtxError;

/// Agent status
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Active,
    Deactivated,
}

/// Validate an agent name
pub fn validate_agent_name(name: &str) -> Result<(), CarryCtxError> {
    if name.is_empty() {
        return Err(CarryCtxError::validation_error(
            "Agent name cannot be empty.",
        ));
    }
    if name.len() > 64 {
        return Err(CarryCtxError::validation_error(
            "Agent name must be 64 characters or fewer.",
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(CarryCtxError::validation_error(
            "Agent name can only contain letters, numbers, hyphens, and underscores.",
        ));
    }
    Ok(())
}

/// Agent record
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Agent {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub provider: String,
    pub role: Option<String>,
    pub metadata: serde_json::Value,
    pub status: AgentStatus,
    pub created_at: String,
    pub updated_at: String,
    pub last_active_at: Option<String>,
}
