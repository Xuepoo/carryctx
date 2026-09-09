//! VcsBackend trait + supporting types (domain-owned, crate-agnostic).

use std::path::PathBuf;

use crate::capabilities::VcsCapabilities;

/// Which VCS backend produced a snapshot or owns a repository.
///
/// Stable surface — mirrors `carryctx_core::domain::git_snapshot::VcsBackend`
/// at the VCS layer so the crate boundary doesn't force callers to reach
/// through core for the kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    Git,
    Jj,
}

impl BackendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::Jj => "jj",
        }
    }
}

impl std::fmt::Display for BackendKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Request to materialize an isolated workspace (git worktree / jj workspace).
#[derive(Debug, Clone)]
pub struct WorkspaceRequest {
    /// Filesystem path where the workspace should be created.
    pub path: PathBuf,
    /// Desired branch/workspace name.
    pub branch: String,
    /// Optional base revision/branch to branch from.
    pub base: Option<String>,
}

/// Result of a successful workspace creation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Workspace {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub head: Option<String>,
}

/// Capability-aware VCS backend trait.
///
/// This is the sole abstraction boundary between CarryCtx and any VCS.
/// `GitBackend` is the Tier 1 implementation; `JjBackend` is the optional
/// runtime backend reached only through `Command::new("jj")` passthrough.
/// See `recording/research/002.md` §3 and `design/002-workspace-crates.md` §2.3.
pub trait VcsBackend: Send + Sync {
    fn kind(&self) -> BackendKind;

    fn repository_root(
        &self,
        start_path: &std::path::Path,
    ) -> Result<PathBuf, carryctx_core::error::CarryCtxError>;
    fn git_common_dir(
        &self,
        start_path: &std::path::Path,
    ) -> Result<PathBuf, carryctx_core::error::CarryCtxError>;
    fn head(
        &self,
        cwd: &std::path::Path,
    ) -> Result<Option<String>, carryctx_core::error::CarryCtxError>;
    fn status(
        &self,
        cwd: &std::path::Path,
    ) -> Result<carryctx_core::domain::git_snapshot::GitSnapshot, carryctx_core::error::CarryCtxError>;
    fn create_workspace(
        &self,
        repo_root: &std::path::Path,
        request: WorkspaceRequest,
    ) -> Result<Workspace, carryctx_core::error::CarryCtxError>;
    fn remove_workspace(
        &self,
        repo_root: &std::path::Path,
        path: &std::path::Path,
        force: bool,
    ) -> Result<(), carryctx_core::error::CarryCtxError>;
    fn capabilities(&self) -> VcsCapabilities;
}
