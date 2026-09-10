//! Tombstones for hard-deleted rows (design
//! `mergeable-git-managed-state.md` §1.3): a side table that records
//! `(table_name, row_id)` deletions so a three-way merge can tell "deleted on
//! this side" from "never seen".
//!
//! Delete paths append one tombstone per removed row in the same transaction
//! as the delete (audit-atomicity). Duplicate records for one key keep the
//! earliest `deleted_at`, matching the merge union rule. A tombstone never
//! changes ordinary read queries: it is a side table only.

use crate::error::CarryCtxError;

/// Table names stored in `tombstones.table_name`. Shared by delete paths,
/// `doctor`, and the merge/export readers so the strings cannot drift.
pub mod tables {
    pub const CHECKPOINT_CORRECTIONS: &str = "checkpoint_corrections";
    pub const CHECKPOINTS: &str = "checkpoints";
    pub const DECISIONS: &str = "decisions";
    pub const HANDOFFS: &str = "handoffs";
    pub const PROGRESS_ITEMS: &str = "progress_items";
    pub const SCOPES: &str = "scopes";
    pub const TASK_DEPENDENCIES: &str = "task_dependencies";
    pub const TASKS: &str = "tasks";
    pub const TEAM_MEMBERS: &str = "team_members";
    pub const WORKTREES: &str = "worktrees";
}

/// Canonical `row_id` for a row whose primary key is composite.
///
/// JSON array encoding keeps the components unambiguous (no separator can
/// collide with a ULID or a user-supplied pattern) and is stable across
/// machines. `project_id` is carried by the tombstone column itself and is
/// not repeated in the key.
pub fn canonical_composite_row_id(parts: &[&str]) -> String {
    serde_json::to_string(parts).expect("a slice of strings always serializes")
}

/// One tombstone row: an input for [`TombstoneRepository::record`] and the
/// shape read back by `find`/`list_for_project`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tombstone {
    pub project_id: String,
    pub table_name: String,
    pub row_id: String,
    pub deleted_at: String,
    pub deleted_by: Option<String>,
    pub reason: Option<String>,
}

pub trait TombstoneRepository {
    /// Append one tombstone. Idempotent per `(project_id, table_name,
    /// row_id)`: the earliest `deleted_at` wins (design §1.3).
    fn record(&self, tombstone: &Tombstone) -> Result<(), CarryCtxError>;

    /// Append many tombstones in order, for bulk paths such as `project prune`.
    fn record_many(&self, tombstones: &[Tombstone]) -> Result<(), CarryCtxError>;

    /// Look up one tombstone by its identity key.
    fn find(
        &self,
        project_id: &str,
        table_name: &str,
        row_id: &str,
    ) -> Result<Option<Tombstone>, CarryCtxError>;

    /// List every tombstone of a project, ordered by `deleted_at`, then
    /// `table_name`, then `row_id`.
    fn list_for_project(&self, project_id: &str) -> Result<Vec<Tombstone>, CarryCtxError>;

    /// Count the tombstones of a project (doctor reporting, merge summaries).
    fn count_for_project(&self, project_id: &str) -> Result<usize, CarryCtxError>;
}

#[cfg(test)]
mod tests {
    use super::canonical_composite_row_id;

    #[test]
    fn composite_row_id_is_a_json_array_of_the_parts() {
        assert_eq!(
            canonical_composite_row_id(&["team-1", "agent-1"]),
            r#"["team-1","agent-1"]"#
        );
    }
}
