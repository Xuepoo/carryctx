use std::collections::{HashSet, VecDeque};
use std::io::Write;
use std::process::{Command, Stdio};

use crate::domain::graph::{GraphEdge, GraphNode};
use crate::error::CarryCtxError;
use crate::repository::GraphRepository;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphExportFormat {
    Mermaid,
    Dot,
    Ascii,
    Json,
}

impl std::str::FromStr for GraphExportFormat {
    type Err = CarryCtxError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "mermaid" | "mmd" => Ok(Self::Mermaid),
            "dot" | "graphviz" => Ok(Self::Dot),
            "ascii" | "txt" => Ok(Self::Ascii),
            "json" => Ok(Self::Json),
            _ => Err(CarryCtxError::invalid_arguments(format!(
                "Unsupported export format '{s}'. Supported: mermaid, dot, ascii, json"
            ))),
        }
    }
}

fn sanitize_mermaid_id(id: &str) -> String {
    format!("node_{}", id.replace(['-', '.', '/', ':', ' '], "_"))
}

pub fn render_mermaid(nodes: &[GraphNode], edges: &[GraphEdge]) -> String {
    let mut out = String::from("graph TD\n");

    for node in nodes {
        let safe_id = sanitize_mermaid_id(&node.id);
        let safe_name = node.name.replace('"', "\\\"");
        match node.node_type.as_str() {
            "task" => {
                out.push_str(&format!("    {safe_id}[\"📋 {}\"]\n", safe_name));
            }
            "file" => {
                out.push_str(&format!("    {safe_id}[\"📄 {}\"]\n", safe_name));
            }
            "module" => {
                out.push_str(&format!("    {safe_id}[\"📦 {}\"]\n", safe_name));
            }
            "agent" => {
                out.push_str(&format!("    {safe_id}[\"🤖 {}\"]\n", safe_name));
            }
            _ => {
                out.push_str(&format!(
                    "    {safe_id}[\"{}: {}\"]\n",
                    node.node_type, safe_name
                ));
            }
        }
    }

    for edge in edges {
        let src_id = sanitize_mermaid_id(&edge.source_id);
        let tgt_id = sanitize_mermaid_id(&edge.target_id);
        let rel = &edge.relation_type;
        out.push_str(&format!("    {src_id} -->|{rel}| {tgt_id}\n"));
    }

    out
}

pub fn render_dot(nodes: &[GraphNode], edges: &[GraphEdge]) -> String {
    let mut out = String::from("digraph ContextGraph {\n");
    out.push_str("    rankdir=LR;\n");
    out.push_str("    node [shape=box, style=\"filled,rounded\", fillcolor=\"#f8f9fa\", fontname=\"Helvetica\"];\n");

    for node in nodes {
        let safe_name = node.name.replace('"', "\\\"");
        let label = format!("{}: {}", node.node_type, safe_name);
        out.push_str(&format!("    \"{}\" [label=\"{}\"];\n", node.id, label));
    }

    for edge in edges {
        out.push_str(&format!(
            "    \"{}\" -> \"{}\" [label=\"{}\"];\n",
            edge.source_id, edge.target_id, edge.relation_type
        ));
    }

    out.push_str("}\n");
    out
}

pub fn render_ascii(mermaid_content: &str) -> Result<String, CarryCtxError> {
    let mut child = Command::new("mermaid-ascii")
        .arg("-f")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            CarryCtxError::unsupported_operation(format!("Failed to execute mermaid-ascii: {e}"))
        })?;

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(mermaid_content.as_bytes());
    }

    let output = child.wait_with_output().map_err(|e| {
        CarryCtxError::unsupported_operation(format!("Failed to read mermaid-ascii output: {e}"))
    })?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let err_msg = String::from_utf8_lossy(&output.stderr);
        Err(CarryCtxError::unsupported_operation(format!(
            "mermaid-ascii failed: {err_msg}"
        )))
    }
}

pub fn filter_subgraph_by_focus(
    all_nodes: Vec<GraphNode>,
    all_edges: Vec<GraphEdge>,
    focus: &str,
    depth: usize,
) -> (Vec<GraphNode>, Vec<GraphEdge>) {
    let focus_node = all_nodes
        .iter()
        .find(|n| n.id == focus || n.name == focus || n.name.ends_with(focus));

    let start_node = match focus_node {
        Some(n) => n,
        None => return (all_nodes, all_edges),
    };

    let mut visited_nodes: HashSet<String> = HashSet::new();
    visited_nodes.insert(start_node.id.clone());

    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    queue.push_back((start_node.id.clone(), 0));

    while let Some((curr_id, curr_depth)) = queue.pop_front() {
        if curr_depth >= depth {
            continue;
        }

        for edge in &all_edges {
            let next_id = if edge.source_id == curr_id {
                Some(&edge.target_id)
            } else if edge.target_id == curr_id {
                Some(&edge.source_id)
            } else {
                None
            };

            if let Some(nid) = next_id {
                if visited_nodes.insert(nid.clone()) {
                    queue.push_back((nid.clone(), curr_depth + 1));
                }
            }
        }
    }

    let filtered_nodes: Vec<GraphNode> = all_nodes
        .into_iter()
        .filter(|n| visited_nodes.contains(&n.id))
        .collect();

    let filtered_edges: Vec<GraphEdge> = all_edges
        .into_iter()
        .filter(|e| visited_nodes.contains(&e.source_id) && visited_nodes.contains(&e.target_id))
        .collect();

    (filtered_nodes, filtered_edges)
}

pub fn compact_graph_by_module(
    nodes: Vec<GraphNode>,
    edges: Vec<GraphEdge>,
) -> (Vec<GraphNode>, Vec<GraphEdge>) {
    fn extract_module(name: &str) -> String {
        let clean = name.strip_prefix("./").unwrap_or(name);
        let parts: Vec<&str> = clean.split('/').collect();
        if parts.len() > 1 {
            format!("{}/{}", parts[0], parts[1])
        } else {
            parts[0].to_string()
        }
    }

    let mut mod_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut mod_nodes: std::collections::HashMap<String, GraphNode> =
        std::collections::HashMap::new();

    let now = chrono::Utc::now().to_rfc3339();

    for n in &nodes {
        let mod_name = extract_module(&n.name);
        mod_map.insert(n.id.clone(), mod_name.clone());

        mod_nodes
            .entry(mod_name.clone())
            .or_insert_with(|| GraphNode {
                id: format!("mod_{}", mod_name.replace(['/', '.', '-'], "_")),
                node_type: "module".into(),
                name: mod_name,
                description: None,
                metadata: serde_json::json!({}),
                created_at: now.clone(),
                updated_at: now.clone(),
            });
    }

    let mut edge_set: HashSet<(String, String, String)> = HashSet::new();
    let mut compact_edges = Vec::new();

    for e in &edges {
        if let (Some(src_mod), Some(tgt_mod)) =
            (mod_map.get(&e.source_id), mod_map.get(&e.target_id))
        {
            if src_mod != tgt_mod {
                let src_id = format!("mod_{}", src_mod.replace(['/', '.', '-'], "_"));
                let tgt_id = format!("mod_{}", tgt_mod.replace(['/', '.', '-'], "_"));
                let key = (src_id.clone(), tgt_id.clone(), e.relation_type.clone());

                if edge_set.insert(key) {
                    compact_edges.push(GraphEdge {
                        source_id: src_id,
                        target_id: tgt_id,
                        relation_type: e.relation_type.clone(),
                        created_at: now.clone(),
                        created_by: e.created_by.clone(),
                        metadata: serde_json::json!({}),
                    });
                }
            }
        }
    }

    (mod_nodes.into_values().collect(), compact_edges)
}

pub fn export_graph(
    repo: &GraphRepository,
    format: GraphExportFormat,
    node_type_filter: Option<&str>,
    focus: Option<&str>,
    depth: usize,
    compact: bool,
) -> Result<String, CarryCtxError> {
    let (mut nodes, mut edges) = match node_type_filter {
        Some(nt) => repo.list_graph_filtered(nt)?,
        None => repo.list_full_graph()?,
    };

    if let Some(f_node) = focus {
        let (f_nodes, f_edges) = filter_subgraph_by_focus(nodes, edges, f_node, depth);
        nodes = f_nodes;
        edges = f_edges;
    }

    if compact {
        let (c_nodes, c_edges) = compact_graph_by_module(nodes, edges);
        nodes = c_nodes;
        edges = c_edges;
    }

    match format {
        GraphExportFormat::Mermaid => Ok(render_mermaid(&nodes, &edges)),
        GraphExportFormat::Dot => Ok(render_dot(&nodes, &edges)),
        GraphExportFormat::Ascii => {
            let mmd = render_mermaid(&nodes, &edges);
            render_ascii(&mmd)
        }
        GraphExportFormat::Json => {
            let json_val = serde_json::json!({
                "nodes": nodes,
                "edges": edges,
            });
            serde_json::to_string_pretty(&json_val).map_err(|e| {
                CarryCtxError::database_error(format!("Failed to serialize graph JSON: {e}"))
            })
        }
    }
}

pub fn render_image_to_file(
    content: &str,
    format: GraphExportFormat,
    output_path: &str,
) -> Result<(), CarryCtxError> {
    let lower_out = output_path.to_lowercase();
    let is_png = lower_out.ends_with(".png");
    let is_svg = lower_out.ends_with(".svg");

    if !is_png && !is_svg {
        std::fs::write(output_path, content).map_err(|e| {
            CarryCtxError::validation_error(format!("Failed to write export file: {e}"))
        })?;
        return Ok(());
    }

    match format {
        GraphExportFormat::Dot => {
            let fmt_arg = if is_png { "-Tpng" } else { "-Tsvg" };
            let mut child = Command::new("dot")
                .arg(fmt_arg)
                .arg("-o")
                .arg(output_path)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| {
                    CarryCtxError::unsupported_operation(format!("Failed to run dot command: {e}"))
                })?;

            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(content.as_bytes());
            }

            let status = child.wait().map_err(|e| {
                CarryCtxError::unsupported_operation(format!("Failed waiting for dot: {e}"))
            })?;

            if !status.success() {
                return Err(CarryCtxError::unsupported_operation(
                    "dot CLI image rendering failed".to_string(),
                ));
            }
        }
        GraphExportFormat::Mermaid | GraphExportFormat::Ascii | GraphExportFormat::Json => {
            let tmp_mmd = format!("{}.tmp.mmd", output_path);
            let mmd_content = if format == GraphExportFormat::Mermaid {
                content.to_string()
            } else {
                content.to_string()
            };
            std::fs::write(&tmp_mmd, mmd_content).map_err(|e| {
                CarryCtxError::validation_error(format!("Failed to write tmp mmd file: {e}"))
            })?;

            let status = Command::new("mmdc")
                .arg("-i")
                .arg(&tmp_mmd)
                .arg("-o")
                .arg(output_path)
                .status()
                .map_err(|e| {
                    CarryCtxError::unsupported_operation(format!("Failed to run mmdc command: {e}"))
                })?;

            let _ = std::fs::remove_file(&tmp_mmd);

            if !status.success() {
                return Err(CarryCtxError::unsupported_operation(
                    "mmdc CLI image rendering failed".to_string(),
                ));
            }
        }
    }

    Ok(())
}
