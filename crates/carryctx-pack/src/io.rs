//! ctxpack directory-layout reader/writer (format `carryctx-pack-dir` v1/v2).
//!
//! Filesystem half of CTX-0111/CTX-0139: reads `<export-dir>/` per Section 2
//! (`manifest.json`, `project.json`, one `*.jsonl` per table), counts rows,
//! and runs the pure [`crate::manifest`] validation fail-closed.
//! No database writes happen here; this module only reads/writes the bundle
//! directory.
//!
//! v1 compatibility: v1 bundles enter the pipeline through the in-memory
//! migrator [`crate::migration::migrate_manifest_value`]. Their v2-only
//! table file (`tombstones.jsonl`) is optional — absence is not a delete —
//! while a v2 bundle must carry it like any other table. A v1 bundle that
//! *does* ship tombstone rows fails count validation (undeclared row set),
//! never silently drops them.

use crate::manifest::{
    self, PACK_MANIFEST_FILE, PACK_PROJECT_FILE, PACK_TABLE_FILES, pack_table_files,
    table_file_required,
};
use crate::migration;
use carryctx_core::error::CarryCtxError;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// A validated export directory: manifest, project row, and per-table rows.
/// `events.jsonl` rows are parsed for validation; callers that persist them
/// must preserve stored payloads verbatim at write time (audit is
/// append-only and historical payloads keep their original casing).
#[derive(Debug, Clone)]
pub struct PackBundle {
    pub dir: PathBuf,
    /// Manifest normalized to the current format version: v1 bundles were
    /// migrated in memory (`parents = []`, no watermarks, zero tombstones;
    /// a `redacted` stamp is preserved). The on-disk version is kept in
    /// [`PackBundle::source_format_version`].
    pub manifest: manifest::PackManifest,
    /// `format_version` declared by the bundle on disk (1 or 2).
    pub source_format_version: u32,
    pub project: serde_json::Value,
    pub tables: BTreeMap<String, Vec<serde_json::Value>>,
}

impl PackBundle {
    /// Row count per table stem, covering every [`PACK_TABLE_FILES`]
    /// entry (zero for empty tables).
    pub fn actual_counts(&self) -> BTreeMap<String, u64> {
        let mut counts = BTreeMap::new();
        for table in PACK_TABLE_FILES {
            let len = self
                .tables
                .get(*table)
                .map(|rows| rows.len() as u64)
                .unwrap_or(0);
            counts.insert((*table).to_string(), len);
        }
        counts
    }
}

/// Read and validate an export directory fail-closed.
///
/// Error mapping (Section 5): missing/invalid manifest, missing table
/// files, malformed JSONL rows, and count skew report `VALIDATION_FAILED`
/// (exit 8); a newer `format_version` reports `UNSUPPORTED_OPERATION`
/// (exit 10). Nothing is written; the bundle directory is only read.
pub fn read_bundle(dir: &Path) -> Result<PackBundle, CarryCtxError> {
    if !dir.is_dir() {
        return Err(CarryCtxError::validation_error(format!(
            "Pack directory '{}' does not exist.",
            dir.display()
        )));
    }

    let manifest_text = fs::read_to_string(dir.join(PACK_MANIFEST_FILE)).map_err(|_| {
        CarryCtxError::validation_error(format!(
            "Pack manifest '{}/{}' is missing.",
            dir.display(),
            PACK_MANIFEST_FILE
        ))
    })?;
    let manifest_value: serde_json::Value = serde_json::from_str(&manifest_text)
        .map_err(|e| CarryCtxError::validation_error(format!("Pack manifest is invalid: {e}")))?;
    // Version gating inside the migrator runs before shape checks so newer
    // writers surface UNSUPPORTED_OPERATION; v1 becomes an implicit v2.
    let migrated = migration::migrate_manifest_value(&manifest_value)?;
    let manifest = migrated.manifest;
    let source_format_version = migrated.source_format_version;

    let project_text = fs::read_to_string(dir.join(PACK_PROJECT_FILE)).map_err(|_| {
        CarryCtxError::validation_error(format!(
            "Pack file '{}/{}' is missing.",
            dir.display(),
            PACK_PROJECT_FILE
        ))
    })?;
    let project: serde_json::Value = serde_json::from_str(&project_text).map_err(|e| {
        CarryCtxError::validation_error(format!("Pack project row is invalid: {e}"))
    })?;
    if !project.is_object() {
        return Err(CarryCtxError::validation_error(
            "Pack project row must be a JSON object.".to_string(),
        ));
    }

    let mut tables = BTreeMap::new();
    for table in PACK_TABLE_FILES {
        let path = dir.join(format!("{table}.jsonl"));
        if !table_file_required(source_format_version, table) && !path.exists() {
            // v1 bundles predate the v2-only side tables: absent == empty.
            tables.insert((*table).to_string(), Vec::new());
            continue;
        }
        let rows = read_table_file(dir, table)?;
        tables.insert((*table).to_string(), rows);
    }

    let bundle = PackBundle {
        dir: dir.to_path_buf(),
        manifest,
        source_format_version,
        project,
        tables,
    };
    manifest::check_counts(&bundle.manifest, &bundle.actual_counts())?;
    Ok(bundle)
}

/// Read one `<table>.jsonl` file. The file must exist (a missing table
/// file refuses the bundle); empty/whitespace-only lines are skipped so a
/// trailing newline is not a row, while any other malformed line refuses
/// the bundle fail-closed.
pub fn read_table_file(dir: &Path, table: &str) -> Result<Vec<serde_json::Value>, CarryCtxError> {
    let path = dir.join(format!("{table}.jsonl"));
    let text = fs::read_to_string(&path).map_err(|_| {
        CarryCtxError::validation_error(format!(
            "Pack file '{}/{}' is missing.",
            dir.display(),
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| format!("{table}.jsonl"))
        ))
    })?;
    let mut rows = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let row: serde_json::Value = serde_json::from_str(line).map_err(|e| {
            CarryCtxError::validation_error(format!(
                "Pack file '{table}.jsonl' line {} is invalid: {e}",
                index + 1
            ))
        })?;
        if !row.is_object() {
            return Err(CarryCtxError::validation_error(format!(
                "Pack file '{table}.jsonl' line {} must be a JSON object.",
                index + 1
            )));
        }
        rows.push(row);
    }
    Ok(rows)
}

/// Write one `<table>.jsonl` file (one JSON object per line, `\\n`
/// terminated, trailing newline is not a row on read).
pub fn write_table_file(
    dir: &Path,
    table: &str,
    rows: &[serde_json::Value],
) -> Result<(), CarryCtxError> {
    let mut text = String::new();
    for row in rows {
        text.push_str(&serde_json::to_string(row).map_err(|e| {
            CarryCtxError::io_error(format!("Failed to encode '{table}.jsonl' row: {e}"))
        })?);
        text.push('\n');
    }
    fs::write(dir.join(format!("{table}.jsonl")), text).map_err(|e| {
        CarryCtxError::io_error(format!("Failed to write pack file '{table}.jsonl': {e}"))
    })?;
    Ok(())
}

/// Write a full bundle directory (`manifest.json`, `project.json`, one
/// `*.jsonl` per table file of the manifest's format version: v1 keeps the
/// 17-file layout, v2 adds `tombstones.jsonl`). Re-validates by re-reading
/// via [`read_bundle`] before returning.
pub fn write_bundle(
    out_dir: &Path,
    manifest: &manifest::PackManifest,
    project: &serde_json::Value,
    tables: &BTreeMap<String, Vec<serde_json::Value>>,
) -> Result<(), CarryCtxError> {
    fs::create_dir_all(out_dir).map_err(|e| {
        CarryCtxError::io_error(format!(
            "Failed to create export directory '{}': {e}",
            out_dir.display()
        ))
    })?;
    fs::write(
        out_dir.join(PACK_MANIFEST_FILE),
        serde_json::to_string_pretty(manifest)
            .map_err(|e| CarryCtxError::io_error(format!("Failed to encode manifest: {e}")))?,
    )
    .map_err(|e| {
        CarryCtxError::io_error(format!("Failed to write pack file 'manifest.json': {e}"))
    })?;
    fs::write(
        out_dir.join(PACK_PROJECT_FILE),
        serde_json::to_string_pretty(project)
            .map_err(|e| CarryCtxError::io_error(format!("Failed to encode project row: {e}")))?,
    )
    .map_err(|e| {
        CarryCtxError::io_error(format!("Failed to write pack file 'project.json': {e}"))
    })?;
    for table in pack_table_files(manifest.format_version) {
        let rows = tables.get(*table).map(|v| v.as_slice()).unwrap_or(&[]);
        write_table_file(out_dir, table, rows)?;
    }
    // Re-read through the validator: bytes on disk must parse and match
    // the manifest (v1 manifests are compared in their migrated v2 view).
    let bundle = read_bundle(out_dir)?;
    manifest::check_counts(&bundle.manifest, &bundle.actual_counts())?;
    if bundle.manifest != manifest.clone().into_current() {
        return Err(CarryCtxError::validation_error(
            "Exported manifest does not round-trip; retry the export.".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{
        PACK_FORMAT_VERSION, PACK_FORMAT_VERSION_V1, PACK_TABLE_FILES_V1, PackManifest, PackSource,
    };

    fn sample_manifest(tasks: u64, events: u64) -> PackManifest {
        PackManifest::new(
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
                ("tasks".to_string(), tasks),
                ("events".to_string(), events),
                ("tombstones".to_string(), 0),
            ]),
        )
    }

    /// Write raw bundle files for a given manifest. `tables` selects the
    /// file list (v1 layout for a v1 manifest).
    fn write_bundle_raw(
        root: &Path,
        manifest: &PackManifest,
        task_rows: &[serde_json::Value],
        event_rows: &[serde_json::Value],
        tombstone_rows: &[serde_json::Value],
    ) {
        fs::write(
            root.join(PACK_MANIFEST_FILE),
            serde_json::to_string_pretty(manifest).unwrap(),
        )
        .unwrap();
        fs::write(
            root.join(PACK_PROJECT_FILE),
            r#"{"id":"01KY6ZK0TMQM5ANGZ97T68C71G","name":"demo"}"#,
        )
        .unwrap();
        for table in pack_table_files(manifest.format_version) {
            let rows: &[serde_json::Value] = match *table {
                "tasks" => task_rows,
                "events" => event_rows,
                "tombstones" => tombstone_rows,
                _ => &[],
            };
            write_table_file(root, table, rows).unwrap();
        }
    }

    fn two_tasks() -> Vec<serde_json::Value> {
        vec![
            serde_json::json!({"id": "01T1", "title": "one"}),
            serde_json::json!({"id": "01T2", "title": "two"}),
        ]
    }

    #[test]
    fn valid_bundle_round_trip() {
        let root = tempfile::tempdir().unwrap();
        let events = vec![serde_json::json!({"id": "01E1"})];
        write_bundle_raw(
            root.path(),
            &sample_manifest(2, 1),
            &two_tasks(),
            &events,
            &[],
        );
        let bundle = read_bundle(root.path()).unwrap();
        assert_eq!(bundle.source_format_version, PACK_FORMAT_VERSION);
        assert_eq!(bundle.manifest.format_version, PACK_FORMAT_VERSION);
        assert_eq!(bundle.manifest.project_id, "01KY6ZK0TMQM5ANGZ97T68C71G");
        assert_eq!(bundle.tables["tasks"].len(), 2);
        assert_eq!(bundle.tables["events"].len(), 1);
        assert_eq!(bundle.actual_counts()["tasks"], 2);
        assert_eq!(bundle.actual_counts()["tombstones"], 0);
    }

    #[test]
    fn v1_bundle_without_tombstones_file_reads_as_implicit_v2() {
        let root = tempfile::tempdir().unwrap();
        let mut manifest = PackManifest::new_v1(
            "0.8.1",
            17,
            "01KY6ZK0TMQM5ANGZ97T68C71G",
            "01M22DJSZX5MHJ33F2CRDQ23YD",
            "2026-09-09T06:00:00Z",
            PackSource {
                git_branch: None,
                git_commit: None,
                hostname: None,
            },
            BTreeMap::from([("tasks".to_string(), 2)]),
        );
        manifest.parents = Vec::new();
        write_bundle_raw(root.path(), &manifest, &two_tasks(), &[], &[]);
        assert!(!root.path().join("tombstones.jsonl").exists());

        let bundle = read_bundle(root.path()).unwrap();
        assert_eq!(bundle.source_format_version, PACK_FORMAT_VERSION_V1);
        assert_eq!(bundle.manifest.format_version, PACK_FORMAT_VERSION);
        assert!(bundle.manifest.parents.is_empty());
        assert_eq!(bundle.manifest.counts["tombstones"], 0);
        assert!(bundle.tables["tombstones"].is_empty());
        assert_eq!(bundle.actual_counts()["tasks"], 2);
    }

    #[test]
    fn v1_bundle_with_tombstone_rows_is_refused_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let manifest = PackManifest::new_v1(
            "0.8.1",
            17,
            "01KY6ZK0TMQM5ANGZ97T68C71G",
            "01M22DJSZX5MHJ33F2CRDQ23YD",
            "2026-09-09T06:00:00Z",
            PackSource {
                git_branch: None,
                git_commit: None,
                hostname: None,
            },
            BTreeMap::from([("tasks".to_string(), 2)]),
        );
        write_bundle_raw(root.path(), &manifest, &two_tasks(), &[], &[]);
        // A v1 bundle cannot declare v2-only tables; shipping rows anyway
        // must fail count validation instead of silently dropping them.
        write_table_file(
            root.path(),
            "tombstones",
            &[serde_json::json!({"table_name": "tasks", "row_id": "01T9"})],
        )
        .unwrap();
        let error = read_bundle(root.path()).unwrap_err();
        assert_eq!(error.code, "VALIDATION_FAILED");
    }

    #[test]
    fn v2_bundle_without_tombstones_file_refused() {
        let root = tempfile::tempdir().unwrap();
        write_bundle_raw(root.path(), &sample_manifest(2, 1), &two_tasks(), &[], &[]);
        fs::remove_file(root.path().join("tombstones.jsonl")).unwrap();
        let error = read_bundle(root.path()).unwrap_err();
        assert_eq!(error.code, "VALIDATION_FAILED");
    }

    #[test]
    fn v2_manifest_with_tombstone_counts_mismatch_refused() {
        let root = tempfile::tempdir().unwrap();
        let manifest = sample_manifest(2, 1);
        write_bundle_raw(root.path(), &manifest, &two_tasks(), &[], &[]);
        // Manifest declares 0 tombstones but the bundle ships one row.
        write_table_file(
            root.path(),
            "tombstones",
            &[serde_json::json!({"table_name": "tasks", "row_id": "01T9"})],
        )
        .unwrap();
        let error = read_bundle(root.path()).unwrap_err();
        assert_eq!(error.code, "VALIDATION_FAILED");
    }

    #[test]
    fn v2_round_trip_preserves_parents_and_tombstones_byte_stable() {
        let root = tempfile::tempdir().unwrap();
        let mut manifest = sample_manifest(2, 0);
        manifest.parents = vec!["01PARENT1".to_string(), "01PARENT2".to_string()];
        let tombstones = vec![
            serde_json::json!({
                "project_id": "01KY6ZK0TMQM5ANGZ97T68C71G",
                "table_name": "tasks",
                "row_id": "01GONE1",
                "deleted_at": "2026-09-10T00:00:00Z"
            }),
            serde_json::json!({
                "project_id": "01KY6ZK0TMQM5ANGZ97T68C71G",
                "table_name": "tasks",
                "row_id": "01GONE2",
                "deleted_at": "2026-09-10T01:00:00Z"
            }),
        ];
        manifest.counts.insert("tombstones".to_string(), 2);
        let project = serde_json::json!({"id": "01KY6ZK0TMQM5ANGZ97T68C71G"});
        let tables = BTreeMap::from([
            ("tasks".to_string(), two_tasks()),
            ("tombstones".to_string(), tombstones.clone()),
        ]);

        let first = root.path().join("first");
        write_bundle(&first, &manifest, &project, &tables).unwrap();
        let bundle = read_bundle(&first).unwrap();
        assert_eq!(bundle.manifest.parents, manifest.parents.clone());
        assert_eq!(bundle.tables["tombstones"], tombstones);
        assert!(!bundle.manifest.redacted);

        let second = root.path().join("second");
        write_bundle(&second, &bundle.manifest, &bundle.project, &bundle.tables).unwrap();
        assert_eq!(
            fs::read(first.join("tombstones.jsonl")).unwrap(),
            fs::read(second.join("tombstones.jsonl")).unwrap()
        );
        assert_eq!(
            fs::read(first.join(PACK_MANIFEST_FILE)).unwrap(),
            fs::read(second.join(PACK_MANIFEST_FILE)).unwrap()
        );
    }

    #[test]
    fn v1_manifest_write_keeps_v1_layout_and_round_trips() {
        let root = tempfile::tempdir().unwrap();
        let manifest = PackManifest::new_v1(
            "0.9.1",
            17,
            "01KY6ZK0TMQM5ANGZ97T68C71G",
            "01M22DJSZX5MHJ33F2CRDQ23YD",
            "2026-09-09T06:00:00Z",
            PackSource {
                git_branch: None,
                git_commit: None,
                hostname: None,
            },
            BTreeMap::from([("tasks".to_string(), 1)]),
        );
        let project = serde_json::json!({"id": "01KY6ZK0TMQM5ANGZ97T68C71G"});
        let tables =
            BTreeMap::from([("tasks".to_string(), vec![serde_json::json!({"id": "01T1"})])]);
        let dest = root.path().join("v1");
        write_bundle(&dest, &manifest, &project, &tables).unwrap();
        assert!(!dest.join("tombstones.jsonl").exists());
        for table in PACK_TABLE_FILES_V1 {
            assert!(dest.join(format!("{table}.jsonl")).exists(), "{table}");
        }
        let bundle = read_bundle(&dest).unwrap();
        assert_eq!(bundle.source_format_version, PACK_FORMAT_VERSION_V1);
        assert_eq!(bundle.manifest.format_version, PACK_FORMAT_VERSION);
    }

    #[test]
    fn tampered_counts_rejected() {
        let root = tempfile::tempdir().unwrap();
        // Manifest claims 104 tasks; the bundle holds 2.
        write_bundle_raw(
            root.path(),
            &sample_manifest(104, 1),
            &two_tasks(),
            &[serde_json::json!({"id": "01E1"})],
            &[],
        );
        let error = read_bundle(root.path()).unwrap_err();
        assert_eq!(error.code, "VALIDATION_FAILED");
    }

    #[test]
    fn future_format_version_refused() {
        let root = tempfile::tempdir().unwrap();
        let mut manifest = sample_manifest(0, 0);
        manifest.format_version = 999;
        write_bundle_raw(root.path(), &manifest, &[], &[], &[]);
        let error = read_bundle(root.path()).unwrap_err();
        assert_eq!(error.code, "UNSUPPORTED_OPERATION");
    }

    #[test]
    fn tampered_parents_rejected_on_read() {
        let root = tempfile::tempdir().unwrap();
        let mut manifest = sample_manifest(0, 0);
        let export_id = manifest.export_id.clone();
        manifest.parents = vec![export_id];
        write_bundle_raw(root.path(), &manifest, &[], &[], &[]);
        let error = read_bundle(root.path()).unwrap_err();
        assert_eq!(error.code, "VALIDATION_FAILED");
    }

    #[test]
    fn missing_table_file_refused() {
        let root = tempfile::tempdir().unwrap();
        write_bundle_raw(root.path(), &sample_manifest(0, 0), &[], &[], &[]);
        fs::remove_file(root.path().join("tasks.jsonl")).unwrap();
        let error = read_bundle(root.path()).unwrap_err();
        assert_eq!(error.code, "VALIDATION_FAILED");
    }

    #[test]
    fn missing_manifest_refused() {
        let root = tempfile::tempdir().unwrap();
        write_bundle_raw(root.path(), &sample_manifest(0, 0), &[], &[], &[]);
        fs::remove_file(root.path().join(PACK_MANIFEST_FILE)).unwrap();
        let error = read_bundle(root.path()).unwrap_err();
        assert_eq!(error.code, "VALIDATION_FAILED");
    }

    #[test]
    fn malformed_jsonl_line_refused() {
        let root = tempfile::tempdir().unwrap();
        write_bundle_raw(root.path(), &sample_manifest(1, 0), &two_tasks(), &[], &[]);
        fs::write(root.path().join("tasks.jsonl"), "{not json}\n").unwrap();
        let error = read_bundle(root.path()).unwrap_err();
        assert_eq!(error.code, "VALIDATION_FAILED");
    }

    #[test]
    fn table_file_write_and_read_round_trip() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path()).unwrap();
        let rows = vec![
            serde_json::json!({"id": "a"}),
            serde_json::json!({"id": "b"}),
        ];
        write_table_file(root.path(), "tasks", &rows).unwrap();
        let back = read_table_file(root.path(), "tasks").unwrap();
        assert_eq!(back, rows);
    }

    #[test]
    fn write_bundle_validates_and_round_trips() {
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("bundle");
        let manifest = sample_manifest(1, 0);
        let project = serde_json::json!({"id": "01KY6ZK0TMQM5ANGZ97T68C71G", "name": "demo"});
        let tables =
            BTreeMap::from([("tasks".to_string(), vec![serde_json::json!({"id": "01T1"})])]);
        write_bundle(&dest, &manifest, &project, &tables).unwrap();
        let bundle = read_bundle(&dest).unwrap();
        assert_eq!(bundle.manifest, manifest);
        assert_eq!(bundle.tables["tasks"].len(), 1);
    }

    #[test]
    fn checksum_helpers_cover_tombstone_bytes() {
        let root = tempfile::tempdir().unwrap();
        let rows = vec![serde_json::json!({"table_name": "tasks", "row_id": "01T9"})];
        write_table_file(root.path(), "tombstones", &rows).unwrap();
        let bytes = fs::read(root.path().join("tombstones.jsonl")).unwrap();
        let mut reader = crate::checksum::checksum_reader(std::io::Cursor::new(bytes.clone()));
        let mut out = Vec::new();
        std::io::Read::read_to_end(&mut reader, &mut out).unwrap();
        let (_, digest) = reader.finalize();
        assert_eq!(digest, crate::checksum::sha256_hex(&bytes));
    }
}
