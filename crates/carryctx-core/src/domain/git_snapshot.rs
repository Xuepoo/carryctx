/// Which VCS backend produced this snapshot.
///
/// `Jj` means the repository is Jujutsu-colocated (a `.jj/` directory sits
/// alongside `.git/`). CarryCtx always reads state through Git itself; this
/// only records which backend's behavior shaped the snapshot's shape, since
/// jj's automatic working-copy snapshotting makes the staged/unstaged split
/// unreliable (see carryctx-docs/plans/2026-07-25-jujutsu-compatibility.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VcsBackend {
    Git,
    Jj,
}

impl VcsBackend {
    pub fn as_str(&self) -> &'static str {
        match self {
            VcsBackend::Git => "git",
            VcsBackend::Jj => "jj",
        }
    }
}

impl std::fmt::Display for VcsBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A snapshot of the Git working tree state
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GitSnapshot {
    pub branch: Option<String>,
    pub head: Option<String>,
    pub dirty: bool,
    pub vcs_backend: VcsBackend,
    /// Staged files (index differs from HEAD). Under jj colocation this is
    /// always empty; use `changed_files` instead (see `vcs_backend`).
    pub staged: Vec<String>,
    /// Modified-but-unstaged files. Always empty under jj colocation.
    pub modified: Vec<String>,
    pub deleted: Vec<String>,
    pub renamed: Vec<RenamedFile>,
    /// Untracked files. Always empty under jj colocation.
    pub untracked: Vec<String>,
    /// Accurate under both backends: every file with any change (staged,
    /// unstaged, or untracked), collapsed into one list. Populated for both
    /// git and jj snapshots; git callers that want the finer split should
    /// keep using `staged`/`modified`/`untracked` instead.
    pub changed_files: Vec<String>,
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
