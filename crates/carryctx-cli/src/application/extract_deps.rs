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
                // 2. use crate::module::sub_module; (incl. brace groups)
                let use_re =
                    Regex::new(r"(?m)^\s*(?:pub(?:\s*\([^)]*\))?\s+)?use\s+crate::([^;]+);")
                        .unwrap();
                for cap in use_re.captures_iter(&content) {
                    if let Some(m) = cap.get(1) {
                        for logical in expand_crate_use_tree(m.as_str()) {
                            // Never emit brace/glob fragments as paths.
                            if logical.is_empty()
                                || logical.contains('{')
                                || logical.contains('}')
                                || logical.contains('*')
                            {
                                continue;
                            }
                            let rel_path = logical.replace("::", "/");
                            deps.push(format!("src/{}.rs", rel_path));
                            deps.push(format!("src/{}/mod.rs", rel_path));
                        }
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

/// Expand a `crate::...` use tree (the text after `crate::`, up to `;`)
/// into individual `::`-separated module paths.
///
/// Handles brace groups (`foo::{bar, baz}`), nested groups
/// (`a::{b::{c, d}, e}`), `self`/`Self` (maps to the base path), globs
/// (`foo::*` maps to `foo`), and `as` aliases (stripped). Trait or symbol
/// suffixes (e.g. `foo::Bar`) are kept as-is so the existing walk-up
/// resolution can map them to the parent module file; anything that cannot
/// resolve is ignored downstream. Never returns paths containing braces.
fn expand_crate_use_tree(tree: &str) -> Vec<String> {
    let tree = tree.trim();
    if tree.is_empty() {
        return Vec::new();
    }
    // No braces: single path.
    if !tree.contains('{') {
        return normalize_simple_path(tree).into_iter().collect();
    }
    let open = match tree.find('{') {
        Some(i) => i,
        None => return normalize_simple_path(tree).into_iter().collect(),
    };
    let close = match find_matching_brace(tree, open) {
        Some(i) => i,
        // Malformed (unbalanced): do not emit brace fragments.
        None => return Vec::new(),
    };
    let base = tree[..open].trim().trim_end_matches(':').trim().to_string();
    let inside = &tree[open + 1..close];
    let mut out = Vec::new();
    for part in split_top_level(inside) {
        let item = strip_alias(&part);
        if item.is_empty() {
            continue;
        }
        if item == "self" || item == "Self" {
            if !base.is_empty() {
                out.push(base.clone());
            }
            continue;
        }
        if item == "*" {
            if !base.is_empty() {
                out.push(base.clone());
            }
            continue;
        }
        // `self::sub` inside a group refers to the base path.
        let item = if let Some(rest) = item
            .strip_prefix("self::")
            .or_else(|| item.strip_prefix("Self::"))
        {
            if base.is_empty() {
                rest.to_string()
            } else if rest.is_empty() {
                base.clone()
            } else {
                format!("{base}::{rest}")
            }
        } else if base.is_empty() {
            item.clone()
        } else {
            format!("{base}::{item}")
        };
        // Recurse: nested groups unnest, plain paths normalize.
        out.extend(expand_crate_use_tree(&item));
    }
    out.sort();
    out.dedup();
    out
}

/// Strip an `as` alias (`bar as baz` -> `bar`, `bar as _` -> `bar`).
fn strip_alias(part: &str) -> String {
    let part = part.trim();
    if part.is_empty() {
        return String::new();
    }
    let tokens: Vec<&str> = part.split_whitespace().collect();
    if let Some(pos) = tokens.iter().position(|t| *t == "as") {
        tokens[..pos].join(" ").trim().to_string()
    } else {
        part.to_string()
    }
}

/// Normalize a brace-free use path into a module path, or `None` to skip.
fn normalize_simple_path(path: &str) -> Option<String> {
    let path = strip_alias(path);
    let path = path.trim().trim_end_matches(':').trim().to_string();
    if path.is_empty() {
        return None;
    }
    if path == "*" || path == "_" || path == "self" || path == "Self" {
        // Crate-root glob/self: no file mapping.
        return None;
    }
    // Reject anything that is not a plain module path.
    if !path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':' || c == '*')
    {
        return None;
    }
    if path.contains('*') {
        // Glob: map `foo::*` (or deeper) to the base module.
        let base = path.split('*').next().unwrap_or("").trim_end_matches(':');
        let base = base.trim().trim_end_matches(':').trim();
        if base.is_empty() {
            return None;
        }
        return Some(base.to_string());
    }
    if let Some(base) = path
        .strip_suffix("::self")
        .or_else(|| path.strip_suffix("::Self"))
    {
        if base.is_empty() {
            return None;
        }
        return Some(base.to_string());
    }
    if path.contains('{') || path.contains('}') {
        return None;
    }
    Some(path)
}

/// Find the byte index of the `}` matching the `{` at `open`.
fn find_matching_brace(s: &str, open: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth = 0usize;
    for (i, b) in bytes.iter().enumerate().skip(open) {
        if *b == b'{' {
            depth += 1;
        } else if *b == b'}' {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

/// Split a brace-group body by commas at the top nesting level.
fn split_top_level(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut current = String::new();
    for c in s.chars() {
        match c {
            '{' => {
                depth += 1;
                current.push(c);
            }
            '}' => {
                depth = depth.saturating_sub(1);
                current.push(c);
            }
            ',' if depth == 0 => {
                parts.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(c),
        }
    }
    if !current.trim().is_empty() {
        parts.push(current.trim().to_string());
    }
    parts
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

#[cfg(test)]
mod tests {
    use super::*;

    /// CTX-0169 / issue #192: brace-grouped imports must expand into
    /// individual paths; no emitted dep may contain a literal brace.
    #[test]
    fn brace_grouped_imports_expand_to_individual_paths() {
        let expanded = expand_crate_use_tree("foo::{bar, baz}");
        assert!(
            expanded.contains(&"foo::bar".to_string()),
            "expected foo::bar in {expanded:?}"
        );
        assert!(
            expanded.contains(&"foo::baz".to_string()),
            "expected foo::baz in {expanded:?}"
        );

        let with_self = expand_crate_use_tree("bar::{self, sub}");
        assert!(
            with_self.contains(&"bar".to_string()),
            "self must map to base path: {with_self:?}"
        );
        assert!(
            with_self.contains(&"bar::sub".to_string()),
            "expected bar::sub in {with_self:?}"
        );

        for path in expanded.iter().chain(with_self.iter()) {
            assert!(
                !path.contains('{') && !path.contains('}'),
                "dep must not contain braces: {path}"
            );
        }
    }

    /// Nested groups, aliases, globs, and trait-style suffixes must unnest
    /// gracefully without ever emitting brace paths.
    #[test]
    fn brace_expansion_handles_nested_groups_aliases_and_globs() {
        let nested = expand_crate_use_tree("a::{b::{c, d}, e}");
        for expected in ["a::b::c", "a::b::d", "a::e"] {
            assert!(
                nested.contains(&expected.to_string()),
                "expected {expected} in {nested:?}"
            );
        }

        let aliased = expand_crate_use_tree("foo::{bar as b, baz}");
        assert!(
            aliased.contains(&"foo::bar".to_string()),
            "alias must strip to path: {aliased:?}"
        );
        assert!(
            aliased.contains(&"foo::baz".to_string()),
            "expected foo::baz in {aliased:?}"
        );
        assert!(
            !aliased.iter().any(|p| p.contains(" as ")),
            "no alias fragments: {aliased:?}"
        );

        // `Self` maps to the base path like `self`; trait suffixes are kept
        // for the walk-up resolution to map to the parent module file.
        let traits = expand_crate_use_tree("foo::{Self, Bar}");
        assert!(
            traits.contains(&"foo".to_string()),
            "Self must map to base: {traits:?}"
        );
        assert!(
            traits.contains(&"foo::Bar".to_string()),
            "trait suffix kept for walk-up: {traits:?}"
        );

        let glob = expand_crate_use_tree("foo::*");
        assert_eq!(glob, vec!["foo".to_string()], "glob maps to base: {glob:?}");

        let root_group = expand_crate_use_tree("{foo, bar}");
        assert!(
            root_group.contains(&"foo".to_string()) && root_group.contains(&"bar".to_string()),
            "crate-root group expands: {root_group:?}"
        );

        for path in nested
            .iter()
            .chain(aliased.iter())
            .chain(traits.iter())
            .chain(glob.iter())
            .chain(root_group.iter())
        {
            assert!(
                !path.contains('{') && !path.contains('}'),
                "dep must not contain braces: {path}"
            );
        }
    }
}
