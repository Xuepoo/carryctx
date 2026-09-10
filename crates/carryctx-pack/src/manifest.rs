//! ctxpack interchange manifest (format `carryctx-pack-dir` v1/v2).
//!
//! Pure domain half of CTX-0111/CTX-0139: manifest shape, manifest
//! validation, count/cross-check validation, and the Section 4 re-anchor
//! helpers. This module performs no filesystem, Git, or SQLite I/O;
//! directory reading lives in `crate::io`.
//!
//! Format v2 (merge milestone, CTX-0139) adds:
//! - `parents`: ordered export-id DAG edges (`[]` for a first export);
//! - `redacted`: publication-artifact marker (default `false`); such
//!   bundles are refused as merge sources by the merge milestone;
//! - optional `watermarks`: per-table row-count/timestamp sanity hints;
//! - the `tombstones` side table (`tombstones.jsonl`), counted like any
//!   other table, with deletion records kept out of ordinary read paths.
//!
//! v1 bundles stay readable for one release cycle after v2 writers ship.
//! They enter the current pipeline through the explicit in-memory
//! v1->v2 migrator ([`crate::migration::migrate_manifest_value`]): a v1
//! bundle becomes an implicit v2 with `parents = []`, no watermarks, and
//! no tombstones (absence without a tombstone is never a delete). The
//! `redacted` stamp is preserved through migration so a redacted
//! publication artifact cannot lose its marker.
//!
//! Fail-closed notes (see design `2026-09-10-mergeable-git-managed-state.md`):
//! - Unknown future `format_version` reports `UNSUPPORTED_OPERATION`.
//! - `format_version` below the oldest supported version reports
//!   `VALIDATION_FAILED` (no migrator exists for unknown legacy layouts).
//! - Unknown `counts` keys report `VALIDATION_FAILED` (never trusted), and
//!   a v1 manifest may not declare v2-only tables.
//! - A v2 manifest must declare `counts.tombstones`, even when zero: a
//!   missing tombstone set must be explicit, not inferred.
//! - Tables with nonzero rows that are missing from `counts` report
//!   `VALIDATION_FAILED` (an undeclared row set must not pass vacuously).
//! - v2 `parents` entries must be non-empty, unique, and never the
//!   manifest's own `export_id`; `watermarks` keys must name known tables
//!   and agree with `counts` where both are present.
//! - There is no checksum field in the manifest, so a consistently
//!   shrunk bundle (rows and counts edited together) reads as a smaller
//!   valid bundle. Count validation detects skew, not consistent edits;
//!   the SHA-256 helpers in [`crate::checksum`] cover external pipelines.

use carryctx_core::error::CarryCtxError;
use std::collections::{BTreeMap, BTreeSet};

/// Interchange directory format marker (v1/v2).
pub const PACK_FORMAT: &str = "carryctx-pack-dir";

/// Current interchange format version emitted by writers (v2).
///
/// v2 writers require the tombstone side table (schema 0018) for
/// merge-grade deletes; databases without it keep emitting v1 until the
/// storage milestone lands.
pub const PACK_FORMAT_VERSION: u32 = 2;

/// Legacy interchange format version accepted for one release cycle.
pub const PACK_FORMAT_VERSION_V1: u32 = 1;

/// Manifest file name inside an export directory.
pub const PACK_MANIFEST_FILE: &str = "manifest.json";

/// Project row file name inside an export directory.
pub const PACK_PROJECT_FILE: &str = "project.json";

/// `*.jsonl` table files of the v1 directory layout (Section 2), without
/// extension. A v1 `counts` key is valid only if it names one of these
/// tables, and every one of these files must exist in a v1 bundle.
pub const PACK_TABLE_FILES_V1: &[&str] = &[
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

/// Tables added by format v2. A v1 manifest must not declare them, and a
/// v1 bundle may omit their files entirely.
pub const PACK_V2_TABLE_FILES: &[&str] = &["tombstones"];

/// `*.jsonl` table files of the v2 directory layout: the v1 tables plus
/// the tombstone side table. A v2 `counts` key is valid only if it names
/// one of these tables.
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
    "tombstones",
];

/// Table files that a bundle at `format_version` must contain. v1 bundles
/// stay byte-compatible: v2-only tables (tombstones) are optional there.
pub fn pack_table_files(format_version: u32) -> &'static [&'static str] {
    if format_version >= PACK_FORMAT_VERSION {
        PACK_TABLE_FILES
    } else {
        PACK_TABLE_FILES_V1
    }
}

/// True when `table` must be present as `<table>.jsonl` in a bundle at
/// `format_version`.
pub fn table_file_required(format_version: u32, table: &str) -> bool {
    pack_table_files(format_version).contains(&table)
}

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

/// v2 optional per-table watermark: exported row count plus the newest
/// `updated_at`/`occurred_at` observed for the table. Watermarks are an
/// optimization and sanity check only; correctness never depends on them.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PackWatermark {
    pub rows: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_updated_at: Option<String>,
}

/// v1/v2 manifest (`manifest.json`). All JSON keys are `snake_case`.
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
    /// v2 publication-artifact marker (redaction pass ran). Absent == false.
    #[serde(default, skip_serializing_if = "is_false")]
    pub redacted: bool,
    /// v2 optional per-table sanity hints. Absent == empty.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub watermarks: BTreeMap<String, PackWatermark>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl PackManifest {
    /// Build a v2 manifest for a fresh export. `counts` must cover every
    /// table stem in [`PACK_TABLE_FILES`] (including `tombstones`, zero for
    /// empty); [`check_counts`] enforces coverage on the read path.
    pub fn new(
        carryctx_version: impl Into<String>,
        schema_version: u32,
        project_id: impl Into<String>,
        export_id: impl Into<String>,
        created_at: impl Into<String>,
        source: PackSource,
        counts: BTreeMap<String, u64>,
    ) -> Self {
        Self::build(
            PACK_FORMAT_VERSION,
            carryctx_version,
            schema_version,
            project_id,
            export_id,
            created_at,
            source,
            counts,
        )
    }

    /// Build a legacy v1 manifest. Emitted only while the local database
    /// predates the tombstone side table (schema 0018); v1 bundles carry no
    /// DAG, no redaction marker, and no tombstones.
    pub fn new_v1(
        carryctx_version: impl Into<String>,
        schema_version: u32,
        project_id: impl Into<String>,
        export_id: impl Into<String>,
        created_at: impl Into<String>,
        source: PackSource,
        counts: BTreeMap<String, u64>,
    ) -> Self {
        Self::build(
            PACK_FORMAT_VERSION_V1,
            carryctx_version,
            schema_version,
            project_id,
            export_id,
            created_at,
            source,
            counts,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build(
        format_version: u32,
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
            format_version,
            carryctx_version: carryctx_version.into(),
            schema_version,
            project_id: project_id.into(),
            export_id: export_id.into(),
            created_at: created_at.into(),
            parents: Vec::new(),
            sequences: BTreeMap::new(),
            source,
            counts,
            redacted: false,
            watermarks: BTreeMap::new(),
        }
    }

    /// Normalize a validated manifest to the current format version:
    /// v1 inputs get `parents = []`, no watermarks (v1 never carried
    /// them), and an explicit `counts.tombstones = 0` (absence is not a
    /// delete); `redacted` is preserved because a redaction stamp is
    /// security-relevant. v2 inputs are returned unchanged.
    pub fn into_current(mut self) -> Self {
        if self.format_version < PACK_FORMAT_VERSION {
            self.format_version = PACK_FORMAT_VERSION;
            self.parents.clear();
            self.watermarks.clear();
            self.counts.entry("tombstones".to_string()).or_insert(0);
        }
        self
    }
}

/// Validate a parsed `manifest.json` value fail-closed.
///
/// Version gating runs before full-shape validation so that bundles from a
/// newer writer always surface `UNSUPPORTED_OPERATION` (exit 10) rather
/// than a misleading `VALIDATION_FAILED`. v1 values validate against the
/// v1 table layout (no `tombstones`, no `watermarks`); v2 values carry the
/// full DAG/tombstone shape checks. The returned manifest keeps the
/// declared `format_version`; callers that need the current view use
/// [`crate::migration::migrate_manifest_value`] or [`PackManifest::into_current`].
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
        Some(v) if v < u64::from(PACK_FORMAT_VERSION_V1) => {
            return Err(CarryCtxError::validation_error(format!(
                "Pack format_version {v} is older than the oldest supported version {PACK_FORMAT_VERSION_V1}."
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

    let allowed_tables = pack_table_files(manifest.format_version);
    for key in manifest.counts.keys() {
        if !allowed_tables.contains(&key.as_str()) {
            return Err(CarryCtxError::validation_error(format!(
                "Pack manifest v{} counts unknown table '{key}'.",
                manifest.format_version
            )));
        }
    }

    if manifest.format_version < PACK_FORMAT_VERSION {
        // v1: parents were reserved-but-ignored, so they are not shape
        // checked; v2-only fields must not appear in a v1 manifest.
        if !manifest.watermarks.is_empty() {
            return Err(CarryCtxError::validation_error(
                "Pack manifest watermarks are a format v2 field.".to_string(),
            ));
        }
        return Ok(manifest);
    }

    // v2 shape checks.
    if !manifest.counts.contains_key("tombstones") {
        return Err(CarryCtxError::validation_error(
            "Pack manifest v2 must declare counts for 'tombstones'.".to_string(),
        ));
    }
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for parent in &manifest.parents {
        if parent.trim().is_empty() {
            return Err(CarryCtxError::validation_error(
                "Pack manifest v2 has an empty 'parents' entry.".to_string(),
            ));
        }
        if parent == &manifest.export_id {
            return Err(CarryCtxError::validation_error(
                "Pack manifest v2 lists its own 'export_id' as a parent.".to_string(),
            ));
        }
        if !seen.insert(parent.as_str()) {
            return Err(CarryCtxError::validation_error(format!(
                "Pack manifest v2 lists parent '{parent}' more than once."
            )));
        }
    }
    for (table, watermark) in &manifest.watermarks {
        if !PACK_TABLE_FILES.contains(&table.as_str()) {
            return Err(CarryCtxError::validation_error(format!(
                "Pack manifest watermarks unknown table '{table}'."
            )));
        }
        let declared = manifest.counts.get(table).copied().unwrap_or(0);
        if watermark.rows != declared {
            return Err(CarryCtxError::validation_error(format!(
                "Pack manifest watermark for '{table}' declares {} rows but counts declare {declared}.",
                watermark.rows
            )));
        }
    }
    Ok(manifest)
}

/// Cross-check manifest `counts` against actual per-table row counts.
///
/// `actual` maps every table stem in [`PACK_TABLE_FILES`] to its observed
/// row count. Every declared count must match exactly, and every table
/// with nonzero rows must be declared (fail closed on undeclared rows). A
/// v2 manifest must declare `tombstones` even when the set is empty.
pub fn check_counts(
    manifest: &PackManifest,
    actual: &BTreeMap<String, u64>,
) -> Result<(), CarryCtxError> {
    if manifest.format_version >= PACK_FORMAT_VERSION && !manifest.counts.contains_key("tombstones")
    {
        return Err(CarryCtxError::validation_error(
            "Pack manifest v2 omits counts for 'tombstones'.".to_string(),
        ));
    }
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
        let mut manifest = PackManifest::new(
            "0.9.1",
            17,
            "01KY6ZK0TMQM5ANGZ97T68C71G",
            "01M22DJSZX5MHJ33F2CRDQ23YD",
            "2026-09-09T06:00:00Z",
            PackSource {
                git_branch: Some("main".to_string()),
                git_commit: Some("21951eb".to_string()),
                hostname: Some("dev-a".to_string()),
            },
            BTreeMap::from([
                ("tasks".to_string(), 2),
                ("events".to_string(), 3),
                ("tombstones".to_string(), 0),
            ]),
        );
        manifest.sequences =
            BTreeMap::from([("task".to_string(), 111), ("decision".to_string(), 14)]);
        manifest
    }

    /// Hand-built v1 manifest value: no v2 fields, no tombstone counts.
    fn v1_value() -> serde_json::Value {
        serde_json::json!({
            "format": PACK_FORMAT,
            "format_version": 1,
            "carryctx_version": "0.8.1",
            "schema_version": 17,
            "project_id": "01KY6ZK0TMQM5ANGZ97T68C71G",
            "export_id": "01M22DJSZX5MHJ33F2CRDQ23YD",
            "created_at": "2026-09-09T06:00:00Z",
            "parents": [],
            "sequences": {"task": 111},
            "source": {"git_branch": "main"},
            "counts": {"tasks": 2, "events": 3}
        })
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
    fn current_format_version_is_v2_with_v1_read_support() {
        assert_eq!(PACK_FORMAT_VERSION, 2);
        assert_eq!(PACK_FORMAT_VERSION_V1, 1);
        assert_eq!(PACK_TABLE_FILES_V1.len(), 17);
        assert_eq!(PACK_TABLE_FILES.len(), PACK_TABLE_FILES_V1.len() + 1);
        assert_eq!(
            PACK_TABLE_FILES.last().copied(),
            Some(PACK_V2_TABLE_FILES[0])
        );
        assert!(!PACK_TABLE_FILES_V1.contains(&"tombstones"));
        assert_eq!(
            pack_table_files(PACK_FORMAT_VERSION_V1),
            PACK_TABLE_FILES_V1
        );
        assert_eq!(pack_table_files(PACK_FORMAT_VERSION), PACK_TABLE_FILES);
    }

    #[test]
    fn v2_only_tables_are_optional_for_v1_bundles() {
        assert!(!table_file_required(PACK_FORMAT_VERSION_V1, "tombstones"));
        assert!(table_file_required(PACK_FORMAT_VERSION, "tombstones"));
        assert!(table_file_required(PACK_FORMAT_VERSION_V1, "tasks"));
        assert!(table_file_required(PACK_FORMAT_VERSION, "tasks"));
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
        assert_eq!(value["format_version"], PACK_FORMAT_VERSION);
        let back = validate_manifest_value(&value).unwrap();
        assert_eq!(back, manifest);
    }

    #[test]
    fn v2_manifest_round_trips_parents_redacted_watermarks() {
        let mut manifest = sample_manifest();
        manifest.parents = vec!["01PARENT1".to_string(), "01PARENT2".to_string()];
        manifest.redacted = true;
        manifest.watermarks.insert(
            "tasks".to_string(),
            PackWatermark {
                rows: 2,
                max_updated_at: Some("2026-09-09T05:00:00Z".to_string()),
            },
        );
        let value = serde_json::to_value(&manifest).unwrap();
        assert_eq!(value["format_version"], 2);
        assert_eq!(value["redacted"], true);
        assert_eq!(value["watermarks"]["tasks"]["rows"], 2);
        let back = validate_manifest_value(&value).unwrap();
        assert_eq!(back, manifest);
    }

    #[test]
    fn v1_manifest_reads_as_declared_version() {
        let manifest = validate_manifest_value(&v1_value()).unwrap();
        assert_eq!(manifest.format_version, PACK_FORMAT_VERSION_V1);
        assert!(manifest.parents.is_empty());
        assert!(!manifest.redacted);
        assert!(manifest.watermarks.is_empty());
    }

    #[test]
    fn v1_manifest_rejects_v2_only_counts() {
        let mut value = v1_value();
        value["counts"]["tombstones"] = serde_json::json!(0);
        let error = validate_manifest_value(&value).unwrap_err();
        assert_eq!(error.code, "VALIDATION_FAILED");
    }

    #[test]
    fn v1_manifest_rejects_v2_watermarks() {
        let mut value = v1_value();
        value["watermarks"] = serde_json::json!({"tasks": {"rows": 2}});
        let error = validate_manifest_value(&value).unwrap_err();
        assert_eq!(error.code, "VALIDATION_FAILED");
    }

    #[test]
    fn v1_manifest_preserves_redacted_stamp() {
        let mut value = v1_value();
        value["redacted"] = serde_json::json!(true);
        let manifest = validate_manifest_value(&value).unwrap();
        assert!(manifest.redacted);
        let current = manifest.into_current();
        assert_eq!(current.format_version, PACK_FORMAT_VERSION);
        assert!(current.redacted);
    }

    #[test]
    fn into_current_normalizes_v1_parents_and_watermarks() {
        let mut value = v1_value();
        value["parents"] = serde_json::json!(["01STALE"]);
        let manifest = validate_manifest_value(&value).unwrap();
        let current = manifest.into_current();
        assert_eq!(current.format_version, PACK_FORMAT_VERSION);
        assert!(current.parents.is_empty());
        assert!(current.watermarks.is_empty());
    }

    #[test]
    fn v2_manifest_requires_tombstone_counts() {
        let mut value = serde_json::to_value(sample_manifest()).unwrap();
        value["counts"]
            .as_object_mut()
            .unwrap()
            .remove("tombstones");
        let error = validate_manifest_value(&value).unwrap_err();
        assert_eq!(error.code, "VALIDATION_FAILED");
    }

    #[test]
    fn parents_must_be_non_empty_strings() {
        let mut value = serde_json::to_value(sample_manifest()).unwrap();
        value["parents"] = serde_json::json!(["01OK", "   "]);
        let error = validate_manifest_value(&value).unwrap_err();
        assert_eq!(error.code, "VALIDATION_FAILED");
    }

    #[test]
    fn self_parent_rejected() {
        let mut value = serde_json::to_value(sample_manifest()).unwrap();
        let export_id = value["export_id"].as_str().unwrap().to_string();
        value["parents"] = serde_json::json!([export_id]);
        let error = validate_manifest_value(&value).unwrap_err();
        assert_eq!(error.code, "VALIDATION_FAILED");
    }

    #[test]
    fn duplicate_parents_rejected() {
        let mut value = serde_json::to_value(sample_manifest()).unwrap();
        value["parents"] = serde_json::json!(["01SAME", "01SAME"]);
        let error = validate_manifest_value(&value).unwrap_err();
        assert_eq!(error.code, "VALIDATION_FAILED");
    }

    #[test]
    fn non_string_parent_rejected() {
        let mut value = serde_json::to_value(sample_manifest()).unwrap();
        value["parents"] = serde_json::json!([42]);
        let error = validate_manifest_value(&value).unwrap_err();
        assert_eq!(error.code, "VALIDATION_FAILED");
    }

    #[test]
    fn non_bool_redacted_rejected() {
        let mut value = serde_json::to_value(sample_manifest()).unwrap();
        value["redacted"] = serde_json::json!("yes");
        let error = validate_manifest_value(&value).unwrap_err();
        assert_eq!(error.code, "VALIDATION_FAILED");
    }

    #[test]
    fn watermark_unknown_table_rejected() {
        let mut manifest = sample_manifest();
        manifest.watermarks.insert(
            "locks".to_string(),
            PackWatermark {
                rows: 0,
                max_updated_at: None,
            },
        );
        let value = serde_json::to_value(&manifest).unwrap();
        let error = validate_manifest_value(&value).unwrap_err();
        assert_eq!(error.code, "VALIDATION_FAILED");
    }

    #[test]
    fn watermark_rows_mismatch_rejected() {
        let mut manifest = sample_manifest();
        manifest.watermarks.insert(
            "tasks".to_string(),
            PackWatermark {
                rows: 99,
                max_updated_at: None,
            },
        );
        let value = serde_json::to_value(&manifest).unwrap();
        let error = validate_manifest_value(&value).unwrap_err();
        assert_eq!(error.code, "VALIDATION_FAILED");
    }

    #[test]
    fn tampered_counts_rejected() {
        let manifest = sample_manifest();
        // Bundle holds 2 tasks but the manifest was edited to claim 104.
        let tampered = PackManifest {
            counts: BTreeMap::from([
                ("tasks".to_string(), 104),
                ("events".to_string(), 3),
                ("tombstones".to_string(), 0),
            ]),
            ..manifest
        };
        let error = check_counts(&tampered, &actual_counts(2, 3)).unwrap_err();
        assert_eq!(error.code, "VALIDATION_FAILED");
    }

    #[test]
    fn tampered_tombstone_counts_rejected() {
        let mut manifest = sample_manifest();
        manifest.counts.insert("tombstones".to_string(), 7);
        let mut actual = actual_counts(2, 3);
        actual.insert("tombstones".to_string(), 1);
        let error = check_counts(&manifest, &actual).unwrap_err();
        assert_eq!(error.code, "VALIDATION_FAILED");
    }

    #[test]
    fn check_counts_requires_v2_tombstone_declaration() {
        let manifest = PackManifest {
            counts: BTreeMap::from([("tasks".to_string(), 2), ("events".to_string(), 3)]),
            ..sample_manifest()
        };
        let error = check_counts(&manifest, &actual_counts(2, 3)).unwrap_err();
        assert_eq!(error.code, "VALIDATION_FAILED");
    }

    #[test]
    fn undeclared_nonzero_table_rejected() {
        // Events rows exist but counts omits them: must not pass vacuously.
        let manifest = PackManifest {
            counts: BTreeMap::from([("tasks".to_string(), 2), ("tombstones".to_string(), 0)]),
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
    fn pre_v1_format_version_refused() {
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
