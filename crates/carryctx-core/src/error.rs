use std::fmt;
use thiserror::Error;

/// Exit codes matching CLI specification (0–12)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    Success = 0,
    General = 1,
    InvalidArguments = 2,
    StateConflict = 3,
    Git = 4,
    Database = 5,
    Configuration = 6,
    ResourceNotFound = 7,
    Validation = 8,
    PermissionScope = 9,
    Unsupported = 10,
    MigrationRequired = 11,
    Interrupted = 12,
}

/// Stable public error codes used in JSON output
pub type ErrorCode = &'static str;

/// CarryCtx domain & application error
#[derive(Error)]
pub struct CarryCtxError {
    pub code: ErrorCode,
    pub message: String,
    pub exit_code: ExitCode,
    #[source]
    pub source: Option<Box<dyn std::error::Error + Send + Sync>>,
    pub details: serde_json::Value,
    pub suggestions: Vec<String>,
}

impl CarryCtxError {
    pub fn new(code: ErrorCode, message: impl Into<String>, exit_code: ExitCode) -> Self {
        Self {
            code,
            message: message.into(),
            exit_code,
            source: None,
            details: serde_json::Value::Null,
            suggestions: Vec::new(),
        }
    }

    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = details;
        self
    }

    pub fn with_suggestions(mut self, suggestions: impl IntoIterator<Item = String>) -> Self {
        self.suggestions = suggestions.into_iter().collect();
        self
    }

    pub fn with_source(mut self, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    pub fn with_context(mut self, context: impl Into<String>) -> Self {
        self.message = format!("{}: {}", context.into(), self.message);
        self
    }
}

impl fmt::Debug for CarryCtxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CarryCtxError({}, {})", self.code, self.message)
    }
}

impl fmt::Display for CarryCtxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

/// Common error constructors
impl CarryCtxError {
    pub fn permission_scope(msg: impl Into<String>) -> Self {
        Self::new("PERMISSION_SCOPE", msg, ExitCode::PermissionScope)
    }

    pub fn invalid_arguments(msg: impl Into<String>) -> Self {
        Self::new("INVALID_ARGUMENTS", msg, ExitCode::InvalidArguments)
    }

    pub fn state_conflict(msg: impl Into<String>) -> Self {
        Self::new("STATE_CONFLICT", msg, ExitCode::StateConflict)
    }

    pub fn resource_not_found(msg: impl Into<String>) -> Self {
        Self::new("RESOURCE_NOT_FOUND", msg, ExitCode::ResourceNotFound)
    }

    /// Merge produced blocking conflicts and staged a merge session; the live
    /// database is untouched (design §2.4, §2.6: `MERGE_CONFLICTS`, exit 3).
    /// `details` carries the staged `mergeId` and the blocking `conflicts`
    /// count so a caller can drive `conflict list/show/resolve` (CTX-0143).
    pub fn merge_conflicts(msg: impl Into<String>, merge_id: &str, conflict_count: u64) -> Self {
        Self::new("MERGE_CONFLICTS", msg, ExitCode::StateConflict).with_details(serde_json::json!({
            "mergeId": merge_id,
            "conflicts": conflict_count,
        }))
    }

    pub fn task_already_claimed(task_id: &str, owner: &str) -> Self {
        Self::new(
            "TASK_ALREADY_CLAIMED",
            format!("Task {} is already claimed by {}", task_id, owner),
            ExitCode::StateConflict,
        )
        .with_details(serde_json::json!({"owner": owner}))
        .with_suggestions([
            format!("Run carryctx task show {}", task_id),
            "Ask the current owner to release the task.".into(),
        ])
    }

    pub fn migration_required(msg: impl Into<String>) -> Self {
        Self::new("MIGRATION_REQUIRED", msg, ExitCode::MigrationRequired)
    }

    pub fn dependency_cycle() -> Self {
        Self::new(
            "DEPENDENCY_CYCLE",
            "Adding this dependency would create a cycle.",
            ExitCode::StateConflict,
        )
    }

    pub fn dependency_incomplete(task_id: &str) -> Self {
        Self::new(
            "DEPENDENCY_INCOMPLETE",
            format!("Task {} has incomplete strong dependencies.", task_id),
            ExitCode::StateConflict,
        )
    }

    pub fn invalid_task_transition(from: &str, to: &str) -> Self {
        Self::new(
            "INVALID_TASK_TRANSITION",
            format!("Cannot transition from {} to {}.", from, to),
            ExitCode::StateConflict,
        )
    }

    pub fn git_error(msg: impl Into<String>) -> Self {
        Self::new("GIT_ERROR", msg, ExitCode::Git)
    }

    pub fn database_error(msg: impl Into<String>) -> Self {
        Self::new("DATABASE_ERROR", msg, ExitCode::Database)
    }

    pub fn configuration_error(msg: impl Into<String>) -> Self {
        Self::new("CONFIGURATION_ERROR", msg, ExitCode::Configuration)
    }

    /// Filesystem I/O failures that are not resource-lookup problems
    /// (permission denied, disk full, ...). Previously these were misfiled as
    /// `RESOURCE_NOT_FOUND`, which told agents the file was absent when it
    /// actually existed and could not be read or written.
    pub fn io_error(msg: impl Into<String>) -> Self {
        Self::new("IO_ERROR", msg, ExitCode::General)
    }

    pub fn validation_error(msg: impl Into<String>) -> Self {
        Self::new("VALIDATION_FAILED", msg, ExitCode::Validation)
    }

    pub fn unsupported_operation(msg: impl Into<String>) -> Self {
        Self::new("UNSUPPORTED_OPERATION", msg, ExitCode::Unsupported)
    }

    pub fn interrupted() -> Self {
        Self::new("INTERRUPTED", "Operation cancelled.", ExitCode::Interrupted)
    }
}

// CarryCtxError already implements std::error::Error via thiserror,
// so anyhow's blanket From<E: StdError> impl covers it automatically.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_conflicts_carries_code_exit_and_details() {
        let error = CarryCtxError::merge_conflicts("conflicts staged", "01MERGE", 3);
        assert_eq!(error.code, "MERGE_CONFLICTS");
        assert_eq!(error.exit_code, ExitCode::StateConflict);
        assert_eq!(error.exit_code as i32, 3);
        assert_eq!(error.details["mergeId"], "01MERGE");
        assert_eq!(error.details["conflicts"], 3);
    }
}
