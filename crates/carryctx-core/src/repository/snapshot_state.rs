//! Local-only key/value bookkeeping for the snapshot and merge flow (design
//! `mergeable-git-managed-state.md` §2.1 and §3.2). `snapshot_state` records
//! where this clone last exported or snapshotted so `import --mode merge` can
//! resolve a base. It is machine-local: never exported, never part of any
//! ctxpack bundle.

use crate::error::CarryCtxError;

/// Key for the newest export id this clone produced or recorded.
pub const LAST_EXPORT_ID: &str = "last_export_id";

/// Key for the newest `carryctx-snapshots` commit this clone wrote.
pub const LAST_SNAPSHOT_COMMIT: &str = "last_snapshot_commit";

pub trait SnapshotStateRepository {
    /// Read one local snapshot value, if present.
    fn get(&self, project_id: &str, key: &str) -> Result<Option<String>, CarryCtxError>;

    /// Upsert one local snapshot value.
    fn set(&self, project_id: &str, key: &str, value: &str, now: &str)
    -> Result<(), CarryCtxError>;
}
