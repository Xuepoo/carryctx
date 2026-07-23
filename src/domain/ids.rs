use std::fmt;

/// Format a display ID with prefix and zero-padded sequence number
/// e.g. format_display_id("CTX", 1) -> "CTX-0001"
pub fn format_display_id(prefix: &str, seq: u32) -> String {
    format!("{}-{:04}", prefix, seq)
}

/// Parse a display ID into prefix and sequence number
pub fn parse_display_id(id: &str) -> Option<(&str, u32)> {
    let dash = id.rfind('-')?;
    let prefix = &id[..dash];
    let num_str = &id[dash + 1..];
    let seq: u32 = num_str.parse().ok()?;
    Some((prefix, seq))
}

/// Validate a task ID prefix (uppercase letters only, 1-10 chars)
pub fn validate_task_prefix(prefix: &str) -> Result<(), String> {
    if prefix.is_empty() || prefix.len() > 10 {
        return Err("Prefix must be 1-10 characters.".into());
    }
    if !prefix.chars().all(|c| c.is_ascii_uppercase()) {
        return Err("Prefix must be uppercase ASCII letters.".into());
    }
    Ok(())
}

/// ULID-based internal ID
pub type InternalId = ulid::Ulid;

/// Generate a new ULID
pub fn new_internal_id() -> InternalId {
    ulid::Ulid::generate()
}

/// Task display ID
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TaskId(pub String);

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Progress item display ID
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressId(pub String);

impl fmt::Display for ProgressId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Decision display ID
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionId(pub String);

/// Handoff display ID
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffId(pub String);
