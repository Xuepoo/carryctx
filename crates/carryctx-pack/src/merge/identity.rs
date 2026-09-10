//! Row identity, canonicalization, and table classification for the pure
//! merge engine (design `2026-09-10-mergeable-git-managed-state.md` §1.1,
//! §1.2, §1.7, §2.3).
//!
//! Everything here is pure: it operates on in-memory `serde_json` objects and
//! computes canonical strings. Identity semantics must match the storage
//! side's tombstone convention
//! ([`carryctx_core::repository::tombstone::canonical_composite_row_id`]) so a
//! tombstone's `row_id` and the merge engine's key agree byte-for-byte.

use carryctx_core::error::CarryCtxError;
use carryctx_core::repository::tombstone::canonical_composite_row_id;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Append-only, immutable-by-construction tables (design §1.4). Merged by
/// set union on `id`; a same-`id` content mismatch is a tamper signal.
pub const APPEND_ONLY_TABLES: &[&str] = &["events", "checkpoints", "checkpoint_corrections"];

/// Immutable-after-create tables (design §1.2). They carry no `updated_at`;
/// identical keys union, differing content is a blocking `immutable_edit`.
pub const IMMUTABLE_TABLES: &[&str] = &["task_dependencies", "scopes", "graph_edges"];

/// Tables whose `display_id` is a unique allocator artifact, not identity
/// (design §1.1). A cross-side collision is auto-renumbered.
pub const DISPLAY_TABLES: &[&str] = &["tasks", "progress_items", "decisions", "handoffs"];

/// Monotonic fact columns that survive last-writer-wins as a NULL-union
/// (design §1.2): setting a fact is never lost to a NULL.
pub const MONOTONIC_COLUMNS: &[&str] = &[
    "started_at",
    "completed_at",
    "accepted_at",
    "declined_at",
    "ended_at",
    "superseded_by",
];

/// True for the append-only audit/history tables.
pub fn is_append_only(table: &str) -> bool {
    APPEND_ONLY_TABLES.contains(&table)
}

/// True for the immutable-after-create tables.
pub fn is_immutable(table: &str) -> bool {
    IMMUTABLE_TABLES.contains(&table)
}

/// True for tables with a display-id allocator artifact.
pub fn is_display_table(table: &str) -> bool {
    DISPLAY_TABLES.contains(&table)
}

/// Machine-local columns excluded from change detection and never overwritten
/// from the incoming side (design §1.7). `projects` is a single-row table and
/// its anchors are re-anchored by the application layer; the merge engine only
/// guarantees it never adopts the incoming values.
pub fn machine_local_columns(table: &str) -> &'static [&'static str] {
    match table {
        "projects" => &["repository_root", "git_common_dir"],
        "worktrees" => &["normalized_path", "git_common_dir"],
        "sessions" => &["working_directory"],
        _ => &[],
    }
}

/// The identity columns of a table, in canonical order. Tables with a single
/// ULID primary key return `["id"]`; composite-key tables return their
/// components. This matches the design §1.1 table.
pub fn identity_columns(table: &str) -> &'static [&'static str] {
    match table {
        "team_members" => &["project_id", "team_id", "agent_id"],
        "graph_edges" => &["source_id", "target_id", "relation_type"],
        "sequences" => &["project_id", "kind"],
        "tombstones" => &["project_id", "table_name", "row_id"],
        _ => &["id"],
    }
}

/// The semantic (non-primary) key of a table, if it has one, used for
/// collision and immutability checks (design §1.1, §2.3).
///
/// `task_dependencies` carries `(task_id, prerequisite_task_id)` and `scopes`
/// carries `(task_id, pattern)` in addition to their `id`.
pub fn semantic_key_columns(table: &str) -> &'static [&'static str] {
    match table {
        "task_dependencies" => &["task_id", "prerequisite_task_id"],
        "scopes" => &["task_id", "pattern"],
        _ => &[],
    }
}

fn key_component(
    row: &Map<String, Value>,
    column: &str,
    table: &str,
) -> Result<String, CarryCtxError> {
    let value = row.get(column).ok_or_else(|| {
        CarryCtxError::validation_error(format!(
            "Merge row for table '{table}' is missing identity column '{column}'."
        ))
    })?;
    match value {
        Value::String(s) if !s.is_empty() => Ok(s.clone()),
        Value::String(_) => Err(CarryCtxError::validation_error(format!(
            "Merge row for table '{table}' has an empty identity column '{column}'."
        ))),
        other => Ok(other.to_string()),
    }
}

/// The canonical identity key of a row as a string. Single-column keys are the
/// bare value; composite keys are a compact JSON array in column order (the
/// [`canonical_composite_row_id`] convention).
pub fn identity_key(table: &str, row: &Map<String, Value>) -> Result<String, CarryCtxError> {
    let columns = identity_columns(table);
    let mut parts = Vec::with_capacity(columns.len());
    for column in columns {
        parts.push(key_component(row, column, table)?);
    }
    if parts.len() == 1 {
        Ok(parts.pop().expect("one part"))
    } else {
        let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
        Ok(canonical_composite_row_id(&refs))
    }
}

/// The canonical semantic key of a row, or `None` when the table has no
/// semantic key. Used to detect two different ULIDs describing the same
/// logical dependency/scope.
pub fn semantic_key(
    table: &str,
    row: &Map<String, Value>,
) -> Result<Option<String>, CarryCtxError> {
    let columns = semantic_key_columns(table);
    if columns.is_empty() {
        return Ok(None);
    }
    let mut parts = Vec::with_capacity(columns.len());
    for column in columns {
        parts.push(key_component(row, column, table)?);
    }
    let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
    Ok(Some(canonical_composite_row_id(&refs)))
}

/// The canonical content frame of a row: machine-local and identity columns
/// removed, keys sorted. Two rows with the same identity compare equal iff
/// their frames are equal.
pub fn frame(table: &str, row: &Map<String, Value>) -> BTreeMap<String, Value> {
    let machine_local = machine_local_columns(table);
    let identity = identity_columns(table);
    row.iter()
        .filter(|(key, _)| {
            !machine_local.contains(&key.as_str()) && !identity.contains(&key.as_str())
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// The canonical JSON string of a row's content frame (stable, sorted keys).
pub fn canonical_frame(table: &str, row: &Map<String, Value>) -> String {
    serde_json::to_string(&frame(table, row)).unwrap_or_default()
}

/// Whether two rows of `table` carry identical content once machine-local and
/// identity columns are removed.
pub fn content_eq(table: &str, a: &Map<String, Value>, b: &Map<String, Value>) -> bool {
    frame(table, a) == frame(table, b)
}

/// The display id of a row, if present and a non-empty string.
pub fn display_id(row: &Map<String, Value>) -> Option<&str> {
    row.get("display_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

/// A parsed display id: `(prefix, number, zero-pad width)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayParts {
    pub prefix: String,
    pub number: u64,
    pub width: usize,
}

/// Split `PREFIX-NNNN` into its prefix, number, and observed digit width.
pub fn split_display_id(display_id: &str) -> Option<DisplayParts> {
    let dash = display_id.rfind('-')?;
    let prefix = display_id[..dash].trim();
    let digits = display_id[dash + 1..].trim();
    if prefix.is_empty() || digits.is_empty() {
        return None;
    }
    let number: u64 = digits.parse().ok()?;
    Some(DisplayParts {
        prefix: prefix.to_string(),
        number,
        width: digits.len(),
    })
}

/// The `sequences.kind` a display id of `table` belongs to, plus the display
/// prefix and zero-pad width to re-emit it. Mirrors `reconcile_sequences` in
/// the import path: tasks key by their prefix (`display_id_CTX`), the other
/// display tables use a fixed kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayKind {
    pub kind: String,
    pub prefix: String,
    pub width: usize,
}

/// Resolve the sequence kind and render prefix for a table's display id.
pub fn display_kind(table: &str, display_id: &str) -> Option<DisplayKind> {
    let parts = split_display_id(display_id)?;
    let kind = match table {
        "tasks" => format!("display_id_{}", parts.prefix),
        "progress_items" => "display_id_progress".to_string(),
        "decisions" => "display_id_decision".to_string(),
        "handoffs" => "display_id_handoff".to_string(),
        _ => return None,
    };
    Some(DisplayKind {
        kind,
        prefix: parts.prefix,
        width: parts.width.max(1),
    })
}

/// Render a display id from its prefix and numeric value, preserving the
/// observed zero-pad width.
pub fn render_display_id(prefix: &str, value: u64, width: usize) -> String {
    format!("{prefix}-{value:0width$}", width = width.max(1))
}
