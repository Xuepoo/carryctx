//! VcsCapabilities — capability matrix that replaces scattered `if jj {}` branches.

/// Capabilities exposed by a VCS backend.
///
/// Carries the load-bearing dispatch for `carryctx-vcs`: call sites branch
/// on `capabilities()` instead of `detect_jj_colocation()` scattered through
/// the codebase. Values follow `recording/research/002.md` §3 and
/// `design/002-workspace-crates.md` §2.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VcsCapabilities {
    /// Whether this backend can create isolated workspaces (git worktree / jj workspace).
    pub workspaces: bool,
    /// Whether `post-commit` / `prepare-commit-msg` hooks fire under this backend.
    pub commit_hooks: bool,
    /// Whether `git add` staging area semantics are meaningful.
    pub staging_area: bool,
    /// Whether changes are mutable rewritable revisions (jj) rather than
    /// immutable commits (git).
    pub mutable_changes: bool,
    /// Whether this backend can own the `carryctx-snapshots` ref: create
    /// commit-per-snapshot objects from the ctxpack directory and read a
    /// ref's export-id history with Git plumbing (design §3.1, CTX-0144).
    ///
    /// Git is `true`. jj is `false`: the jj backend is a thin `jj` CLI
    /// passthrough that does not own Git ref plumbing, and in colocated mode
    /// jj's automatic working-copy snapshotting mutates the Git index as a
    /// side effect of read-only commands — committing arbitrary local refs
    /// through that working copy is not a contract this backend can keep.
    /// Snapshot ref I/O therefore stays Git-only and is dispatched on this
    /// capability, not on `detect_jj_colocation`.
    pub snapshot_ref: bool,
}

impl VcsCapabilities {
    /// Git Tier 1 capabilities (stable, full).
    pub const fn git() -> Self {
        Self {
            workspaces: true,
            commit_hooks: true,
            staging_area: true,
            mutable_changes: false,
            snapshot_ref: true,
        }
    }

    /// jj colocated Tier 2 capabilities (experimental).
    pub const fn jj() -> Self {
        Self {
            workspaces: true,
            commit_hooks: false,
            staging_area: false,
            mutable_changes: true,
            snapshot_ref: false,
        }
    }
}
