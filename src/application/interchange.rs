//! ctxpack directory-layout reader/validator (format `carryctx-pack-dir` v1).
//!
//! Filesystem half of CTX-0111: reads `<export-dir>/` per Section 2
//! (`manifest.json`, `project.json`, one `*.jsonl` per table), counts rows,
//! and runs the pure [`crate::domain::pack`] validation fail-closed.
//! No database writes happen here; this module only reads the bundle.

use crate::domain::pack;
use crate::error::CarryCtxError;
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
    pub manifest: pack::PackManifest,
    pub project: serde_json::Value,
    pub tables: BTreeMap<String, Vec<serde_json::Value>>,
}

impl PackBundle {
    /// Row count per table stem, covering every [`pack::PACK_TABLE_FILES`]
    /// entry (zero for empty tables).
    pub fn actual_counts(&self) -> BTreeMap<String, u64> {
        let mut counts = BTreeMap::new();
        for table in pack::PACK_TABLE_FILES {
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

    let manifest_text = fs::read_to_string(dir.join(pack::PACK_MANIFEST_FILE)).map_err(|_| {
        CarryCtxError::validation_error(format!(
            "Pack manifest '{}/{}' is missing.",
            dir.display(),
            pack::PACK_MANIFEST_FILE
        ))
    })?;
    let manifest_value: serde_json::Value = serde_json::from_str(&manifest_text)
        .map_err(|e| CarryCtxError::validation_error(format!("Pack manifest is invalid: {e}")))?;
    // Version gating inside validate_manifest_value runs before shape
    // checks so newer writers surface UNSUPPORTED_OPERATION.
    let manifest = pack::validate_manifest_value(&manifest_value)?;

    let project_text = fs::read_to_string(dir.join(pack::PACK_PROJECT_FILE)).map_err(|_| {
        CarryCtxError::validation_error(format!(
            "Pack file '{}/{}' is missing.",
            dir.display(),
            pack::PACK_PROJECT_FILE
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
    for table in pack::PACK_TABLE_FILES {
        let rows = read_table_file(dir, table)?;
        tables.insert((*table).to_string(), rows);
    }

    let bundle = PackBundle {
        dir: dir.to_path_buf(),
        manifest,
        project,
        tables,
    };
    pack::check_counts(&bundle.manifest, &bundle.actual_counts())?;
    Ok(bundle)
}

/// Read one `<table>.jsonl` file. The file must exist (a missing table
/// file refuses the bundle); empty/whitespace-only lines are skipped so a
/// trailing newline is not a row, while any other malformed line refuses
/// the bundle fail-closed.
fn read_table_file(dir: &Path, table: &str) -> Result<Vec<serde_json::Value>, CarryCtxError> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::pack::{PackManifest, PackSource};

    fn sample_manifest(tasks: u64, events: u64) -> PackManifest {
        PackManifest::new(
            "0.8.1",
            17,
            "01KY6ZK0TMQM5ANGZ97T68C71G",
            "01M22DJSZX5MHJ33F2CRDQ23YD",
            "2026-09-09T06:00:00Z",
            PackSource {
                git_branch: Some("main".to_string()),
                git_commit: Some("21951eb".to_string()),
                hostname: Some("dev-a".to_string()),
            },
            BTreeMap::from([("tasks".to_string(), tasks), ("events".to_string(), events)]),
        )
    }

    fn write_bundle(
        root: &Path,
        manifest: &PackManifest,
        task_rows: &[serde_json::Value],
        event_rows: &[serde_json::Value],
    ) {
        fs::write(
            root.join(pack::PACK_MANIFEST_FILE),
            serde_json::to_string_pretty(manifest).unwrap(),
        )
        .unwrap();
        fs::write(
            root.join(pack::PACK_PROJECT_FILE),
            r#"{"id":"01KY6ZK0TMQM5ANGZ97T68C71G","name":"demo"}"#,
        )
        .unwrap();
        for table in pack::PACK_TABLE_FILES {
            let rows: &[serde_json::Value] = match *table {
                "tasks" => task_rows,
                "events" => event_rows,
                _ => &[],
            };
            let mut text = String::new();
            for row in rows {
                text.push_str(&serde_json::to_string(row).unwrap());
                text.push('\n');
            }
            fs::write(root.join(format!("{table}.jsonl")), text).unwrap();
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
        write_bundle(root.path(), &sample_manifest(2, 1), &two_tasks(), &events);
        let bundle = read_bundle(root.path()).unwrap();
        assert_eq!(bundle.manifest.project_id, "01KY6ZK0TMQM5ANGZ97T68C71G");
        assert_eq!(bundle.tables["tasks"].len(), 2);
        assert_eq!(bundle.tables["events"].len(), 1);
        assert_eq!(bundle.actual_counts()["tasks"], 2);
    }

    #[test]
    fn tampered_counts_rejected() {
        let root = tempfile::tempdir().unwrap();
        // Manifest claims 104 tasks; the bundle holds 2.
        write_bundle(
            root.path(),
            &sample_manifest(104, 1),
            &two_tasks(),
            &[serde_json::json!({"id": "01E1"})],
        );
        let error = read_bundle(root.path()).unwrap_err();
        assert_eq!(error.code, "VALIDATION_FAILED");
    }

    #[test]
    fn future_format_version_refused() {
        let root = tempfile::tempdir().unwrap();
        let mut manifest = sample_manifest(0, 0);
        manifest.format_version = 999;
        write_bundle(root.path(), &manifest, &[], &[]);
        let error = read_bundle(root.path()).unwrap_err();
        assert_eq!(error.code, "UNSUPPORTED_OPERATION");
    }

    #[test]
    fn missing_table_file_refused() {
        let root = tempfile::tempdir().unwrap();
        write_bundle(root.path(), &sample_manifest(0, 0), &[], &[]);
        fs::remove_file(root.path().join("tasks.jsonl")).unwrap();
        let error = read_bundle(root.path()).unwrap_err();
        assert_eq!(error.code, "VALIDATION_FAILED");
    }

    #[test]
    fn missing_manifest_refused() {
        let root = tempfile::tempdir().unwrap();
        write_bundle(root.path(), &sample_manifest(0, 0), &[], &[]);
        fs::remove_file(root.path().join(pack::PACK_MANIFEST_FILE)).unwrap();
        let error = read_bundle(root.path()).unwrap_err();
        assert_eq!(error.code, "VALIDATION_FAILED");
    }

    #[test]
    fn malformed_jsonl_line_refused() {
        let root = tempfile::tempdir().unwrap();
        write_bundle(root.path(), &sample_manifest(1, 0), &two_tasks(), &[]);
        fs::write(root.path().join("tasks.jsonl"), "{not json}\n").unwrap();
        let error = read_bundle(root.path()).unwrap_err();
        assert_eq!(error.code, "VALIDATION_FAILED");
    }
}
