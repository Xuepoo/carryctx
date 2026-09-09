// P4 thin bridge: ctxpack reader/validator owned by `carryctx-pack`.
// This file stays as a re-export shim so existing
// `crate::application::interchange::*` imports keep compiling with zero
// CLI contract change. Core-only constraint: pack depends only on
// `carryctx-core`, never on SQLite/Git/CLI.
pub use carryctx_pack::io::{
    PackBundle, read_bundle, read_table_file, write_bundle, write_table_file,
};
pub use carryctx_pack::manifest::{
    PACK_FORMAT, PACK_FORMAT_VERSION, PACK_MANIFEST_FILE, PACK_PROJECT_FILE, PACK_TABLE_FILES,
    PackManifest, PackSource, check_counts, prune_worktrees, reanchor_project,
    validate_manifest_value,
};

// Re-expose via crate::domain::pack path for call sites that import pack
// constants through the domain module (zero CLI contract change).
#[allow(unused_imports)]
pub use carryctx_pack::manifest as pack;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::pack::{PackManifest, PackSource};
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;

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
