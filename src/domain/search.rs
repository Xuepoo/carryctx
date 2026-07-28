use serde::{Deserialize, Serialize};

/// Which entity kind a search hit came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchKind {
    Task,
    Progress,
    Checkpoint,
    Decision,
}

impl SearchKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SearchKind::Task => "task",
            SearchKind::Progress => "progress",
            SearchKind::Checkpoint => "checkpoint",
            SearchKind::Decision => "decision",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "task" => Some(SearchKind::Task),
            "progress" => Some(SearchKind::Progress),
            "checkpoint" => Some(SearchKind::Checkpoint),
            "decision" => Some(SearchKind::Decision),
            _ => None,
        }
    }
}

/// A single full-text search hit, always resolved back to its owning task
/// (display ID + status) and, where available, the branch it was worked on
/// — the reason most searches like this happen in the first place, per
/// https://github.com/Xuepoo/carryctx/issues/45.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub kind: SearchKind,
    /// ULID of the matched record itself (task/progress/checkpoint/decision).
    pub id: String,
    /// Human-facing display ID of the matched record, if it has its own
    /// (progress items and decisions do; checkpoints don't).
    pub display_id: Option<String>,
    pub task_id: String,
    pub task_display_id: String,
    pub task_status: String,
    /// Best-known branch for the owning task: the task's own worktree
    /// binding if present, else the branch recorded on the matched
    /// checkpoint (if this hit is a checkpoint), else `None`.
    pub branch: Option<String>,
    /// A short excerpt around the match, with `[...]` bracketing the hit
    /// (SQLite FTS5's `snippet()` semantics).
    pub snippet: String,
    /// BM25 relevance score. Lower is more relevant (SQLite FTS5
    /// convention); results are already sorted by this ascending.
    pub score: f64,
    pub created_at: String,
}
