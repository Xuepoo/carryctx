use crate::application::runtime::InvocationContext;
use crate::domain::graph::{GraphEdge, GraphNode};
use crate::error::CarryCtxError;
use crate::repository::GraphRepository;
use chrono::Utc;
use regex::Regex;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};

pub fn extract_deps_for_file(
    file_path: &str,
    repo: &GraphRepository,
    ctx: &InvocationContext,
) -> Result<Vec<GraphEdge>, CarryCtxError> {
    let content = fs::read_to_string(file_path).map_err(|e| {
        CarryCtxError::validation_error(format!("Failed to read {}: {}", file_path, e))
    })?;

    let path = Path::new(file_path);
    let mut deps = Vec::new();

    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        match ext {
            "rs" => {
                // Parse Rust dependencies
                // 1. mod sub_module;
                let mod_re = Regex::new(r"(?m)^\s*(?:pub\s+)?mod\s+([a-zA-Z0-9_]+)\s*;").unwrap();
                for cap in mod_re.captures_iter(&content) {
                    if let Some(m) = cap.get(1) {
                        deps.push(format!("{}.rs", m.as_str()));
                        deps.push(format!("{}/mod.rs", m.as_str()));
                    }
                }
                // 2. use crate::module::sub_module;
                let use_re =
                    Regex::new(r"(?m)^\s*(?:pub\s+)?use\s+crate::([a-zA-Z0-9_:]+)[^;]*;").unwrap();
                for cap in use_re.captures_iter(&content) {
                    if let Some(m) = cap.get(1) {
                        // The matched string will look like `domain::graph` or `domain::{graph, preset}`
                        // We will do a simplistic replacement. For braces, we won't fully parse them in this basic version.
                        let rel_path = m.as_str().trim_end_matches(':').replace("::", "/");
                        deps.push(format!("src/{}.rs", rel_path));
                    }
                }
            }
            "js" | "ts" | "jsx" | "tsx" => {
                // Parse JS/TS dependencies
                // 1. import ... from "./path";
                let import_re =
                    Regex::new(r#"(?m)^\s*import\s+.*from\s+['"]([^'"]+)['"]"#).unwrap();
                for cap in import_re.captures_iter(&content) {
                    if let Some(m) = cap.get(1) {
                        deps.push(m.as_str().to_string());
                    }
                }
                // 2. require("./path");
                let require_re = Regex::new(r#"(?m)require\(['"]([^'"]+)['"]\)"#).unwrap();
                for cap in require_re.captures_iter(&content) {
                    if let Some(m) = cap.get(1) {
                        deps.push(m.as_str().to_string());
                    }
                }
            }
            _ => {}
        }
    }

    let parent_dir = path.parent().unwrap_or(Path::new(""));
    let mut resolved_deps = Vec::new();

    for dep in deps {
        // Resolve path relative to the current file or project root
        let target_path = if dep.starts_with("src/") {
            PathBuf::from(&dep)
        } else {
            parent_dir.join(&dep)
        };

        // Try exact path first
        if target_path.exists() && target_path.is_file() {
            if let Some(p) = target_path.to_str() {
                resolved_deps.push(p.to_string());
            }
        } else if dep.starts_with("src/") && target_path.extension().unwrap_or_default() == "rs" {
            // For Rust, the import might include structs/functions. Walk up the path segments.
            let mut current = target_path.clone();
            while let Some(parent) = current.parent() {
                // Ignore empty parents or just "src"
                if parent.as_os_str().is_empty() || parent == Path::new("src") {
                    break;
                }
                let try_path = parent.with_extension("rs");
                if try_path.exists() {
                    resolved_deps.push(try_path.to_string_lossy().to_string());
                    break;
                }
                let try_mod_path = parent.join("mod.rs");
                if try_mod_path.exists() {
                    resolved_deps.push(try_mod_path.to_string_lossy().to_string());
                    break;
                }
                current = parent.to_path_buf();
            }
        }
    }

    // Deduplicate
    resolved_deps.sort();
    resolved_deps.dedup();

    if resolved_deps.is_empty() {
        return Ok(vec![]);
    }

    // Ensure source node exists
    let source_node = get_or_create_file_node(repo, file_path)?;

    let mut created_edges = Vec::new();
    let now = Utc::now().to_rfc3339();

    for target_path in resolved_deps {
        let target_node = get_or_create_file_node(repo, &target_path)?;

        // Check if edge already exists
        if let Ok(Some(_)) = repo.get_edge(&source_node.id, &target_node.id, "depends_on") {
            continue;
        }

        let edge = GraphEdge::new(
            &source_node.id,
            &target_node.id,
            "depends_on",
            now.clone(),
            ctx.agent.clone(),
            json!({ "extracted_by": "carryctx-cli" }),
        );

        repo.insert_edge(&edge)?;
        created_edges.push(edge);
    }

    Ok(created_edges)
}

fn get_or_create_file_node(
    repo: &GraphRepository,
    file_path: &str,
) -> Result<GraphNode, CarryCtxError> {
    if let Ok(Some(node)) = repo.get_node_by_name_and_type(file_path, "file") {
        return Ok(node);
    }

    let id = ulid::Ulid::generate().to_string();
    let now = Utc::now().to_rfc3339();
    let node = GraphNode::new(&id, "file", file_path, None, json!({}), now);

    repo.insert_node(&node)?;
    Ok(node)
}
