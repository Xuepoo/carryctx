//! Pure merge plan types (design `2026-09-10-mergeable-git-managed-state.md`
//! §2.2–§2.3).
//!
//! These are the data structures the merge engine produces. They are pure
//! Rust/serde values: no SQLite, Git, CLI, filesystem, or network access.
//! Every vector in [`MergeReport`] is sorted by a stable key so a caller can
//! render or persist the plan deterministically.

use serde_json::Value;

/// A complete row set for every table of one state snapshot: table name
/// (`snake_case`, matching `PACK_TABLE_FILES`) to its JSON rows. The map is a
/// `BTreeMap`, so table iteration order is canonical.
pub type TableSet = std::collections::BTreeMap<String, Vec<Value>>;

/// Input knobs for [`crate::merge::merge_tables`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MergeOptions {
    /// Promote every last-writer-wins `row_edit` election where both sides
    /// changed into a blocking `row_edit` conflict (design §1.2, §2.3).
    pub strict_edits: bool,
}

/// Whether a [`RowWrite`] inserts a row `ours` never had or updates one it
/// already carried.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum WriteKind {
    Insert,
    Update,
}

/// One row the candidate database must insert or update, relative to `ours`.
#[derive(Debug, Clone, PartialEq)]
pub struct RowWrite {
    pub table: String,
    /// Canonical identity key (see `merge::identity`).
    pub key: String,
    pub row: Value,
    pub kind: WriteKind,
}

/// One row `ours` carried that the merged result removes, identified by table
/// and canonical identity key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowDelete {
    pub table: String,
    pub key: String,
}

/// A display id collision resolved by renumbering a row's allocator artifact
/// (display ids are not identity; ULIDs survive — design §1.1, §2.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Renumber {
    pub table: String,
    /// ULID identity of the renumbered row.
    pub row_id: String,
    /// The display id that collided.
    pub display_id: String,
    /// The newly allocated, monotonically increasing display id.
    pub new_display_id: String,
    pub reason: String,
}

/// One foreign-key cell rewritten during agent aliasing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceRemap {
    pub table: String,
    /// Canonical identity key of the row carrying the reference.
    pub row_id: String,
    pub column: String,
}

/// An agent-name collision resolved by aliasing the incoming agent to the
/// local one and remapping its references (design §2.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentAlias {
    pub project_id: String,
    pub name: String,
    /// The local agent id that survives.
    pub existing_agent_id: String,
    /// The incoming agent id that is folded into `existing_agent_id`.
    pub incoming_agent_id: String,
    pub remapped_references: Vec<ReferenceRemap>,
}

/// A recorded, non-silent automatic resolution (design §1.2). The `winner`
/// digest is order-independent: it is the SHA-256 of the elected row's
/// canonical JSON, so merging `(A, B)` or `(B, A)` records the same value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoResolution {
    pub table: String,
    pub key: String,
    /// `row_edit`, `status_terminal`, or `dependency_kind`.
    pub kind: String,
    pub winner: String,
    pub reason: String,
}

/// A blocking conflict: merge cannot apply until a human resolves it. The
/// engine leaves the conflicted key at `ours`.
#[derive(Debug, Clone, PartialEq)]
pub struct Conflict {
    /// Stable, order-independent identifier derived from kind/table/key.
    pub id: String,
    /// `row_edit`, `status_gap`, `delete_vs_edit`, `unique_key`, or
    /// `immutable_edit`.
    pub kind: String,
    pub table: String,
    /// Canonical identity key.
    pub key: String,
    pub base: Option<Value>,
    pub ours: Option<Value>,
    pub theirs: Option<Value>,
    pub reason: String,
}

/// The full pure merge plan.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MergeReport {
    /// The complete merged candidate row set (all tables, including
    /// `tombstones` and `sequences`).
    pub result: TableSet,
    pub writes: Vec<RowWrite>,
    pub deletes: Vec<RowDelete>,
    pub renumbers: Vec<Renumber>,
    pub aliases: Vec<AgentAlias>,
    pub auto_resolutions: Vec<AutoResolution>,
    pub conflicts: Vec<Conflict>,
    pub warnings: Vec<String>,
    /// True when no merge base was available and the engine ran a degraded
    /// two-way merge (design §2.1).
    pub degraded: bool,
}

impl MergeReport {
    /// Whether any blocking conflict remains open.
    pub fn has_conflicts(&self) -> bool {
        !self.conflicts.is_empty()
    }
}
