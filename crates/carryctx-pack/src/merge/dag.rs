//! Pure merge-base selection over the export-id DAG (design
//! `2026-09-10-mergeable-git-managed-state.md` §2.1).
//!
//! This is the base-selection half of the merge engine: given the locally
//! known snapshot nodes (export id plus ordered parent export ids) and the
//! export ids of `ours` (the live database's last export) and `theirs` (the
//! incoming bundle), it returns the newest common ancestor. It performs no
//! I/O: loading snapshot nodes from `carryctx-snapshots` trailers or the local
//! snapshot cache, and materializing the base rows, are caller concerns
//! (CTX-0142/CTX-0145). When no common ancestor exists the caller runs the
//! degraded two-way merge that [`super::merge_tables`] produces for
//! `base = None`.

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

/// Return the newest common ancestor export id of `ours` and `theirs`, or
/// `None` when the histories share no ancestor (a degraded two-way merge) or
/// the DAG is cyclic (fail closed: a corrupt DAG never yields a base).
///
/// Each id is treated as an ancestor of itself, so a linear history where one
/// point is an ancestor of the other returns that ancestor, and identical ids
/// return the id itself. Parent ids referenced but not present in `nodes` are
/// still reachable ancestors (the caller may know an edge before it has the
/// snapshot row). When several incomparable common ancestors exist
/// (criss-cross), the greatest export id is returned so the choice is
/// deterministic and independent of argument order.
pub fn select_merge_base(nodes: &[SnapshotNode], ours: &str, theirs: &str) -> Option<String> {
    let parents: BTreeMap<&str, &[String]> = nodes
        .iter()
        .map(|node| (node.export_id.as_str(), node.parents.as_slice()))
        .collect();
    let ours_ancestors = ancestors(&parents, ours)?;
    let theirs_ancestors = ancestors(&parents, theirs)?;
    ours_ancestors
        .intersection(&theirs_ancestors)
        .max()
        .cloned()
}

/// Transitive parent closure of `start` (including `start` itself). Returns
/// `None` if a cycle is reachable, so a corrupt DAG degrades instead of
/// looping or silently picking a bogus base.
fn ancestors(parents: &BTreeMap<&str, &[String]>, start: &str) -> Option<BTreeSet<String>> {
    let mut result = BTreeSet::new();
    if visit(
        start,
        parents,
        &mut BTreeSet::new(),
        &mut BTreeSet::new(),
        &mut result,
    ) {
        Some(result)
    } else {
        None
    }
}

fn visit(
    node: &str,
    parents: &BTreeMap<&str, &[String]>,
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
        for parent in *children {
            if !visit(parent, parents, on_path, done, result) {
                return false;
            }
        }
    }
    on_path.remove(node);
    done.insert(node.to_string());
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(export_id: &str, parents: &[&str]) -> SnapshotNode {
        SnapshotNode::new(
            export_id,
            parents.iter().map(|parent| parent.to_string()).collect(),
        )
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
}
