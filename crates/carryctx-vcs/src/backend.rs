//! VcsBackend trait + supporting types (domain-owned, crate-agnostic).

use std::path::PathBuf;

use crate::capabilities::VcsCapabilities;
use crate::snapshot::{SnapshotCommit, SnapshotRefCommit};

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

    /// Create one commit on `ref_name` whose tree holds `files` at the commit
    /// root, using Git plumbing only (`hash-object`/`mktree`/`commit-tree`/
    /// `update-ref` compare-and-swap): no index, worktree, or network access
    /// (design §3.1, CTX-0144).
    ///
    /// `parents` are the Git parent commit shas (empty for the first snapshot);
    /// the first parent is also the ref tip the compare-and-swap is checked
    /// against, so a concurrent ref move fails closed instead of clobbering.
    /// `export_id` and `source_label` are written into the commit-message
    /// trailers.
    ///
    /// Backends without [`VcsCapabilities::snapshot_ref`] return
    /// `UNSUPPORTED_OPERATION`.
    #[allow(clippy::too_many_arguments)]
    fn create_snapshot_commit(
        &self,
        _repo_root: &std::path::Path,
        _ref_name: &str,
        _files: &[(String, Vec<u8>)],
        _export_id: &str,
        _parents: &[String],
        _source_label: &str,
    ) -> Result<SnapshotCommit, carryctx_core::error::CarryCtxError> {
        Err(snapshot_ref_unsupported(self.capabilities()))
    }

    /// Read the tip `manifest.json` bytes and tip commit sha from `ref_name`.
    /// A missing ref returns `(empty, None)`; a ref whose tree has no
    /// `manifest.json` is a `GIT_ERROR`. Backends without the capability
    /// return `UNSUPPORTED_OPERATION`.
    fn read_snapshot_manifest(
        &self,
        _repo_root: &std::path::Path,
        _ref_name: &str,
    ) -> Result<(Vec<u8>, Option<String>), carryctx_core::error::CarryCtxError> {
        Err(snapshot_ref_unsupported(self.capabilities()))
    }

    /// Read one file from `revision`'s tree (`git show <rev>:<file>`), or
    /// `None` when the path is absent. Used to materialize a snapshot ref into
    /// a bundle directory without touching any index or worktree.
    fn read_snapshot_file(
        &self,
        _repo_root: &std::path::Path,
        _revision: &str,
        _file: &str,
    ) -> Result<Option<Vec<u8>>, carryctx_core::error::CarryCtxError> {
        Err(snapshot_ref_unsupported(self.capabilities()))
    }

    /// Walk the commit history reachable from `ref_name`, newest first, and
    /// parse each commit's CarryCtx trailers into a [`SnapshotRefCommit`]
    /// (design §3.1). Commits without an export-id trailer are skipped. A
    /// missing ref returns an empty history.
    fn snapshot_history(
        &self,
        _repo_root: &std::path::Path,
        _ref_name: &str,
    ) -> Result<Vec<SnapshotRefCommit>, carryctx_core::error::CarryCtxError> {
        Err(snapshot_ref_unsupported(self.capabilities()))
    }

    /// Whether `revision` resolves to a commit in this repository.
    fn revision_exists(
        &self,
        _repo_root: &std::path::Path,
        _revision: &str,
    ) -> Result<bool, carryctx_core::error::CarryCtxError> {
        Err(snapshot_ref_unsupported(self.capabilities()))
    }
}

/// The refusal returned by snapshot-ref trait default methods for a backend
/// whose [`VcsCapabilities::snapshot_ref`] is false (jj).
fn snapshot_ref_unsupported(capabilities: VcsCapabilities) -> carryctx_core::error::CarryCtxError {
    let _ = capabilities;
    carryctx_core::error::CarryCtxError::unsupported_operation(
        "This VCS backend does not support local snapshot refs; `export --snapshot` and `import --from-git` require the Git backend.",
    )
}
