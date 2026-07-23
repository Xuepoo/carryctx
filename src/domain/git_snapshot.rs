/// A snapshot of the Git working tree state
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GitSnapshot {
    pub branch: Option<String>,
    pub head: String,
    pub dirty: bool,
    pub staged: Vec<String>,
    pub modified: Vec<String>,
    pub deleted: Vec<String>,
    pub renamed: Vec<RenamedFile>,
    pub untracked: Vec<String>,
    pub diff_stats: Option<DiffStats>,
}

/// Diff statistics
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DiffStats {
    pub files: i64,
    pub insertions: i64,
    pub deletions: i64,
}

/// A renamed file record
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RenamedFile {
    pub from: String,
    pub to: String,
}
