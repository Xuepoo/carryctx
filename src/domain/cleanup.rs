use crate::error::CarryCtxError;

/// Lifecycle state for a worktree cleanup request.
///
/// The six-state model is the source of truth; SQLite enforces it via a
/// `CHECK(state IN (...))` constraint so the Rust enum and the database can
/// never drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupState {
    Pending,
    Running,
    Blocked,
    Completed,
    Failed,
    Cancelled,
}

impl CleanupState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Blocked => "blocked",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    pub fn is_active(self) -> bool {
        matches!(self, Self::Pending | Self::Running | Self::Blocked)
    }
}

impl std::fmt::Display for CleanupState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for CleanupState {
    type Err = CarryCtxError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "blocked" => Ok(Self::Blocked),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(CarryCtxError::validation_error(format!(
                "Unknown cleanup state: {other}"
            ))),
        }
    }
}

/// Why the cleanup was requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupReason {
    TaskCompleted,
    Manual,
}

impl CleanupReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TaskCompleted => "task_completed",
            Self::Manual => "manual",
        }
    }
}

impl std::fmt::Display for CleanupReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for CleanupReason {
    type Err = CarryCtxError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "task_completed" => Ok(Self::TaskCompleted),
            "manual" => Ok(Self::Manual),
            other => Err(CarryCtxError::validation_error(format!(
                "Unknown cleanup reason: {other}"
            ))),
        }
    }
}

/// Why a cleanup cannot proceed. Safe to surface to users and to persist as a
/// single `blocked_reason` TEXT column; `ActiveSession` carries the session id
/// so the scheduler can explain which session blocks removal.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[serde(tag = "kind", content = "value")]
pub enum CleanupBlocker {
    ActiveSession { session_id: String },
    DirtyWorktree,
    CurrentWorkingDirectory,
    MissingGitMetadata,
    WorktreeLocked,
    JjColocation,
}

impl CleanupBlocker {
    /// Canonical string stored in `blocked_reason` for blocker variants that
    /// have no payload. `ActiveSession` is encoded as
    /// `active_session:<session_id>` so the column stays a single TEXT field
    /// while remaining reversible without JSON.
    pub fn to_db_string(&self) -> String {
        match self {
            Self::ActiveSession { session_id } => format!("active_session:{session_id}"),
            Self::DirtyWorktree => "dirty_worktree".into(),
            Self::CurrentWorkingDirectory => "current_working_directory".into(),
            Self::MissingGitMetadata => "missing_git_metadata".into(),
            Self::WorktreeLocked => "worktree_locked".into(),
            Self::JjColocation => "jj_colocation".into(),
        }
    }

    pub fn from_db_string(s: &str) -> Option<Self> {
        if let Some(rest) = s.strip_prefix("active_session:") {
            return Some(Self::ActiveSession {
                session_id: rest.to_string(),
            });
        }
        match s {
            "dirty_worktree" => Some(Self::DirtyWorktree),
            "current_working_directory" => Some(Self::CurrentWorkingDirectory),
            "missing_git_metadata" => Some(Self::MissingGitMetadata),
            "worktree_locked" => Some(Self::WorktreeLocked),
            "jj_colocation" => Some(Self::JjColocation),
            // Legacy plain `active_session` without an id (never written by
            // current code, but tolerate on read).
            "active_session" => Some(Self::ActiveSession {
                session_id: String::new(),
            }),
            _ => None,
        }
    }
}

impl std::fmt::Display for CleanupBlocker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ActiveSession { session_id } if session_id.is_empty() => {
                write!(f, "active_session")
            }
            Self::ActiveSession { session_id } => write!(f, "active_session:{session_id}"),
            Self::DirtyWorktree => write!(f, "dirty_worktree"),
            Self::CurrentWorkingDirectory => write!(f, "current_working_directory"),
            Self::MissingGitMetadata => write!(f, "missing_git_metadata"),
            Self::WorktreeLocked => write!(f, "worktree_locked"),
            Self::JjColocation => write!(f, "jj_colocation"),
        }
    }
}

/// Persisted cleanup request. Mirrors the `worktree_cleanup_requests` table
/// exactly so the repository can map rows without loss.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CleanupRequest {
    pub id: String,
    pub project_id: String,
    /// Nullable FK to `worktrees(id)` — the request keeps a snapshot of
    /// `worktree_path`/`branch` even after the worktree row is pruned.
    pub worktree_id: Option<String>,
    pub worktree_path: String,
    pub branch: Option<String>,
    pub task_id: Option<String>,
    pub reason: CleanupReason,
    pub state: CleanupState,
    pub blocked_reason: Option<CleanupBlocker>,
    pub attempt_count: i64,
    pub requested_at: String,
    pub last_attempt_at: Option<String>,
    pub completed_at: Option<String>,
}

impl CleanupRequest {
    pub fn is_terminal(&self) -> bool {
        self.state.is_terminal()
    }

    pub fn is_active(&self) -> bool {
        self.state.is_active()
    }
}

/// Pure assessment result — no I/O, no git, no rusqlite.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CleanupAssessment {
    pub removable: bool,
    pub blockers: Vec<CleanupBlocker>,
}

impl CleanupAssessment {
    pub fn removable() -> Self {
        Self {
            removable: true,
            blockers: Vec::new(),
        }
    }

    pub fn blocked(blockers: Vec<CleanupBlocker>) -> Self {
        Self {
            removable: false,
            blockers,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn state_round_trips_via_display_and_from_str() {
        for state in [
            CleanupState::Pending,
            CleanupState::Running,
            CleanupState::Blocked,
            CleanupState::Completed,
            CleanupState::Failed,
            CleanupState::Cancelled,
        ] {
            let s = state.to_string();
            assert_eq!(CleanupState::from_str(&s).unwrap(), state);
            // serde snake_case
            let json = serde_json::to_string(&state).unwrap();
            let back: CleanupState = serde_json::from_str(&json).unwrap();
            assert_eq!(back, state);
        }
    }

    #[test]
    fn state_terminal_and_active_partitions() {
        assert!(!CleanupState::Pending.is_terminal());
        assert!(!CleanupState::Running.is_terminal());
        assert!(!CleanupState::Blocked.is_terminal());
        assert!(CleanupState::Completed.is_terminal());
        assert!(CleanupState::Failed.is_terminal());
        assert!(CleanupState::Cancelled.is_terminal());

        assert!(CleanupState::Pending.is_active());
        assert!(CleanupState::Running.is_active());
        assert!(CleanupState::Blocked.is_active());
        assert!(!CleanupState::Completed.is_active());
    }

    #[test]
    fn reason_round_trips() {
        for r in [CleanupReason::TaskCompleted, CleanupReason::Manual] {
            let s = r.to_string();
            assert_eq!(CleanupReason::from_str(&s).unwrap(), r);
            let json = serde_json::to_string(&r).unwrap();
            let back: CleanupReason = serde_json::from_str(&json).unwrap();
            assert_eq!(back, r);
        }
    }

    #[test]
    fn blocker_db_string_round_trips() {
        let cases = vec![
            CleanupBlocker::ActiveSession {
                session_id: "sess-123".into(),
            },
            CleanupBlocker::DirtyWorktree,
            CleanupBlocker::CurrentWorkingDirectory,
            CleanupBlocker::MissingGitMetadata,
            CleanupBlocker::WorktreeLocked,
            CleanupBlocker::JjColocation,
        ];
        for b in cases {
            let s = b.to_db_string();
            let back = CleanupBlocker::from_db_string(&s).expect("round-trip");
            assert_eq!(back, b);
            assert_eq!(b.to_string(), s);
        }
    }

    #[test]
    fn blocker_active_session_without_id_tolerated() {
        let b = CleanupBlocker::from_db_string("active_session").unwrap();
        assert_eq!(
            b,
            CleanupBlocker::ActiveSession {
                session_id: String::new()
            }
        );
    }

    #[test]
    fn blocker_unknown_returns_none() {
        assert!(CleanupBlocker::from_db_string("nope").is_none());
    }

    #[test]
    fn assessment_helpers() {
        let a = CleanupAssessment::removable();
        assert!(a.removable);
        assert!(a.blockers.is_empty());
        let b = CleanupAssessment::blocked(vec![CleanupBlocker::DirtyWorktree]);
        assert!(!b.removable);
        assert_eq!(b.blockers.len(), 1);
    }

    #[test]
    fn request_is_terminal_delegates_to_state() {
        let mut req = CleanupRequest {
            id: "id".into(),
            project_id: "p".into(),
            worktree_id: None,
            worktree_path: "/tmp/wt".into(),
            branch: None,
            task_id: None,
            reason: CleanupReason::Manual,
            state: CleanupState::Completed,
            blocked_reason: None,
            attempt_count: 0,
            requested_at: "now".into(),
            last_attempt_at: None,
            completed_at: Some("now".into()),
        };
        assert!(req.is_terminal());
        req.state = CleanupState::Pending;
        assert!(!req.is_terminal());
    }
}
