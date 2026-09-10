// P3 thin bridge: VCS owner moved to `carryctx-vcs`.
// Keep every `crate::adapter::git::*` import valid while the VCS owner
// lives in the `carryctx-vcs` crate.
pub use carryctx_vcs::{
    BackendKind, GitBackend, GitCli, GitProject, LOCAL_SNAPSHOT_REF_PREFIX, PUBLIC_SNAPSHOT_REF,
    SNAPSHOT_REF_DEFAULT, SnapshotCommit, SnapshotRefCommit, SnapshotTrailers, VcsBackend,
    WorktreeEntry, auto_backend_kind, detect_jj_colocation, isolate_git_env,
};
