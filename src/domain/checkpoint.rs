/// A Git-aware checkpoint capturing task progress and repository state
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Checkpoint {
    pub id: String,
    pub project_id: String,
    pub task_id: String,
    pub session_id: Option<String>,
    pub agent_id: Option<String>,
    pub worktree_id: Option<String>,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub dirty: bool,
    pub staged_files: Vec<String>,
    pub modified_files: Vec<String>,
    pub deleted_files: Vec<String>,
    pub renamed_files: Vec<RenamedFile>,
    pub untracked_files: Vec<String>,
    pub diff_files: Option<i64>,
    pub diff_insertions: Option<i64>,
    pub diff_deletions: Option<i64>,
    pub done: Vec<String>,
    pub remaining: Vec<String>,
    pub blockers: Vec<String>,
    pub risks: Vec<String>,
    pub next_actions: Vec<String>,
    pub notes: Vec<String>,
    pub created_at: String,
}

/// A correction to a checkpoint (immutable base + overlay)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CheckpointCorrection {
    pub id: String,
    pub checkpoint_id: String,
    pub done: Option<Vec<String>>,
    pub remaining: Option<Vec<String>>,
    pub blockers: Option<Vec<String>>,
    pub risks: Option<Vec<String>>,
    pub next_actions: Option<Vec<String>>,
    pub notes: Option<Vec<String>>,
    pub created_at: String,
}

/// A renamed file in a Git snapshot
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RenamedFile {
    pub from: String,
    pub to: String,
}
