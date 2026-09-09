//! ctxpack interchange core (format `carryctx-pack-dir` v1).
//!
//! Pure domain half of CTX-0111: manifest shape, manifest validation,
//! count/cross-check validation, and the Section 4 re-anchor helpers.
//! This module performs no filesystem, Git, or SQLite I/O; directory
//! reading lives in `crate::application::interchange`.
//!
//! Fail-closed notes (see design `2026-09-09-ctxpack-export-import.md`):
//! - Unknown future `format_version` reports `UNSUPPORTED_OPERATION`.
//! - Older `format_version` reports `VALIDATION_FAILED`: v1 ships no
//!   forward migrator, so silently accepting an older layout is unsafe.
//! - Unknown `counts` keys report `VALIDATION_FAILED` (never trusted).
//! - Tables with nonzero rows that are missing from `counts` report
//!   `VALIDATION_FAILED` (an undeclared row set must not pass vacuously).
//! - `parents`/`sequences` contents are shape-checked but otherwise
//!   ignored by v1 readers (reserved for merge/DAG milestones).
//! - There is no checksum field in the v1 manifest, so a consistently
//!   shrunk bundle (rows and counts edited together) reads as a smaller
//!   valid bundle. Count validation detects skew, not consistent edits.

use crate::error::CarryCtxError;
use std::collections::BTreeMap;

/// Interchange directory format marker (v1).
pub const PACK_FORMAT: &str = "carryctx-pack-dir";

/// Current (and only) interchange format version understood by this binary.
pub const PACK_FORMAT_VERSION: u32 = 1;

/// Manifest file name inside an export directory.
pub const PACK_MANIFEST_FILE: &str = "manifest.json";

/// Project row file name inside an export directory.
pub const PACK_PROJECT_FILE: &str = "project.json";

/// `*.jsonl` table files of the v1 directory layout (Section 2), without
/// extension. A `counts` key is valid only if it names one of these tables.
pub const PACK_TABLE_FILES: &[&str] = &[
    "agents",
    "tasks",
    "task_dependencies",
    "progress_items",
    "sessions",
    "worktrees",
    "checkpoints",
    "checkpoint_corrections",
    "scopes",
    "decisions",
    "handoffs",
    "teams",
    "team_members",
    "graph_nodes",
    "graph_edges",
    "events",
    "sequences",
];

/// Informational source block of the v1 manifest. Keys stay optional so
/// older/newer writers with partial source info still validate; the block
/// itself must be a JSON object and present keys must be strings.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PackSource {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
}

/// v1 manifest (`manifest.json`). All JSON keys are `snake_case`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PackManifest {
    pub format: String,
    pub format_version: u32,
    pub carryctx_version: String,
    pub schema_version: u32,
    pub project_id: String,
    pub export_id: String,
    pub created_at: String,
    #[serde(default)]
    pub parents: Vec<String>,
    #[serde(default)]
    pub sequences: BTreeMap<String, u64>,
    pub source: PackSource,
    #[serde(default)]
    pub counts: BTreeMap<String, u64>,
}

impl PackManifest {
    /// Build a manifest for a fresh export. `counts` must map every table
    /// stem in [`PACK_TABLE_FILES`] to its exported row count (zero for
    /// empty tables); [`check_counts`] enforces coverage on the read path.
    pub fn new(
        carryctx_version: impl Into<String>,
        schema_version: u32,
        project_id: impl Into<String>,
        export_id: impl Into<String>,
        created_at: impl Into<String>,
        source: PackSource,
        counts: BTreeMap<String, u64>,
    ) -> Self {
        Self {
            format: PACK_FORMAT.to_string(),
            format_version: PACK_FORMAT_VERSION,
            carryctx_version: carryctx_version.into(),
            schema_version,
            project_id: project_id.into(),
            export_id: export_id.into(),
            created_at: created_at.into(),
            parents: Vec::new(),
            sequences: BTreeMap::new(),
            source,
            counts,
        }
    }
}

/// Validate a parsed `manifest.json` value fail-closed.
///
/// Version gating runs before full-shape validation so that bundles from a
/// newer writer always surface `UNSUPPORTED_OPERATION` (exit 10) rather
/// than a misleading `VALIDATION_FAILED`.
pub fn validate_manifest_value(value: &serde_json::Value) -> Result<PackManifest, CarryCtxError> {
    let object = value.as_object().ok_or_else(|| {
        CarryCtxError::validation_error("Pack manifest must be a JSON object.".to_string())
    })?;

    // Gate the format version first: unknown futures refuse with
    // UNSUPPORTED_OPERATION per the Section 5 error table.
    let version = object.get("format_version").and_then(|v| v.as_u64());
    match version {
        Some(v) if v > u64::from(PACK_FORMAT_VERSION) => {
            return Err(CarryCtxError::unsupported_operation(format!(
                "Pack format_version {v} is newer than supported version {PACK_FORMAT_VERSION}."
            )));
        }
        Some(v) if v < u64::from(PACK_FORMAT_VERSION) => {
            return Err(CarryCtxError::validation_error(format!(
                "Pack format_version {v} is older than supported version {PACK_FORMAT_VERSION}; no v1 migrator exists."
            )));
        }
        Some(_) => {}
        None => {
            return Err(CarryCtxError::validation_error(
                "Pack manifest is missing an integer 'format_version'.".to_string(),
            ));
        }
    }

    let manifest: PackManifest = serde_json::from_value(value.clone())
        .map_err(|e| CarryCtxError::validation_error(format!("Pack manifest is invalid: {e}")))?;

    if manifest.format != PACK_FORMAT {
        return Err(CarryCtxError::validation_error(format!(
            "Pack manifest format '{}' is not '{PACK_FORMAT}'.",
            manifest.format
        )));
    }
    if manifest.project_id.trim().is_empty() {
        return Err(CarryCtxError::validation_error(
            "Pack manifest has an empty 'project_id'.".to_string(),
        ));
    }
    if manifest.export_id.trim().is_empty() {
        return Err(CarryCtxError::validation_error(
            "Pack manifest has an empty 'export_id'.".to_string(),
        ));
    }
    if manifest.carryctx_version.trim().is_empty() {
        return Err(CarryCtxError::validation_error(
            "Pack manifest has an empty 'carryctx_version'.".to_string(),
        ));
    }
    if chrono::DateTime::parse_from_rfc3339(&manifest.created_at).is_err() {
        return Err(CarryCtxError::validation_error(
            "Pack manifest 'created_at' is not RFC 3339.".to_string(),
        ));
    }
    for key in manifest.counts.keys() {
        if !PACK_TABLE_FILES.contains(&key.as_str()) {
            return Err(CarryCtxError::validation_error(format!(
                "Pack manifest counts unknown table '{key}'."
            )));
        }
    }
    Ok(manifest)
}

/// Cross-check manifest `counts` against actual per-table row counts.
///
/// `actual` maps every table stem in [`PACK_TABLE_FILES`] to its observed
/// row count. Every declared count must match exactly, and every table
/// with nonzero rows must be declared (fail closed on undeclared rows).
pub fn check_counts(
    manifest: &PackManifest,
    actual: &BTreeMap<String, u64>,
) -> Result<(), CarryCtxError> {
    for (table, expected) in &manifest.counts {
        let observed = actual.get(table).copied().unwrap_or(0);
        if observed != *expected {
            return Err(CarryCtxError::validation_error(format!(
                "Pack count mismatch for '{table}': manifest declares {expected}, bundle holds {observed}."
            ))
            .with_details(serde_json::json!({
                "table": table,
                "expected": expected,
                "actual": observed,
            })));
        }
    }
    for table in PACK_TABLE_FILES {
        let observed = actual.get(*table).copied().unwrap_or(0);
        if observed > 0 && !manifest.counts.contains_key(*table) {
            return Err(CarryCtxError::validation_error(format!(
                "Pack manifest omits counts for '{table}' with {observed} rows."
            ))
            .with_details(serde_json::json!({
                "table": table,
                "actual": observed,
            })));
        }
    }
    Ok(())
}

/// Rewrite a `projects`-row JSON object to the target machine (Section 4
/// re-anchor policy, pure half). Only the two anchor columns change;
/// identity (`id`) and history columns are preserved verbatim.
pub fn reanchor_project(
    project: &mut serde_json::Map<String, serde_json::Value>,
    repository_root: &str,
    git_common_dir: &str,
) {
    project.insert(
        "repository_root".to_string(),
        serde_json::Value::String(repository_root.to_string()),
    );
    project.insert(
        "git_common_dir".to_string(),
        serde_json::Value::String(git_common_dir.to_string()),
    );
}

/// Split `worktrees` rows into `(kept, pruned)` per the Section 4 policy:
/// a row whose `normalized_path` does not exist at the target leaves the
/// live table (caller audits each pruned row as `worktree.pruned`).
/// Absolute paths are never compared for equality across machines; the
/// caller-supplied `exists` predicate owns all filesystem access.
/// Rows with a missing/non-string `normalized_path` are pruned, never kept.
pub fn prune_worktrees<F>(
    rows: Vec<serde_json::Value>,
    exists: F,
) -> (Vec<serde_json::Value>, Vec<serde_json::Value>)
where
    F: Fn(&str) -> bool,
{
    let mut kept = Vec::new();
    let mut pruned = Vec::new();
    for row in rows {
        let live = row
            .get("normalized_path")
            .and_then(|v| v.as_str())
            .is_some_and(&exists);
        if live {
            kept.push(row);
        } else {
            pruned.push(row);
        }
    }
    (kept, pruned)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn sample_manifest() -> PackManifest {
        PackManifest {
            format: PACK_FORMAT.to_string(),
            format_version: PACK_FORMAT_VERSION,
            carryctx_version: "0.8.1".to_string(),
            schema_version: 17,
            project_id: "01KY6ZK0TMQM5ANGZ97T68C71G".to_string(),
            export_id: "01M22DJSZX5MHJ33F2CRDQ23YD".to_string(),
            created_at: "2026-09-09T06:00:00Z".to_string(),
            parents: Vec::new(),
            sequences: BTreeMap::from([("task".to_string(), 111), ("decision".to_string(), 14)]),
            source: PackSource {
                git_branch: Some("main".to_string()),
                git_commit: Some("21951eb".to_string()),
                hostname: Some("dev-a".to_string()),
            },
            counts: BTreeMap::from([("tasks".to_string(), 2), ("events".to_string(), 3)]),
        }
    }

    fn actual_counts(tasks: u64, events: u64) -> BTreeMap<String, u64> {
        let mut actual = BTreeMap::new();
        for table in PACK_TABLE_FILES {
            actual.insert((*table).to_string(), 0);
        }
        actual.insert("tasks".to_string(), tasks);
        actual.insert("events".to_string(), events);
        actual
    }

    #[test]
    fn valid_manifest_round_trip() {
        let manifest = sample_manifest();
        let value = serde_json::to_value(&manifest).unwrap();
        // Keys stay snake_case across the JSON boundary.
        let object = value.as_object().unwrap();
        assert!(object.contains_key("format_version"));
        assert!(object.contains_key("project_id"));
        assert!(object.contains_key("created_at"));
        let back = validate_manifest_value(&value).unwrap();
        assert_eq!(back, manifest);
    }

    #[test]
    fn tampered_counts_rejected() {
        let manifest = sample_manifest();
        // Bundle holds 2 tasks but the manifest was edited to claim 104.
        let tampered = PackManifest {
            counts: BTreeMap::from([("tasks".to_string(), 104), ("events".to_string(), 3)]),
            ..manifest
        };
        let error = check_counts(&tampered, &actual_counts(2, 3)).unwrap_err();
        assert_eq!(error.code, "VALIDATION_FAILED");
    }

    #[test]
    fn undeclared_nonzero_table_rejected() {
        // Events rows exist but counts omits them: must not pass vacuously.
        let manifest = PackManifest {
            counts: BTreeMap::from([("tasks".to_string(), 2)]),
            ..sample_manifest()
        };
        let error = check_counts(&manifest, &actual_counts(2, 3)).unwrap_err();
        assert_eq!(error.code, "VALIDATION_FAILED");
    }

    #[test]
    fn future_format_version_refused() {
        let mut value = serde_json::to_value(sample_manifest()).unwrap();
        value["format_version"] = serde_json::json!(999);
        let error = validate_manifest_value(&value).unwrap_err();
        assert_eq!(error.code, "UNSUPPORTED_OPERATION");
    }

    #[test]
    fn older_format_version_refused_without_migrator() {
        let mut value = serde_json::to_value(sample_manifest()).unwrap();
        value["format_version"] = serde_json::json!(0);
        let error = validate_manifest_value(&value).unwrap_err();
        assert_eq!(error.code, "VALIDATION_FAILED");
    }

    #[test]
    fn unknown_counts_key_rejected() {
        let mut manifest = sample_manifest();
        manifest.counts.insert("locks".to_string(), 1);
        let value = serde_json::to_value(&manifest).unwrap();
        let error = validate_manifest_value(&value).unwrap_err();
        assert_eq!(error.code, "VALIDATION_FAILED");
    }

    #[test]
    fn bad_created_at_rejected() {
        let mut manifest = sample_manifest();
        manifest.created_at = "yesterday".to_string();
        let value = serde_json::to_value(&manifest).unwrap();
        let error = validate_manifest_value(&value).unwrap_err();
        assert_eq!(error.code, "VALIDATION_FAILED");
    }

    #[test]
    fn reanchor_project_rewrites_only_anchors() {
        let mut project = serde_json::json!({
            "id": "01KY6ZK0TMQM5ANGZ97T68C71G",
            "name": "demo",
            "repository_root": "/old/root",
            "git_common_dir": "/old/root/.git",
        });
        let map = project.as_object_mut().unwrap();
        reanchor_project(map, "/new/root", "/new/root/.git");
        assert_eq!(map["id"], "01KY6ZK0TMQM5ANGZ97T68C71G");
        assert_eq!(map["name"], "demo");
        assert_eq!(map["repository_root"], "/new/root");
        assert_eq!(map["git_common_dir"], "/new/root/.git");
    }

    #[test]
    fn prune_worktrees_drops_missing_paths() {
        let rows = vec![
            serde_json::json!({"id": "a", "normalized_path": "/live/wt"}),
            serde_json::json!({"id": "b", "normalized_path": "/gone/wt"}),
            serde_json::json!({"id": "c"}),
        ];
        let (kept, pruned) = prune_worktrees(rows, |path| path == "/live/wt");
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0]["id"], "a");
        assert_eq!(pruned.len(), 2);
    }
}
