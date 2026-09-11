use crate::error::CarryCtxError;

/// Agent status
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Active,
    Deactivated,
}

/// Execution kind recorded on an Agent.
///
/// `None` on the owning record means unclassified/legacy: it keeps the
/// pre-team behavior. Authorization checks only bite once an agent explicitly
/// opts into a kind (CTX-0044, design 2026-08-21 §3.2/§4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    Commander,
    Subagent,
}

impl AgentKind {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "commander" => Some(Self::Commander),
            "subagent" => Some(Self::Subagent),
            _ => None,
        }
    }
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
    pub kind: Option<String>,
    pub metadata: serde_json::Value,
    pub status: AgentStatus,
    pub created_at: String,
    pub updated_at: String,
    pub last_active_at: Option<String>,
}
