//! Pure export-id DAG and merge-base acquisition (design
//! `2026-09-10-mergeable-git-managed-state.md` §2.1).
//!
//! This is the base-selection half of the merge engine. Given the locally
//! known snapshot nodes (export id plus ordered parent export ids) and the
//! export ids of `ours` (the live database's last export) and `theirs` (the
//! incoming bundle), [`ExportDag::newest_common_ancestor`] returns the newest
//! common ancestor. [`resolve_base`] then applies the design §2.1 fallback
//! order — explicit `--base` > merge-base over the DAG > local snapshot cache >
//! base-less degraded — with the snapshot cache supplied as a caller closure
//! so the engine stays pure.
//!
//! No I/O happens here: loading snapshot nodes from `carryctx-snapshots`
//! trailers or the local snapshot cache, and materializing the base rows, are
//! caller concerns (CTX-0142/CTX-0145). Passing no base to
//! [`super::merge_tables`] runs the degraded two-way merge.
//!
//! ## Ordering
//!
//! Export ids are ULIDs: 26-character Crockford base32 strings whose natural
//! `str`/`String` byte-wise lexicographic order is creation order, so `max()`
//! selects the newest. The engine relies on plain [`Ord`] over the id string
//! and never parses the ULID, so any opaque, lexicographically stable id works.

use super::plan::TableSet;
use crate::manifest::PackManifest;
use carryctx_core::error::CarryCtxError;
use std::collections::{BTreeMap, BTreeSet};

/// One node of the export-id DAG: a snapshot export id and its ordered parent
/// export ids (`[]` for a first export). Mirrors manifest v2 `parents`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotNode {
    pub export_id: String,
    pub parents: Vec<String>,
}

impl SnapshotNode {
    pub fn new(export_id: impl Into<String>, parents: Vec<String>) -> Self {
        Self {
            export_id: export_id.into(),
            parents,
        }
    }
}

/// An export-id DAG: `export_id -> ordered parent export ids`. Construct it
/// from a node set ([`ExportDag::from_snapshot_nodes`], [`ExportDag::from_edges`],
/// [`ExportDag::from_nodes`]) or straight from a v2 manifest's `parents`
/// ([`ExportDag::from_manifest`]). Parent ids referenced by an edge but not
/// present as nodes are still reachable ancestors.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportDag {
    parents: BTreeMap<String, Vec<String>>,
}

impl ExportDag {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build from snapshot nodes (export id plus parent list).
    pub fn from_snapshot_nodes(nodes: &[SnapshotNode]) -> Self {
        let mut dag = Self::new();
        for node in nodes {
            dag.insert(node.export_id.clone(), node.parents.clone());
        }
        dag
    }

    /// Build from `(export_id, parents)` edges. Roots may be passed with an
    /// empty parent list.
    pub fn from_edges<I, S>(edges: I) -> Self
    where
        I: IntoIterator<Item = (S, Vec<S>)>,
        S: Into<String>,
    {
        let mut dag = Self::new();
        for (export_id, parents) in edges {
            dag.insert(export_id, parents);
        }
        dag
    }

    /// Build a parentless node set (all roots).
    pub fn from_nodes<I, S>(nodes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut dag = Self::new();
        for export_id in nodes {
            dag.add_node(export_id);
        }
        dag
    }

    /// Build from a v2 manifest: `manifest.export_id -> manifest.parents`.
    pub fn from_manifest(manifest: &PackManifest) -> Self {
        let mut dag = Self::new();
        dag.insert(manifest.export_id.clone(), manifest.parents.clone());
        dag
    }

    /// Register `export_id` as a node with no parents if it is not present.
    pub fn add_node(&mut self, export_id: impl Into<String>) {
        self.parents.entry(export_id.into()).or_default();
    }

    /// Insert or replace one `export_id -> parents` edge, registering every
    /// referenced parent as a node.
    pub fn insert<S: Into<String>>(&mut self, export_id: impl Into<String>, parents: Vec<S>) {
        let export_id = export_id.into();
        let parents: Vec<String> = parents.into_iter().map(Into::into).collect();
        for parent in &parents {
            self.add_node(parent.clone());
        }
        self.parents.insert(export_id, parents);
    }

    pub fn contains(&self, export_id: &str) -> bool {
        self.parents.contains_key(export_id)
    }

    /// The direct parents of `export_id` (empty when the id is unknown).
    pub fn parents_of(&self, export_id: &str) -> &[String] {
        self.parents
            .get(export_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn len(&self) -> usize {
        self.parents.len()
    }

    pub fn is_empty(&self) -> bool {
        self.parents.is_empty()
    }

    /// Transitive parent closure of `export_id`, including `export_id` itself.
    /// Returns `None` when a cycle is reachable, so a corrupt DAG degrades
    /// instead of looping or silently picking a bogus base.
    pub fn ancestors(&self, export_id: &str) -> Option<BTreeSet<String>> {
        let mut result = BTreeSet::new();
        if visit(
            export_id,
            &self.parents,
            &mut BTreeSet::new(),
            &mut BTreeSet::new(),
            &mut result,
        ) {
            Some(result)
        } else {
            None
        }
    }

    /// The newest common ancestor of `ours` and `theirs`, or `None` when the
    /// histories share no ancestor (a degraded two-way merge) or the DAG is
    /// cyclic (fail closed).
    ///
    /// Each id is treated as an ancestor of itself, so a linear history where
    /// one point is an ancestor of the other returns that ancestor, and
    /// identical ids return the id itself. When several incomparable common
    /// ancestors exist (criss-cross), the greatest export id is returned so the
    /// choice is deterministic and independent of argument order.
    pub fn newest_common_ancestor(&self, ours: &str, theirs: &str) -> Option<String> {
        let ours_ancestors = self.ancestors(ours)?;
        let theirs_ancestors = self.ancestors(theirs)?;
        ours_ancestors
            .intersection(&theirs_ancestors)
            .max()
            .cloned()
    }
}

/// Convenience wrapper over [`ExportDag::newest_common_ancestor`] for a bare
/// node set.
pub fn select_merge_base(nodes: &[SnapshotNode], ours: &str, theirs: &str) -> Option<String> {
    ExportDag::from_snapshot_nodes(nodes).newest_common_ancestor(ours, theirs)
}

fn visit(
    node: &str,
    parents: &BTreeMap<String, Vec<String>>,
    on_path: &mut BTreeSet<String>,
    done: &mut BTreeSet<String>,
    result: &mut BTreeSet<String>,
) -> bool {
    if done.contains(node) {
        return true;
    }
    if !on_path.insert(node.to_string()) {
        return false;
    }
    result.insert(node.to_string());
    if let Some(children) = parents.get(node) {
        for parent in children {
            if !visit(parent, parents, on_path, done, result) {
                return false;
            }
        }
    }
    on_path.remove(node);
    done.insert(node.to_string());
    true
}

/// Where the effective merge base came from (design §2.1 fallback order).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BaseSource {
    /// A caller-supplied `--base` row set.
    Explicit,
    /// The newest common ancestor over the export-id DAG, materialized from
    /// the local snapshot cache.
    Ancestor,
    /// The local snapshot cache entry for `ours`'s own last export, used when
    /// no common ancestor exists.
    Snapshot,
    /// No base: a degraded two-way merge.
    #[default]
    None,
}

/// The outcome of [`resolve_base`]: either a usable base plus its source, or an
/// explicit degraded / required-missing signal.
#[derive(Debug, Clone, PartialEq)]
pub enum BaseResolution {
    Resolved {
        base: TableSet,
        source: BaseSource,
    },
    /// No base could be resolved; run the degraded two-way merge.
    Degraded,
    /// `require_base` was requested but no base could be resolved. The caller
    /// maps this to [`required_base_error`] (`VALIDATION_FAILED`, exit 8).
    RequiredMissing,
}

impl BaseResolution {
    /// The resolved base row set, if any.
    pub fn base(&self) -> Option<&TableSet> {
        match self {
            Self::Resolved { base, .. } => Some(base),
            Self::Degraded | Self::RequiredMissing => None,
        }
    }

    /// The resolution source ([`BaseSource::None`] when unresolved).
    pub fn source(&self) -> BaseSource {
        match self {
            Self::Resolved { source, .. } => *source,
            Self::Degraded | Self::RequiredMissing => BaseSource::None,
        }
    }

    pub fn is_degraded(&self) -> bool {
        matches!(self, Self::Degraded)
    }

    pub fn is_required_missing(&self) -> bool {
        matches!(self, Self::RequiredMissing)
    }
}

/// The error a caller returns for a `--require-base` refusal (design §2.1,
/// §2.3 `base_required_missing`): `VALIDATION_FAILED`, exit 8. Kept in the pure
/// engine so no CLI dependency is needed; CTX-0142 wires it up.
pub fn required_base_error() -> CarryCtxError {
    CarryCtxError::validation_error(
        "A merge base is required but no common ancestor or local snapshot is available.",
    )
    .with_details(serde_json::json!({ "kind": "base_required_missing" }))
}

/// Resolve the effective base with the design §2.1 fallback order:
/// explicit `--base` > newest common ancestor over `dag` > local snapshot of
/// `ours`'s export id > base-less degraded.
///
/// `snapshot` is a caller-supplied pure lookup keyed by export id, so the
/// engine performs no I/O. When nothing resolves and `require_base` is set the
/// result is [`BaseResolution::RequiredMissing`] rather than a degraded merge.
pub fn resolve_base<F>(
    dag: &ExportDag,
    explicit: Option<&TableSet>,
    ours: Option<&str>,
    theirs: Option<&str>,
    require_base: bool,
    mut snapshot: F,
) -> BaseResolution
where
    F: FnMut(&str) -> Option<TableSet>,
{
    if let Some(base) = explicit {
        return BaseResolution::Resolved {
            base: base.clone(),
            source: BaseSource::Explicit,
        };
    }

    if let (Some(ours), Some(theirs)) = (ours, theirs) {
        if let Some(ancestor) = dag.newest_common_ancestor(ours, theirs) {
            if let Some(base) = snapshot(&ancestor) {
                return BaseResolution::Resolved {
                    base,
                    source: BaseSource::Ancestor,
                };
            }
        }
    }

    if let Some(ours) = ours {
        if let Some(base) = snapshot(ours) {
            return BaseResolution::Resolved {
                base,
                source: BaseSource::Snapshot,
            };
        }
    }

    if require_base {
        BaseResolution::RequiredMissing
    } else {
        BaseResolution::Degraded
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::PackSource;
    use serde_json::json;

    fn node(export_id: &str, parents: &[&str]) -> SnapshotNode {
        SnapshotNode::new(
            export_id,
            parents.iter().map(|parent| parent.to_string()).collect(),
        )
    }

    fn base_table() -> TableSet {
        let mut set = TableSet::new();
        set.insert(
            "tasks".to_string(),
            vec![json!({
                "id": "01T",
                "project_id": "01PROJECT",
                "title": "base",
                "status": "planned",
                "updated_at": "2026-01-01T00:00:00Z",
            })],
        );
        set
    }

    #[test]
    fn identical_ids_return_the_id_itself() {
        let nodes = [node("01A", &[])];
        assert_eq!(
            select_merge_base(&nodes, "01A", "01A").as_deref(),
            Some("01A")
        );
    }

    #[test]
    fn linear_history_returns_the_ancestor_regardless_of_order() {
        // C -> B -> A
        let nodes = [
            node("01A", &[]),
            node("01B", &["01A"]),
            node("01C", &["01B"]),
        ];
        assert_eq!(
            select_merge_base(&nodes, "01C", "01B").as_deref(),
            Some("01B")
        );
        assert_eq!(
            select_merge_base(&nodes, "01B", "01C").as_deref(),
            Some("01B")
        );
    }

    #[test]
    fn diamond_returns_the_fork_point() {
        // D -> B -> A ; E -> C -> A
        let nodes = [
            node("01A", &[]),
            node("01B", &["01A"]),
            node("01C", &["01A"]),
            node("01D", &["01B"]),
            node("01E", &["01C"]),
        ];
        assert_eq!(
            select_merge_base(&nodes, "01D", "01E").as_deref(),
            Some("01A")
        );
    }

    #[test]
    fn criss_cross_picks_the_greatest_common_ancestor_deterministically() {
        // A root; B and C children of A; D and E both merge B and C.
        let nodes = [
            node("01A", &[]),
            node("01B", &["01A"]),
            node("01C", &["01A"]),
            node("01D", &["01B", "01C"]),
            node("01E", &["01B", "01C"]),
        ];
        assert_eq!(
            select_merge_base(&nodes, "01D", "01E").as_deref(),
            Some("01C")
        );
        assert_eq!(
            select_merge_base(&nodes, "01E", "01D").as_deref(),
            Some("01C")
        );
    }

    #[test]
    fn disjoint_histories_have_no_common_ancestor() {
        let nodes = [node("01A", &[]), node("01B", &[])];
        assert_eq!(select_merge_base(&nodes, "01A", "01B"), None);
    }

    #[test]
    fn referenced_missing_parents_still_count_as_ancestors() {
        // The shared parent "01B" is an edge on both sides but has no node.
        let nodes = [node("01O", &["01B"]), node("01T", &["01B"])];
        assert_eq!(
            select_merge_base(&nodes, "01O", "01T").as_deref(),
            Some("01B")
        );
    }

    #[test]
    fn cyclic_histories_degrade_to_no_base() {
        let nodes = [node("01X", &["01Y"]), node("01Y", &["01X"])];
        assert_eq!(select_merge_base(&nodes, "01X", "01Y"), None);
    }

    #[test]
    fn selection_is_commutative_over_fixtures() {
        let fixtures: Vec<Vec<SnapshotNode>> = vec![
            vec![
                node("01A", &[]),
                node("01B", &["01A"]),
                node("01C", &["01B"]),
            ],
            vec![
                node("01A", &[]),
                node("01B", &["01A"]),
                node("01C", &["01A"]),
                node("01D", &["01B", "01C"]),
                node("01E", &["01B", "01C"]),
            ],
            vec![node("01A", &[]), node("01B", &[])],
            vec![node("01O", &["01B"]), node("01T", &["01B"])],
        ];
        for nodes in &fixtures {
            for ours in nodes {
                for theirs in nodes {
                    assert_eq!(
                        select_merge_base(nodes, &ours.export_id, &theirs.export_id),
                        select_merge_base(nodes, &theirs.export_id, &ours.export_id),
                        "base selection must be commutative for {} and {}",
                        ours.export_id,
                        theirs.export_id
                    );
                }
            }
        }
    }

    // --- ExportDag constructors and methods -------------------------------

    #[test]
    fn export_dag_methods_match_the_free_function() {
        let nodes = [
            node("01A", &[]),
            node("01B", &["01A"]),
            node("01C", &["01A"]),
            node("01D", &["01B", "01C"]),
            node("01E", &["01B", "01C"]),
        ];
        let dag = ExportDag::from_snapshot_nodes(&nodes);
        assert_eq!(dag.len(), 5);
        assert!(!dag.is_empty());
        assert!(dag.contains("01D"));
        assert_eq!(dag.parents_of("01D"), ["01B", "01C"]);
        assert_eq!(dag.parents_of("missing"), [] as [String; 0]);
        assert_eq!(
            dag.newest_common_ancestor("01D", "01E").as_deref(),
            Some("01C")
        );
        // Greatest export id wins because ids order lexicographically.
        assert!("01C" > "01B");
    }

    #[test]
    fn export_dag_from_edges_and_nodes_are_roots() {
        let dag = ExportDag::from_edges(vec![("01B", vec!["01A"])]);
        assert_eq!(dag.parents_of("01B"), ["01A"]);
        assert_eq!(dag.parents_of("01A"), [] as [String; 0]);

        let roots = ExportDag::from_nodes(["01X", "01Y"]);
        assert_eq!(roots.len(), 2);
        assert!(roots.parents_of("01X").is_empty());
    }

    #[test]
    fn export_dag_from_manifest_uses_export_id_and_parents() {
        let manifest = PackManifest::new(
            "0.9.1",
            1,
            "01PROJECT",
            "01SNAP",
            "2026-01-01T00:00:00Z",
            PackSource {
                git_branch: None,
                git_commit: None,
                hostname: None,
            },
            BTreeMap::new(),
        );
        // `new` writes an empty parent list, so drive the DAG edge directly.
        let mut manifest = manifest;
        manifest.parents = vec!["01PARENT".to_string()];
        let dag = ExportDag::from_manifest(&manifest);
        assert_eq!(dag.parents_of("01SNAP"), ["01PARENT"]);
        assert!(dag.contains("01PARENT"));
    }

    // --- Base resolution --------------------------------------------------

    #[test]
    fn resolve_base_prefers_explicit_then_ancestor_then_snapshot() {
        let dag = ExportDag::from_edges(vec![
            ("01BASE", vec![]),
            ("01OURS", vec!["01BASE"]),
            ("01THEIRS", vec!["01BASE"]),
        ]);
        let base = base_table();

        let explicit = resolve_base(
            &dag,
            Some(&base),
            Some("01OURS"),
            Some("01THEIRS"),
            false,
            |_| None,
        );
        assert_eq!(explicit.source(), BaseSource::Explicit);
        assert_eq!(explicit.base(), Some(&base));

        let ancestor = resolve_base(&dag, None, Some("01OURS"), Some("01THEIRS"), false, |id| {
            (id == "01BASE").then(base_table)
        });
        assert_eq!(ancestor.source(), BaseSource::Ancestor);
        assert_eq!(ancestor.base(), Some(&base));

        // No common ancestor, but `ours` has a local snapshot: use it.
        let disjoint = ExportDag::from_nodes(["01OURS", "01OTHER"]);
        let snapshot = resolve_base(
            &disjoint,
            None,
            Some("01OURS"),
            Some("01OTHER"),
            false,
            |id| (id == "01OURS").then(base_table),
        );
        assert_eq!(snapshot.source(), BaseSource::Snapshot);
        assert!(snapshot.base().is_some());
    }

    #[test]
    fn resolve_base_degrades_and_require_base_is_a_hard_signal() {
        let dag = ExportDag::from_nodes(["01A", "01B"]);

        let degraded = resolve_base(&dag, None, Some("01A"), Some("01B"), false, |_| None);
        assert_eq!(degraded, BaseResolution::Degraded);
        assert!(degraded.is_degraded());
        assert!(degraded.base().is_none());
        assert_eq!(degraded.source(), BaseSource::None);

        let required = resolve_base(&dag, None, Some("01A"), Some("01B"), true, |_| None);
        assert_eq!(required, BaseResolution::RequiredMissing);
        assert!(required.is_required_missing());

        let error = required_base_error();
        assert_eq!(error.code, "VALIDATION_FAILED");
        assert_eq!(error.exit_code, carryctx_core::error::ExitCode::Validation);
    }
}
