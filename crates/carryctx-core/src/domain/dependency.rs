use std::collections::{HashMap, HashSet, VecDeque};

/// Dependency kind
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyKind {
    Strong,
    Informational,
}

/// A dependency edge: task_id depends on prerequisite_id
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyEdge {
    pub task_id: String,
    pub prerequisite_id: String,
    pub kind: DependencyKind,
}

/// Detect if adding edge (task_id, prerequisite_id) would create a cycle.
/// Edge (A, B) means A depends on B. A cycle exists when prerequisite_id
/// can reach task_id through existing dependency chains.
pub fn would_create_cycle(edges: &[DependencyEdge], task_id: &str, prerequisite_id: &str) -> bool {
    if task_id == prerequisite_id {
        return true;
    }

    // Build adjacency: task -> what it depends on (prerequisites)
    let adj: HashMap<&str, Vec<&str>> = {
        let mut m: HashMap<&str, Vec<&str>> = HashMap::new();
        for e in edges {
            m.entry(e.task_id.as_str())
                .or_default()
                .push(e.prerequisite_id.as_str());
        }
        m
    };

    // BFS from prerequisite_id following dependency chains (task -> prereq)
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    queue.push_back(prerequisite_id);

    while let Some(node) = queue.pop_front() {
        if node == task_id {
            return true;
        }
        if !visited.insert(node) {
            continue;
        }
        if let Some(neighbors) = adj.get(node) {
            for &next in neighbors {
                if !visited.contains(next) {
                    queue.push_back(next);
                }
            }
        }
    }

    false
}

/// Validate a dependency edge
pub fn validate_dependency_edge(
    edges: &[DependencyEdge],
    task_id: &str,
    prerequisite_id: &str,
) -> Result<(), String> {
    if task_id == prerequisite_id {
        return Err("Self-dependency is not allowed.".into());
    }
    if would_create_cycle(edges, task_id, prerequisite_id) {
        return Err("Adding this dependency would create a cycle.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(task: &str, prereq: &str) -> DependencyEdge {
        DependencyEdge {
            task_id: task.into(),
            prerequisite_id: prereq.into(),
            kind: DependencyKind::Strong,
        }
    }

    #[test]
    fn test_self_loop_is_cycle() {
        assert!(would_create_cycle(&[], "CTX-0001", "CTX-0001"));
    }

    #[test]
    fn test_simple_dep_no_cycle() {
        assert!(!would_create_cycle(&[], "CTX-0002", "CTX-0001"));
    }

    #[test]
    fn test_direct_cycle() {
        let edges = vec![edge("CTX-0002", "CTX-0001")];
        assert!(would_create_cycle(&edges, "CTX-0001", "CTX-0002"));
    }

    #[test]
    fn test_indirect_cycle() {
        let edges = vec![edge("CTX-0002", "CTX-0001"), edge("CTX-0003", "CTX-0002")];
        assert!(would_create_cycle(&edges, "CTX-0001", "CTX-0003"));
    }

    #[test]
    fn test_no_cycle_with_branching() {
        let edges = vec![edge("CTX-0002", "CTX-0001"), edge("CTX-0003", "CTX-0001")];
        assert!(!would_create_cycle(&edges, "CTX-0004", "CTX-0002"));
    }
}
