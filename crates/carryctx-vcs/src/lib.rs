//! carryctx-vcs — VCS abstraction crate (P3).
//!
//! Owns `VcsBackend` + `VcsCapabilities`, `GitBackend` (Tier 1) and
//! `JjBackend` (optional runtime backend), detection/auto selection, and
//! capability-aware guards. Depends only on `carryctx-core` (pure) — no
//! `rusqlite`, no `clap`, no network. See `recording/research/002.md` §3
//! and `design/002-workspace-crates.md` §2.3-2.4.

pub mod backend;
pub mod capabilities;
pub mod git;
pub mod jj;
pub mod snapshot;
pub mod xdg;

pub use backend::{BackendKind, VcsBackend, Workspace, WorkspaceRequest};
pub use capabilities::VcsCapabilities;

pub use git::{
    GitBackend, GitCli, GitProject, WorktreeEntry, detect_jj_colocation, isolate_git_env,
};
pub use jj::JjBackend;
pub use snapshot::{
    EXPORT_ID_TRAILER, PARENTS_TRAILER, SNAPSHOT_MANIFEST_FILE, SNAPSHOT_REF_DEFAULT,
    SOURCE_TRAILER, SnapshotCommit, SnapshotRefCommit, SnapshotTrailers, render_snapshot_message,
};

/// Auto-select the VCS backend for a repository (no Cargo feature matrix).
///
/// Rule from `recording/research/002.md` §3: if `.jj/` exists alongside
/// `.git/` then `JjBackend`, otherwise `GitBackend`. Returns the selected
/// kind without constructing the backend (callers may instantiate either).
pub fn auto_backend_kind(git_common_dir: &std::path::Path) -> BackendKind {
    if detect_jj_colocation(git_common_dir) {
        BackendKind::Jj
    } else {
        BackendKind::Git
    }
}
