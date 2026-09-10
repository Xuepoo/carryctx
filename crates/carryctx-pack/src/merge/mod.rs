//! Pure three-way merge engine (design `2026-09-10-mergeable-git-managed-state.md`).
//!
//! This module implements the pure half of the merge milestone: identity and
//! delete semantics (§1.1–§1.3), append-only union and sequence floors
//! (§1.4–§1.5), machine-local column policy (§1.7), and the per-key policy
//! table that turns `base`/`ours`/`theirs` row sets into a
//! [`plan::MergeReport`] (§2.2–§2.3). It performs **no** I/O: no SQLite, no
//! Git, no CLI, no filesystem, no network. The base is supplied as an explicit
//! row set; [`dag::resolve_base`] resolves it from the export-id DAG (or a
//! caller-supplied snapshot cache), and passing `None` (no common ancestor)
//! produces a degraded two-way merge (`degraded = true`). [`MergeRequest`] is
//! the additive wrapper that carries the resolved base plus its
//! [`BaseSource`].
//!
//! ## Determinism
//!
//! The semantic core is commutative and idempotent: the set of elected rows
//! (ignoring machine-local columns, which are intentionally never merged),
//! the conflict identities, the auto-resolution winners, and the display/
//! agent/unique-key survivors do not depend on which side is `ours`. Every
//! output vector is sorted by a stable key. The survivors are chosen by an
//! order-independent canonical key (minimum identity key / minimum ULID), not
//! by side. `writes`, `deletes`, `renumbers`, and `aliases` are necessarily
//! expressed relative to `ours`, so they are deterministic for a fixed
//! orientation but not symmetric under a swap; the invariant the callers rely
//! on is that applying `writes`/`deletes` to `ours` reconstructs `result`.
//!
//! Identity-collision precedence (design §2.3): an agent-name collision is the
//! special-cased auto-alias (every non-survivor agent folds into the canonical
//! minimum ULID and its references are remapped); a team-name collision is a
//! blocking `unique_key` conflict keyed by the semantic unique key. Display-id
//! collisions are auto-renumbered, and the collision survivor is chosen by
//! canonical identity key so the merged rows stay commutative.
//!
//! ## Scope and known limits
//!
//! - **One-sided fast path (finding 6).** When only one side changed a row and
//!   the other matches the base, the changed row is taken whole; the NULL-union
//!   monotonic-fact merge and the task terminal-status lattice are applied only
//!   when both sides changed. This is accepted scope: one-sided edits cannot
//!   lose a *concurrent* fact, because the unchanged side carries the base
//!   value forward. LWW elections, NULL-union, and the status lattice all run
//!   on the both-changed path.
//! - **Dangling worktree references (finding 8).** This engine can prune an
//!   agent row via aliasing and can drop a `unique_key`/display-collision row,
//!   but it does not null or rebind dangling `sessions.worktree_id` /
//!   `sessions.agent_id` / `tasks.owner_agent_id` references. Reference
//!   reconciliation for pruned worktrees belongs to CTX-0142's candidate
//!   builder (design §1.7), which inserts in `LOAD_ORDER` and can null or
//!   remap references before insert; the merge report exposes the affected keys
//!   (`deletes`, `aliases`) for that step. It is intentionally not implemented
//!   here.

pub mod dag;
pub mod identity;
pub mod plan;

#[cfg(test)]
mod tests;

pub use dag::{
    BaseResolution, BaseSource, ExportDag, SnapshotNode, required_base_error, resolve_base,
    select_merge_base,
};
pub use plan::{
    AgentAlias, AutoResolution, Conflict, MergeOptions, MergeReport, ReferenceRemap, Renumber,
    RowDelete, RowWrite, TableSet, WriteKind,
};

use self::identity::{
    canonical_frame, content_eq, display_id, display_kind, frame, identity_key, is_append_only,
    is_immutable, machine_local_columns, render_display_id, split_display_id,
};
use carryctx_core::error::CarryCtxError;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Foreign-key columns remapped when an incoming agent is aliased to a local
/// one (design §2.3). A column absent from a row is skipped, which keeps the
/// engine robust to schema evolution (for example `events.actor_agent_id`).
const AGENT_REFERENCE_COLUMNS: &[(&str, &str)] = &[
    ("sessions", "agent_id"),
    ("tasks", "owner_agent_id"),
    ("handoffs", "from_agent_id"),
    ("handoffs", "to_agent_id"),
    ("events", "actor_agent_id"),
    ("teams", "commander_agent_id"),
    // CTX-0146: the composite team-member key ends in the agent id, so an
    // aliased incoming agent must be remapped here too or the candidate
    // insert fails the `team_members.agent_id` FK (and the
    // `teams.commander_agent_id` composite FK it feeds).
    ("team_members", "agent_id"),
];

type Row = Map<String, Value>;
type RowIndex = BTreeMap<String, Row>;
type TombstoneIndex = BTreeMap<(String, String), Row>;

/// Merge three in-memory row sets with the design's row/delete policies.
///
/// `base` is the explicit merge base; `None` runs a degraded two-way merge.
/// Append-only tamper returns `VALIDATION_FAILED` and no report (fail closed,
/// no partial output). A directly supplied base is reported as
/// [`BaseSource::Explicit`]; callers that resolved the base from the DAG pass
/// the real source through [`merge_tables_with_source`].
pub fn merge_tables(
    base: Option<&TableSet>,
    ours: &TableSet,
    theirs: &TableSet,
    options: &MergeOptions,
) -> Result<MergeReport, CarryCtxError> {
    merge_tables_with_source(base, ours, theirs, options, None)
}

/// [`merge_tables`] with an explicit [`BaseSource`] for the resolved base
/// (design §2.1). `None` derives the source from the base: `Explicit` when a
/// base row set was supplied, `None` when it was not. [`MergeRequest::run`]
/// uses this so an ancestor/snapshot base is labelled accurately instead of
/// being reported as `Explicit`.
pub fn merge_tables_with_source(
    base: Option<&TableSet>,
    ours: &TableSet,
    theirs: &TableSet,
    options: &MergeOptions,
    base_source: Option<BaseSource>,
) -> Result<MergeReport, CarryCtxError> {
    let mut builder = Builder {
        options,
        conflicts: Vec::new(),
        auto_resolutions: Vec::new(),
        warnings: Vec::new(),
    };

    if options.require_base && base.is_none() {
        return Err(required_base_error());
    }

    if base.is_none() {
        builder.warnings.push(
            "No merge base is available; running a degraded two-way merge. Tombstones and structural keys still detect deletes, but three-way edit classification is unavailable.".to_string(),
        );
    }

    let mut table_names: BTreeSet<String> = BTreeSet::new();
    for tables in [base, Some(ours), Some(theirs)].into_iter().flatten() {
        table_names.extend(tables.keys().cloned());
    }

    let base_tombstones = tombstone_index(base)?;
    let our_tombstones = tombstone_index(Some(ours))?;
    let their_tombstones = tombstone_index(Some(theirs))?;

    let mut result: TableSet = TableSet::new();
    for table in &table_names {
        if table == "tombstones" || table == "sequences" {
            continue;
        }
        let rows = merge_table(
            table,
            base,
            ours,
            theirs,
            &base_tombstones,
            &our_tombstones,
            &their_tombstones,
            &mut builder,
        )?;
        result.insert(table.clone(), rows);
    }

    let merged_sequences = merge_sequences(base, ours, theirs)?;
    if table_names.contains("sequences") || !merged_sequences.is_empty() {
        result.insert("sequences".to_string(), merged_sequences);
    }
    let mut tombstone_rows = merge_tombstones(base, ours, theirs)?;

    let mut aliases = plan_agent_aliases(ours, theirs, &mut builder)?;
    remap_agent_references(&mut result, &mut aliases);
    if !aliases.is_empty() {
        for alias in &aliases {
            builder.warnings.push(format!(
                "Agent name '{}' collides in project '{}'; aliasing incoming agent '{}' to '{}'.",
                alias.name, alias.project_id, alias.incoming_agent_id, alias.existing_agent_id
            ));
        }
    }

    resolve_unique_collisions(
        "teams",
        &["project_id", "name"],
        ours,
        theirs,
        &mut result,
        &mut builder,
    )?;
    resolve_unique_collisions(
        "worktrees",
        &["project_id", "normalized_path"],
        ours,
        theirs,
        &mut result,
        &mut builder,
    )?;
    resolve_dependency_kind_collisions(&mut result, &mut builder)?;
    resolve_unique_collisions(
        "task_dependencies",
        &["task_id", "prerequisite_task_id"],
        ours,
        theirs,
        &mut result,
        &mut builder,
    )?;
    resolve_unique_collisions(
        "scopes",
        &["task_id", "pattern"],
        ours,
        theirs,
        &mut result,
        &mut builder,
    )?;

    stabilize_projects(&mut result, ours, theirs, &mut builder)?;

    let renumbers = plan_display_renumbers(&mut result, &mut builder.warnings);
    filter_tombstones(&mut tombstone_rows, &result);
    if table_names.contains("tombstones") || !tombstone_rows.is_empty() {
        result.insert("tombstones".to_string(), tombstone_rows);
    }

    for (table, rows) in result.iter_mut() {
        sort_rows_by_key(table, rows);
    }

    let (writes, deletes) = compute_writes_deletes(ours, &result)?;

    builder.conflicts.sort_by(conflict_order);
    builder.conflicts.dedup();
    builder.auto_resolutions.sort_by(auto_resolution_order);
    builder.auto_resolutions.dedup();
    builder.warnings.sort();
    builder.warnings.dedup();

    Ok(MergeReport {
        result,
        writes,
        deletes,
        renumbers,
        aliases,
        auto_resolutions: builder.auto_resolutions,
        conflicts: builder.conflicts,
        warnings: builder.warnings,
        base_source: base_source.unwrap_or(if base.is_none() {
            BaseSource::None
        } else {
            BaseSource::Explicit
        }),
        degraded: base.is_none(),
    })
}

/// Additive façade over [`merge_tables`] that carries a resolved base and its
/// [`BaseSource`] so the [`MergeReport`] can state how the base was acquired
/// (design §2.1). The four-argument [`merge_tables`] entry point is unchanged.
#[derive(Debug)]
pub struct MergeRequest<'a> {
    /// The resolved merge base row set, or `None` for a degraded two-way merge.
    pub base: Option<&'a TableSet>,
    /// Where `base` came from (see [`dag::resolve_base`]).
    pub base_source: BaseSource,
    pub ours: &'a TableSet,
    pub theirs: &'a TableSet,
    pub options: MergeOptions,
}

impl<'a> MergeRequest<'a> {
    /// A base-less request; the caller can add a base with [`Self::with_base`].
    pub fn new(ours: &'a TableSet, theirs: &'a TableSet) -> Self {
        Self {
            base: None,
            base_source: BaseSource::None,
            ours,
            theirs,
            options: MergeOptions::default(),
        }
    }

    pub fn with_options(mut self, options: MergeOptions) -> Self {
        self.options = options;
        self
    }

    pub fn with_base(mut self, base: &'a TableSet, source: BaseSource) -> Self {
        self.base = Some(base);
        self.base_source = source;
        self
    }

    /// Build a request from a [`BaseResolution`]. A
    /// [`BaseResolution::RequiredMissing`] becomes a `VALIDATION_FAILED`
    /// (exit 8) error via [`required_base_error`].
    pub fn from_resolution(
        resolution: &'a BaseResolution,
        ours: &'a TableSet,
        theirs: &'a TableSet,
        options: MergeOptions,
    ) -> Result<Self, CarryCtxError> {
        match resolution {
            BaseResolution::Resolved { base, source } => Ok(Self {
                base: Some(base),
                base_source: *source,
                ours,
                theirs,
                options,
            }),
            BaseResolution::Degraded => Ok(Self {
                base: None,
                base_source: BaseSource::None,
                ours,
                theirs,
                options,
            }),
            BaseResolution::RequiredMissing => Err(required_base_error()),
        }
    }

    /// Run the merge, propagating the resolved [`BaseSource`] into the report.
    pub fn run(&self) -> Result<MergeReport, CarryCtxError> {
        merge_tables_with_source(
            self.base,
            self.ours,
            self.theirs,
            &self.options,
            Some(self.base_source),
        )
    }
}

struct Builder<'a> {
    options: &'a MergeOptions,
    conflicts: Vec<Conflict>,
    auto_resolutions: Vec<AutoResolution>,
    warnings: Vec<String>,
}

impl Builder<'_> {
    #[allow(clippy::too_many_arguments)]
    fn push_conflict(
        &mut self,
        kind: &str,
        table: &str,
        key: &str,
        base: Option<&Row>,
        ours: Option<&Row>,
        theirs: Option<&Row>,
        reason: String,
    ) {
        self.conflicts.push(Conflict {
            id: conflict_id(kind, table, key),
            kind: kind.to_string(),
            table: table.to_string(),
            key: key.to_string(),
            base: base.map(|row| Value::Object(row.clone())),
            ours: ours.map(|row| Value::Object(row.clone())),
            theirs: theirs.map(|row| Value::Object(row.clone())),
            reason,
        });
    }
}

fn conflict_id(kind: &str, table: &str, key: &str) -> String {
    format!("{kind}:{table}:{key}")
}

fn conflict_order(a: &Conflict, b: &Conflict) -> std::cmp::Ordering {
    (&a.table, &a.key, &a.kind, &a.id).cmp(&(&b.table, &b.key, &b.kind, &b.id))
}

fn auto_resolution_order(a: &AutoResolution, b: &AutoResolution) -> std::cmp::Ordering {
    (&a.table, &a.key, &a.kind, &a.winner).cmp(&(&b.table, &b.key, &b.kind, &b.winner))
}

fn table_rows<'a>(tables: Option<&'a TableSet>, table: &str) -> &'a [Value] {
    tables
        .and_then(|set| set.get(table))
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn as_object<'a>(table: &str, value: &'a Value) -> Result<&'a Row, CarryCtxError> {
    value.as_object().ok_or_else(|| {
        CarryCtxError::validation_error(format!(
            "Merge row for table '{table}' must be a JSON object."
        ))
    })
}

/// Index rows by canonical identity key. Duplicate keys inside one side are a
/// malformed input; the canonical minimum is kept deterministically and a
/// warning is recorded rather than failing the merge.
fn index_rows(
    table: &str,
    rows: &[Value],
    warnings: &mut Vec<String>,
) -> Result<RowIndex, CarryCtxError> {
    let mut index = RowIndex::new();
    for row in rows {
        let map = as_object(table, row)?;
        let key = identity_key(table, map)?;
        match index.get_mut(&key) {
            None => {
                index.insert(key, map.clone());
            }
            Some(existing) => {
                let existing_canon = canonical_frame(table, existing);
                let candidate_canon = canonical_frame(table, map);
                if existing_canon != candidate_canon {
                    warnings.push(format!(
                        "Table '{table}' has duplicate identity key '{key}' with differing content; keeping the canonical minimum."
                    ));
                }
                if candidate_canon < existing_canon {
                    *existing = map.clone();
                }
            }
        }
    }
    Ok(index)
}

fn tombstone_index(tables: Option<&TableSet>) -> Result<TombstoneIndex, CarryCtxError> {
    let mut index = TombstoneIndex::new();
    for row in table_rows(tables, "tombstones") {
        let map = as_object("tombstones", row)?;
        let table_name = map
            .get("table_name")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                CarryCtxError::validation_error(
                    "Tombstone row is missing its 'table_name'.".to_string(),
                )
            })?;
        let row_id = map.get("row_id").and_then(Value::as_str).ok_or_else(|| {
            CarryCtxError::validation_error("Tombstone row is missing its 'row_id'.".to_string())
        })?;
        index.insert((table_name.to_string(), row_id.to_string()), map.clone());
    }
    Ok(index)
}

#[allow(clippy::too_many_arguments)]
fn merge_table(
    table: &str,
    base: Option<&TableSet>,
    ours: &TableSet,
    theirs: &TableSet,
    base_tombstones: &TombstoneIndex,
    our_tombstones: &TombstoneIndex,
    their_tombstones: &TombstoneIndex,
    builder: &mut Builder,
) -> Result<Vec<Value>, CarryCtxError> {
    if is_append_only(table) {
        return merge_append_only(table, base, ours, theirs);
    }

    let base_index = index_rows(table, table_rows(base, table), &mut builder.warnings)?;
    let our_index = index_rows(table, table_rows(Some(ours), table), &mut builder.warnings)?;
    let their_index = index_rows(
        table,
        table_rows(Some(theirs), table),
        &mut builder.warnings,
    )?;

    let mut keys: BTreeSet<String> = BTreeSet::new();
    keys.extend(base_index.keys().cloned());
    keys.extend(our_index.keys().cloned());
    keys.extend(their_index.keys().cloned());
    for (tomb_table, row_id) in base_tombstones
        .keys()
        .chain(our_tombstones.keys())
        .chain(their_tombstones.keys())
    {
        if tomb_table == table {
            keys.insert(row_id.clone());
        }
    }

    let mut out = Vec::new();
    for key in keys {
        let base_row = base_index.get(&key);
        let our_row = our_index.get(&key);
        let their_row = their_index.get(&key);
        let tombstone_key = (table.to_string(), key.clone());
        let our_deleted = our_tombstones.contains_key(&tombstone_key);
        let their_deleted = their_tombstones.contains_key(&tombstone_key);

        if our_deleted || their_deleted {
            if let Some(row) = resolve_delete_vs_edit(
                table,
                &key,
                base_row,
                our_row,
                their_row,
                our_deleted,
                their_deleted,
                builder,
            ) {
                out.push(Value::Object(row));
            }
            continue;
        }

        match (our_row, their_row) {
            (Some(our), Some(their)) if content_eq(table, our, their) => {
                out.push(Value::Object(our.clone()));
            }
            (Some(our), Some(their)) if is_immutable(table) => {
                if let Some(elected) = dependency_kind_election(table, our, their) {
                    builder.auto_resolutions.push(AutoResolution {
                        table: table.to_string(),
                        key: key.clone(),
                        kind: "dependency_kind".to_string(),
                        winner: row_digest(table, &elected),
                        reason: "Strong dependency kind wins over informational.".to_string(),
                    });
                    out.push(Value::Object(elected));
                } else {
                    builder.push_conflict(
                        "immutable_edit",
                        table,
                        &key,
                        base_row,
                        our_row,
                        their_row,
                        "Immutable table row has differing content on both sides.".to_string(),
                    );
                    out.push(Value::Object(our.clone()));
                }
            }
            (Some(our), Some(their)) => {
                let our_changed = changed(table, Some(our), base_row);
                let their_changed = changed(table, Some(their), base_row);
                if our_changed && their_changed {
                    out.push(builder.elect(table, &key, base_row, our, their));
                } else if our_changed {
                    out.push(Value::Object(our.clone()));
                } else {
                    let mut elected = their.clone();
                    overlay_machine_local(table, &mut elected, Some(our));
                    out.push(Value::Object(elected));
                }
            }
            (Some(our), None) => out.push(Value::Object(our.clone())),
            (None, Some(their)) => out.push(Value::Object(their.clone())),
            (None, None) => {}
        }
    }
    Ok(out)
}

/// Apply the design §1.3 delete/delete-vs-edit rules. Returns the row left at
/// `ours` when a blocking `delete_vs_edit` still keeps it (otherwise `None`).
#[allow(clippy::too_many_arguments)]
fn resolve_delete_vs_edit(
    table: &str,
    key: &str,
    base: Option<&Row>,
    ours: Option<&Row>,
    theirs: Option<&Row>,
    our_deleted: bool,
    their_deleted: bool,
    builder: &mut Builder,
) -> Option<Row> {
    if our_deleted && their_deleted {
        return None;
    }
    if our_deleted {
        if let Some(their) = theirs {
            let edited = base.is_none_or(|base| !content_eq(table, their, base));
            if edited {
                builder.push_conflict(
                    "delete_vs_edit",
                    table,
                    key,
                    base,
                    ours,
                    theirs,
                    "Row is deleted on one side but edited on the other.".to_string(),
                );
                return ours.cloned();
            }
        }
        return None;
    }
    // their_deleted && !our_deleted
    if let Some(our) = ours {
        let edited = base.is_none_or(|base| !content_eq(table, our, base));
        if edited {
            builder.push_conflict(
                "delete_vs_edit",
                table,
                key,
                base,
                ours,
                theirs,
                "Row is deleted on one side but edited on the other.".to_string(),
            );
            return Some(our.clone());
        }
    }
    None
}

/// `dependency_kind` auto-resolution (design §2.3): the same
/// `task_dependencies` row differing only in `kind` between `strong` and
/// `informational` resolves to `strong`, order-independently, instead of
/// blocking as `immutable_edit`. Any other difference stays a conflict.
fn dependency_kind_election(table: &str, ours: &Row, theirs: &Row) -> Option<Row> {
    if table != "task_dependencies" {
        return None;
    }
    let our_kind = ours.get("kind").and_then(Value::as_str)?;
    let their_kind = theirs.get("kind").and_then(Value::as_str)?;
    if our_kind == their_kind || !is_dependency_kind(our_kind) || !is_dependency_kind(their_kind) {
        return None;
    }
    if frame_without_kind(table, ours) != frame_without_kind(table, theirs) {
        return None;
    }
    if our_kind == "strong" {
        Some(ours.clone())
    } else {
        Some(theirs.clone())
    }
}

fn is_dependency_kind(kind: &str) -> bool {
    matches!(kind, "strong" | "informational")
}

/// The content frame of a `task_dependencies` row with `kind` removed, so two
/// rows (possibly with different ULIDs) that differ only by dependency kind
/// compare equal. [`identity::frame`] already drops the `id` identity column.
fn frame_without_kind(table: &str, row: &Row) -> BTreeMap<String, Value> {
    let mut frame = frame(table, row);
    frame.remove("kind");
    frame
}

/// `dependency_kind` auto-resolution across distinct ULIDs claiming one
/// semantic edge (design §2.3). The generic unique-key resolver would block
/// two `task_dependencies` rows with the same
/// `(task_id, prerequisite_task_id)` but different `kind`; here a pure
/// `strong` vs `informational` difference elects the `strong` row and records
/// a `dependency_kind` auto-resolution instead. Any genuine field difference
/// (or an unrecognised kind pair) is left to the blocking resolver.
fn resolve_dependency_kind_collisions(
    result: &mut TableSet,
    builder: &mut Builder,
) -> Result<(), CarryCtxError> {
    let table = "task_dependencies";
    if !result.contains_key(table) {
        return Ok(());
    }

    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut rows_by_key: BTreeMap<String, Row> = BTreeMap::new();
    for row in table_rows(Some(result), table) {
        let map = as_object(table, row)?;
        let Some(edge) = identity::semantic_key(table, map)? else {
            continue;
        };
        let key = identity_key(table, map)?;
        groups.entry(edge).or_default().push(key.clone());
        rows_by_key.entry(key).or_insert_with(|| map.clone());
    }

    let mut drop: BTreeSet<String> = BTreeSet::new();
    for (_edge, mut members) in groups {
        if members.len() < 2 {
            continue;
        }
        members.sort();
        let kinds: BTreeSet<&str> = members
            .iter()
            .filter_map(|key| {
                rows_by_key
                    .get(key)
                    .and_then(|row| row.get("kind"))
                    .and_then(Value::as_str)
            })
            .collect();
        if !(kinds.contains("strong") && kinds.contains("informational")) {
            continue;
        }
        let Some(first) = rows_by_key.get(&members[0]) else {
            continue;
        };
        let first_frame = frame_without_kind(table, first);
        if !members.iter().all(|key| {
            rows_by_key
                .get(key)
                .map(|row| frame_without_kind(table, row) == first_frame)
                .unwrap_or(false)
        }) {
            continue;
        }

        let survivor = members
            .iter()
            .find(|key| {
                rows_by_key
                    .get(*key)
                    .and_then(|row| row.get("kind"))
                    .and_then(Value::as_str)
                    == Some("strong")
            })
            .cloned()
            .expect("a strong member exists");
        let winner = rows_by_key.get(&survivor).cloned().expect("survivor row");
        for key in &members {
            if key != &survivor {
                drop.insert(key.clone());
            }
        }
        builder.auto_resolutions.push(AutoResolution {
            table: table.to_string(),
            key: survivor,
            kind: "dependency_kind".to_string(),
            winner: row_digest(table, &winner),
            reason: "Strong dependency kind wins over informational for the same edge.".to_string(),
        });
    }

    if !drop.is_empty() {
        if let Some(rows) = result.get_mut(table) {
            rows.retain(|row| {
                row.as_object()
                    .and_then(|map| identity_key(table, map).ok())
                    .map(|key| !drop.contains(&key))
                    .unwrap_or(true)
            });
        }
    }
    Ok(())
}

impl Builder<'_> {
    fn elect(
        &mut self,
        table: &str,
        key: &str,
        base: Option<&Row>,
        ours: &Row,
        theirs: &Row,
    ) -> Value {
        if table == "tasks" {
            let our_status = status(ours);
            let their_status = status(theirs);
            let our_terminal = is_terminal_status(our_status);
            let their_terminal = is_terminal_status(their_status);
            if our_terminal && their_terminal && our_status != their_status {
                let (first, second) = if our_status <= their_status {
                    (our_status, their_status)
                } else {
                    (their_status, our_status)
                };
                self.push_conflict(
                    "status_gap",
                    table,
                    key,
                    base,
                    Some(ours),
                    Some(theirs),
                    format!(
                        "Terminal task status '{first}' conflicts with terminal status '{second}'."
                    ),
                );
                return Value::Object(ours.clone());
            }
            if our_terminal != their_terminal {
                let (winner, loser) = if our_terminal {
                    (ours, theirs)
                } else {
                    (theirs, ours)
                };
                let elected = elected_row(table, winner, loser, Some(ours));
                self.auto_resolutions.push(AutoResolution {
                    table: table.to_string(),
                    key: key.to_string(),
                    kind: "status_terminal".to_string(),
                    winner: row_digest(table, &elected),
                    reason: "Terminal task status beats a non-terminal status.".to_string(),
                });
                return Value::Object(elected);
            }
        }

        if self.options.strict_edits {
            self.push_conflict(
                "row_edit",
                table,
                key,
                base,
                Some(ours),
                Some(theirs),
                "Both sides changed this row and --strict-edits is enabled.".to_string(),
            );
            return Value::Object(ours.clone());
        }

        let (winner, loser, reason) = lww(table, ours, theirs);
        let elected = elected_row(table, winner, loser, Some(ours));
        self.auto_resolutions.push(AutoResolution {
            table: table.to_string(),
            key: key.to_string(),
            kind: "row_edit".to_string(),
            winner: row_digest(table, &elected),
            reason,
        });
        Value::Object(elected)
    }
}

/// Whether a side's row changed relative to `base`. With no base, presence is
/// treated as a change; absence is never a change.
fn changed(table: &str, side: Option<&Row>, base: Option<&Row>) -> bool {
    match side {
        None => false,
        Some(_) if base.is_none() => true,
        Some(side) => !content_eq(table, side, base.expect("base present")),
    }
}

fn status(row: &Row) -> &str {
    row.get("status").and_then(Value::as_str).unwrap_or("")
}

fn is_terminal_status(status: &str) -> bool {
    matches!(status, "completed" | "cancelled")
}

fn parse_instant(value: Option<&Value>) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    value
        .and_then(Value::as_str)
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
}

/// Row-level last-writer-wins by `updated_at`, with a canonical row-JSON then
/// `id` tie-break so the winner never depends on which side is `ours`.
fn lww<'a>(table: &str, ours: &'a Row, theirs: &'a Row) -> (&'a Row, &'a Row, String) {
    let our_time = parse_instant(ours.get("updated_at"));
    let their_time = parse_instant(theirs.get("updated_at"));
    match (our_time, their_time) {
        (Some(a), Some(b)) if a > b => (
            ours,
            theirs,
            "Last-writer-wins: the elected row has the newer updated_at.".to_string(),
        ),
        (Some(a), Some(b)) if b > a => (
            theirs,
            ours,
            "Last-writer-wins: the elected row has the newer updated_at.".to_string(),
        ),
        _ => {
            let our_canon = canonical_frame(table, ours);
            let their_canon = canonical_frame(table, theirs);
            if our_canon != their_canon {
                if our_canon > their_canon {
                    (
                        ours,
                        theirs,
                        "Equal updated_at: canonical row JSON tie-break.".to_string(),
                    )
                } else {
                    (
                        theirs,
                        ours,
                        "Equal updated_at: canonical row JSON tie-break.".to_string(),
                    )
                }
            } else {
                let our_id = identity_key(table, ours).unwrap_or_default();
                let their_id = identity_key(table, theirs).unwrap_or_default();
                if our_id >= their_id {
                    (
                        ours,
                        theirs,
                        "Equal updated_at and content: id tie-break.".to_string(),
                    )
                } else {
                    (
                        theirs,
                        ours,
                        "Equal updated_at and content: id tie-break.".to_string(),
                    )
                }
            }
        }
    }
}

/// Build the elected row: NULL-union monotonic facts from the loser, then
/// keep our machine-local columns (never overwrite ours from theirs).
fn elected_row(table: &str, winner: &Row, loser: &Row, ours: Option<&Row>) -> Row {
    let mut elected = winner.clone();
    for column in identity::MONOTONIC_COLUMNS {
        let winner_missing = elected.get(*column).is_none_or(Value::is_null);
        if winner_missing {
            if let Some(value) = loser.get(*column).filter(|value| !value.is_null()) {
                elected.insert((*column).to_string(), value.clone());
            }
        }
    }
    overlay_machine_local(table, &mut elected, ours);
    elected
}

/// Copy our machine-local columns onto a row that may have been taken from
/// theirs (design §1.7: never overwrite ours from theirs).
fn overlay_machine_local(table: &str, row: &mut Row, ours: Option<&Row>) {
    if let Some(ours) = ours {
        for column in machine_local_columns(table) {
            if let Some(value) = ours.get(*column) {
                row.insert((*column).to_string(), value.clone());
            }
        }
    }
}

fn row_digest(table: &str, row: &Row) -> String {
    crate::checksum::sha256_hex(canonical_frame(table, row).as_bytes())
}

/// Append-only tables merge by union on `id`; a same-id content mismatch is a
/// tamper signal that fails the whole merge before any output (design §1.4).
fn merge_append_only(
    table: &str,
    base: Option<&TableSet>,
    ours: &TableSet,
    theirs: &TableSet,
) -> Result<Vec<Value>, CarryCtxError> {
    let mut rows: RowIndex = RowIndex::new();
    for tables in [base, Some(ours), Some(theirs)].into_iter().flatten() {
        for row in table_rows(Some(tables), table) {
            let map = as_object(table, row)?;
            let id = identity_key(table, map)?;
            match rows.get(&id) {
                None => {
                    rows.insert(id, map.clone());
                }
                Some(existing) => {
                    if canonical_frame(table, existing) != canonical_frame(table, map) {
                        return Err(CarryCtxError::validation_error(format!(
                            "Append-only table '{table}' row '{id}' has differing content between sides; tamper detected."
                        ))
                        .with_details(serde_json::json!({
                            "table": table,
                            "row_id": id,
                        })));
                    }
                }
            }
        }
    }
    Ok(rows.into_values().map(Value::Object).collect())
}

/// `sequences` merge by per-`(project_id, kind)` maximum `next_value`; counters
/// never rewind and kinds from every side are kept (design §1.5).
fn merge_sequences(
    base: Option<&TableSet>,
    ours: &TableSet,
    theirs: &TableSet,
) -> Result<Vec<Value>, CarryCtxError> {
    let mut rows: BTreeMap<String, Row> = BTreeMap::new();
    for tables in [base, Some(ours), Some(theirs)].into_iter().flatten() {
        for row in table_rows(Some(tables), "sequences") {
            let map = as_object("sequences", row)?;
            let project_id = map.get("project_id").and_then(Value::as_str).unwrap_or("");
            let kind = map.get("kind").and_then(Value::as_str).unwrap_or("");
            let next = map.get("next_value").and_then(Value::as_u64).unwrap_or(1);
            let key = format!("{project_id}\u{1}{kind}");
            match rows.get_mut(&key) {
                None => {
                    let mut copy = map.clone();
                    copy.insert("next_value".to_string(), Value::from(next));
                    rows.insert(key, copy);
                }
                Some(existing) => {
                    let current = existing
                        .get("next_value")
                        .and_then(Value::as_u64)
                        .unwrap_or(1);
                    if next > current {
                        existing.insert("next_value".to_string(), Value::from(next));
                    }
                }
            }
        }
    }
    Ok(rows.into_values().map(Value::Object).collect())
}

/// Union tombstones by key, keeping the earliest `deleted_at` (design §1.3).
fn merge_tombstones(
    base: Option<&TableSet>,
    ours: &TableSet,
    theirs: &TableSet,
) -> Result<Vec<Value>, CarryCtxError> {
    let mut rows: BTreeMap<String, Row> = BTreeMap::new();
    for tables in [base, Some(ours), Some(theirs)].into_iter().flatten() {
        for row in table_rows(Some(tables), "tombstones") {
            let map = as_object("tombstones", row)?;
            let project_id = map.get("project_id").and_then(Value::as_str).unwrap_or("");
            let table_name = map.get("table_name").and_then(Value::as_str).unwrap_or("");
            let row_id = map.get("row_id").and_then(Value::as_str).unwrap_or("");
            let key = format!("{project_id}\u{1}{table_name}\u{1}{row_id}");
            match rows.get_mut(&key) {
                None => {
                    rows.insert(key, map.clone());
                }
                Some(existing) => {
                    if earlier_tombstone(map, existing) {
                        *existing = map.clone();
                    }
                }
            }
        }
    }
    Ok(rows.into_values().map(Value::Object).collect())
}

fn earlier_tombstone(candidate: &Row, current: &Row) -> bool {
    let candidate_time = parse_instant(candidate.get("deleted_at"));
    let current_time = parse_instant(current.get("deleted_at"));
    match (candidate_time, current_time) {
        (Some(a), Some(b)) if a != b => a < b,
        (Some(_), None) => true,
        (None, Some(_)) => false,
        _ => canonical_frame("tombstones", candidate) < canonical_frame("tombstones", current),
    }
}

/// Drop tombstones for keys that the merged result still carries a row for, so
/// "conflicts left at ours" never yields a row and a tombstone for one key.
fn filter_tombstones(tombstones: &mut Vec<Value>, result: &TableSet) {
    let mut present: BTreeSet<(String, String)> = BTreeSet::new();
    for (table, rows) in result {
        if table == "tombstones" {
            continue;
        }
        for row in rows {
            if let Some(map) = row.as_object() {
                if let Ok(key) = identity_key(table, map) {
                    present.insert((table.clone(), key));
                }
            }
        }
    }
    tombstones.retain(|row| {
        let Some(map) = row.as_object() else {
            return true;
        };
        let (Some(table_name), Some(row_id)) = (
            map.get("table_name").and_then(Value::as_str),
            map.get("row_id").and_then(Value::as_str),
        ) else {
            return true;
        };
        !present.contains(&(table_name.to_string(), row_id.to_string()))
    });
}

/// Plan agent-name aliases order-independently (design §2.3).
///
/// Agents with the same `(project_id, name)` but different ULIDs are the same
/// human/agent identity that forked. The survivor is the **canonical minimum
/// ULID** — never "whichever side is `ours`" — so `merge(A, B)` and
/// `merge(B, A)` elect the same agent and record the same alias. Every other
/// id in the group is aliased into the survivor and its references remapped by
/// [`remap_agent_references`].
fn plan_agent_aliases(
    ours: &TableSet,
    theirs: &TableSet,
    builder: &mut Builder,
) -> Result<Vec<AgentAlias>, CarryCtxError> {
    let our_agents = index_rows(
        "agents",
        table_rows(Some(ours), "agents"),
        &mut builder.warnings,
    )?;
    let their_agents = index_rows(
        "agents",
        table_rows(Some(theirs), "agents"),
        &mut builder.warnings,
    )?;

    // Group every agent id from both sides by its name identity. `BTreeSet`
    // keeps the ids sorted, so the first is the canonical minimum.
    let mut groups: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    for agents in [&our_agents, &their_agents] {
        for (id, row) in agents {
            if let (Some(project_id), Some(name)) = (
                row.get("project_id").and_then(Value::as_str),
                row.get("name").and_then(Value::as_str),
            ) {
                groups
                    .entry((project_id.to_string(), name.to_string()))
                    .or_default()
                    .insert(id.clone());
            }
        }
    }

    let mut aliases = Vec::new();
    for ((project_id, name), ids) in groups {
        if ids.len() < 2 {
            continue;
        }
        let mut members = ids.into_iter();
        let survivor = members.next().expect("non-empty group");
        for loser in members {
            aliases.push(AgentAlias {
                project_id: project_id.clone(),
                name: name.clone(),
                existing_agent_id: survivor.clone(),
                incoming_agent_id: loser,
                remapped_references: Vec::new(),
            });
        }
    }
    aliases.sort_by(|a, b| {
        (&a.project_id, &a.name, &a.incoming_agent_id).cmp(&(
            &b.project_id,
            &b.name,
            &b.incoming_agent_id,
        ))
    });
    Ok(aliases)
}

/// Rewrite every agent FK reference from an aliased loser id to its canonical
/// survivor in the merged result, drop the folded agent rows, and record each
/// rewritten cell on the alias (design §2.3, §1.7).
///
/// The loser is whichever non-survivor the order-independent
/// [`plan_agent_aliases`] elected, so the remap set is the same under a side
/// swap even when the loser row originated on the local side.
fn remap_agent_references(result: &mut TableSet, aliases: &mut [AgentAlias]) {
    let remap: BTreeMap<String, String> = aliases
        .iter()
        .map(|alias| {
            (
                alias.incoming_agent_id.clone(),
                alias.existing_agent_id.clone(),
            )
        })
        .collect();
    if remap.is_empty() {
        return;
    }

    for (table, column) in AGENT_REFERENCE_COLUMNS {
        let Some(rows) = result.get_mut(*table) else {
            continue;
        };
        for row in rows.iter_mut() {
            let Some(map) = row.as_object_mut() else {
                continue;
            };
            let Some(current) = map.get(*column).and_then(Value::as_str).map(str::to_string) else {
                continue;
            };
            let Some(existing) = remap.get(&current) else {
                continue;
            };
            map.insert((*column).to_string(), Value::String(existing.clone()));
            let row_id = identity_key(table, map).unwrap_or_default();
            if let Some(alias) = aliases
                .iter_mut()
                .find(|alias| alias.incoming_agent_id == current)
            {
                alias.remapped_references.push(ReferenceRemap {
                    table: table.to_string(),
                    row_id,
                    column: column.to_string(),
                });
            }
        }
    }

    if let Some(rows) = result.get_mut("agents") {
        rows.retain(|row| {
            row.as_object()
                .and_then(|map| map.get("id"))
                .and_then(Value::as_str)
                .map(|id| !remap.contains_key(id))
                .unwrap_or(true)
        });
    }

    for alias in aliases.iter_mut() {
        alias.remapped_references.sort_by(|a, b| {
            (&a.table, &a.row_id, &a.column).cmp(&(&b.table, &b.row_id, &b.column))
        });
        alias.remapped_references.dedup();
    }
}

/// Blocking `unique_key` collisions for non-display, non-agent unique keys
/// (design §2.3): two different ULIDs claiming one unique key.
///
/// The conflict is identified by the **semantic unique key** — the sorted
/// column list plus the canonical column values — never by whichever ULID is
/// dropped, so a side swap yields the same conflict id. The surviving row is
/// the canonical minimum identity key (again independent of `ours`), which
/// keeps the candidate row set commutative; because the collision is blocking,
/// a caller must not apply it until a human resolves it.
fn resolve_unique_collisions(
    table: &str,
    columns: &[&str],
    ours: &TableSet,
    theirs: &TableSet,
    result: &mut TableSet,
    builder: &mut Builder,
) -> Result<(), CarryCtxError> {
    if !result.contains_key(table) {
        return Ok(());
    }
    let our_keys: BTreeSet<String> =
        index_rows(table, table_rows(Some(ours), table), &mut Vec::new())?
            .keys()
            .cloned()
            .collect();
    let their_keys: BTreeSet<String> =
        index_rows(table, table_rows(Some(theirs), table), &mut Vec::new())?
            .keys()
            .cloned()
            .collect();

    let mut sorted_columns: Vec<&str> = columns.to_vec();
    sorted_columns.sort_unstable();

    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut group_rows: BTreeMap<String, Row> = BTreeMap::new();
    for row in table_rows(Some(result), table) {
        let map = as_object(table, row)?;
        let key = identity_key(table, map)?;
        let parts: Vec<&str> = sorted_columns
            .iter()
            .map(|column| map.get(*column).and_then(Value::as_str).unwrap_or(""))
            .collect();
        let vkey = parts.join("\u{1}");
        groups.entry(vkey).or_default().push(key.clone());
        group_rows.entry(key).or_insert_with(|| map.clone());
    }

    let mut keep: BTreeSet<String> = BTreeSet::new();
    for (vkey, mut members) in groups {
        if members.len() <= 1 {
            keep.extend(members);
            continue;
        }
        members.sort();
        let survivor = members[0].clone();
        keep.insert(survivor.clone());

        let semantic_key = format!("{}|{}", sorted_columns.join(","), vkey);
        let ours_row = members
            .iter()
            .find(|key| our_keys.contains(*key))
            .and_then(|key| group_rows.get(key));
        let theirs_row = members
            .iter()
            .find(|key| their_keys.contains(*key))
            .and_then(|key| group_rows.get(key));
        builder.push_conflict(
            "unique_key",
            table,
            &semantic_key,
            None,
            ours_row,
            theirs_row,
            format!("Unique key collision on table '{table}'."),
        );
    }

    if let Some(rows) = result.get_mut(table) {
        rows.retain(|row| {
            row.as_object()
                .and_then(|map| identity_key(table, map).ok())
                .map(|key| keep.contains(&key))
                .unwrap_or(true)
        });
    }
    Ok(())
}

/// Keep project identity columns from `ours` and warn on bundle drift
/// (design §1.6); the merge engine does not re-anchor paths itself.
fn stabilize_projects(
    result: &mut TableSet,
    ours: &TableSet,
    theirs: &TableSet,
    builder: &mut Builder,
) -> Result<(), CarryCtxError> {
    let our_project = table_rows(Some(ours), "projects")
        .first()
        .and_then(Value::as_object)
        .cloned();
    let Some(our_project) = our_project else {
        return Ok(());
    };
    let their_project = table_rows(Some(theirs), "projects")
        .first()
        .and_then(Value::as_object)
        .cloned();

    if let Some(their_project) = &their_project {
        for column in ["name", "task_prefix"] {
            let ours_value = our_project.get(column);
            let theirs_value = their_project.get(column);
            if ours_value.is_some() && theirs_value.is_some() && ours_value != theirs_value {
                builder.warnings.push(format!(
                    "Project '{column}' differs between local state and the bundle; keeping the local value."
                ));
            }
        }
    }

    if let Some(rows) = result.get_mut("projects") {
        for row in rows.iter_mut() {
            let Some(map) = row.as_object_mut() else {
                continue;
            };
            for column in ["id", "name", "task_prefix"] {
                if let Some(value) = our_project.get(column) {
                    map.insert(column.to_string(), value.clone());
                }
            }
        }
    }
    Ok(())
}

/// Plan display-id renumbering (design §1.1, §2.3). Display ids are allocator
/// artifacts, not identity: when two ULIDs claim one display id the canonical
/// minimum keeps it and the others are renumbered from the merged sequence
/// floor (max existing + 1, monotonic). Choosing the survivor by identity key
/// rather than by side keeps the merged rows commutative.
///
/// A sequence row is written back **only for kinds that actually renumbered**,
/// so `merge(M, M)` (and any merge without a collision) leaves both the
/// display ids and the sequence floors untouched.
fn plan_display_renumbers(result: &mut TableSet, warnings: &mut Vec<String>) -> Vec<Renumber> {
    let sequences = sequence_state(result);
    let high_water = display_high_water(result);

    // Sequence floors to persist: only kinds consumed by a renumber.
    let mut updates: BTreeMap<(String, String), u64> = BTreeMap::new();
    let mut renumbers = Vec::new();

    for table in identity::DISPLAY_TABLES {
        let Some(rows) = result.get(*table).cloned() else {
            continue;
        };

        let mut groups: BTreeMap<String, Vec<(String, Row)>> = BTreeMap::new();
        for row in &rows {
            let Some(map) = row.as_object() else {
                continue;
            };
            let Some(display) = display_id(map) else {
                continue;
            };
            let key = identity_key(table, map).unwrap_or_default();
            groups
                .entry(display.to_string())
                .or_default()
                .push((key, map.clone()));
        }

        let mut merged_rows: Vec<Value> = Vec::with_capacity(rows.len());
        for (display, mut members) in groups {
            if members.len() <= 1 {
                if let Some((_, row)) = members.pop() {
                    merged_rows.push(Value::Object(row));
                }
                continue;
            }
            members.sort_by(|a, b| a.0.cmp(&b.0));
            let (_, survivor) = members.remove(0);
            merged_rows.push(Value::Object(survivor));
            for (key, mut map) in members {
                let Some(kind) = display_kind(table, &display) else {
                    merged_rows.push(Value::Object(map));
                    continue;
                };
                let project_id = map
                    .get("project_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let next = {
                    let entry = updates
                        .entry((project_id.clone(), kind.kind.clone()))
                        .or_insert_with(|| {
                            let existing = sequences
                                .get(&(project_id.clone(), kind.kind.clone()))
                                .copied()
                                .unwrap_or(1);
                            let floor = high_water
                                .get(&(project_id.clone(), kind.kind.clone()))
                                .copied()
                                .unwrap_or(0)
                                .saturating_add(1);
                            existing.max(floor).max(1)
                        });
                    let value = *entry;
                    *entry = entry.saturating_add(1);
                    value
                };
                let new_display = render_display_id(&kind.prefix, next, kind.width);
                map.insert("display_id".to_string(), Value::String(new_display.clone()));
                warnings.push(format!(
                    "Display id '{display}' collided on table '{table}'; renumbered row '{key}' to '{new_display}'."
                ));
                renumbers.push(Renumber {
                    table: table.to_string(),
                    row_id: key,
                    display_id: display.clone(),
                    new_display_id: new_display,
                    reason: "Display id collision: display ids are not identity.".to_string(),
                });
                merged_rows.push(Value::Object(map));
            }
        }
        result.insert(table.to_string(), merged_rows);
    }

    if !updates.is_empty() {
        write_back_sequences(result, &updates);
    }
    renumbers.sort_by(|a, b| {
        (&a.table, &a.row_id, &a.new_display_id).cmp(&(&b.table, &b.row_id, &b.new_display_id))
    });
    renumbers
}

/// The largest display number observed per `(project_id, sequence_kind)` across
/// every display table, used to seed a renumber floor lazily.
fn display_high_water(result: &TableSet) -> BTreeMap<(String, String), u64> {
    let mut high_water: BTreeMap<(String, String), u64> = BTreeMap::new();
    for table in identity::DISPLAY_TABLES {
        let Some(rows) = result.get(*table) else {
            continue;
        };
        for row in rows {
            let Some(map) = row.as_object() else {
                continue;
            };
            let (Some(project_id), Some(display)) = (
                map.get("project_id").and_then(Value::as_str),
                display_id(map),
            ) else {
                continue;
            };
            let Some(kind) = display_kind(table, display) else {
                continue;
            };
            if let Some(parts) = split_display_id(display) {
                let entry = high_water
                    .entry((project_id.to_string(), kind.kind))
                    .or_insert(0);
                *entry = (*entry).max(parts.number);
            }
        }
    }
    high_water
}

fn sequence_state(result: &TableSet) -> BTreeMap<(String, String), u64> {
    let mut sequences = BTreeMap::new();
    if let Some(rows) = result.get("sequences") {
        for row in rows {
            if let Some(map) = row.as_object() {
                if let (Some(project_id), Some(kind), Some(next)) = (
                    map.get("project_id").and_then(Value::as_str),
                    map.get("kind").and_then(Value::as_str),
                    map.get("next_value").and_then(Value::as_u64),
                ) {
                    sequences.insert((project_id.to_string(), kind.to_string()), next);
                }
            }
        }
    }
    sequences
}

fn write_back_sequences(result: &mut TableSet, sequences: &BTreeMap<(String, String), u64>) {
    if sequences.is_empty() {
        return;
    }
    let rows = result.entry("sequences".to_string()).or_default();
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    for row in rows.iter_mut() {
        if let Some(map) = row.as_object_mut() {
            let project_id = map
                .get("project_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let kind = map
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if let Some(next) = sequences.get(&(project_id.clone(), kind.clone())) {
                map.insert("next_value".to_string(), Value::from(*next));
                seen.insert((project_id, kind));
            }
        }
    }
    for ((project_id, kind), next) in sequences {
        if seen.contains(&(project_id.clone(), kind.clone())) {
            continue;
        }
        rows.push(serde_json::json!({
            "project_id": project_id,
            "kind": kind,
            "next_value": next,
        }));
    }
}

/// Compute the row inserts/updates and deletes needed to move `ours` to
/// `result` (machine-local content is ignored so it is never rewritten).
fn compute_writes_deletes(
    ours: &TableSet,
    result: &TableSet,
) -> Result<(Vec<RowWrite>, Vec<RowDelete>), CarryCtxError> {
    let mut writes = Vec::new();
    let mut deletes = Vec::new();

    let mut table_names: BTreeSet<&String> = BTreeSet::new();
    table_names.extend(ours.keys());
    table_names.extend(result.keys());

    for table in table_names {
        let our_index = index_rows(table, table_rows(Some(ours), table), &mut Vec::new())?;
        let result_index = index_rows(table, table_rows(Some(result), table), &mut Vec::new())?;

        for (key, row) in &result_index {
            match our_index.get(key) {
                None => writes.push(RowWrite {
                    table: table.clone(),
                    key: key.clone(),
                    row: Value::Object(row.clone()),
                    kind: WriteKind::Insert,
                }),
                Some(our_row) if frame(table, our_row) != frame(table, row) => {
                    writes.push(RowWrite {
                        table: table.clone(),
                        key: key.clone(),
                        row: Value::Object(row.clone()),
                        kind: WriteKind::Update,
                    });
                }
                Some(_) => {}
            }
        }

        for key in our_index.keys() {
            if !result_index.contains_key(key) {
                deletes.push(RowDelete {
                    table: table.clone(),
                    key: key.clone(),
                });
            }
        }
    }

    writes.sort_by(|a, b| (&a.table, &a.key).cmp(&(&b.table, &b.key)));
    deletes.sort_by(|a, b| (&a.table, &a.key).cmp(&(&b.table, &b.key)));
    Ok((writes, deletes))
}

fn sort_rows_by_key(table: &str, rows: &mut [Value]) {
    rows.sort_by(|a, b| {
        let left = a
            .as_object()
            .and_then(|map| identity_key(table, map).ok())
            .unwrap_or_default();
        let right = b
            .as_object()
            .and_then(|map| identity_key(table, map).ok())
            .unwrap_or_default();
        left.cmp(&right)
    });
}
