mod common;

use std::path::Path;

use carryctx_cli::application::interchange::{
    PACK_FORMAT_VERSION, PACK_FORMAT_VERSION_V1, pack_table_files, read_bundle,
};

fn json(output: &std::process::Output) -> serde_json::Value {
    let bytes = if output.stdout.is_empty() {
        &output.stderr
    } else {
        &output.stdout
    };
    serde_json::from_slice(bytes).unwrap_or_else(|error| {
        panic!(
            "expected JSON output ({error}): {}",
            String::from_utf8_lossy(bytes)
        )
    })
}

/// The writer emits v2 once schema 0018 (tombstones) exists and v1 before
/// that; this keeps the assertion valid across the CTX-0140 landing.
fn expected_writer_format(dir: &Path) -> u32 {
    let db = dir.join(".git/carryctx/state.sqlite");
    let conn = rusqlite::Connection::open(db).unwrap();
    let has_tombstones: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'tombstones'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    if has_tombstones > 0 {
        PACK_FORMAT_VERSION
    } else {
        PACK_FORMAT_VERSION_V1
    }
}

fn init(dir: &Path, bin: &Path) {
    common::init_and_agent(dir, bin);
    // Seed some state so the bundle is non-trivial.
    let task = common::run_cmd(
        dir,
        bin,
        &["task", "create", "--title", "export me", "--json"],
    );
    assert!(task.status.success(), "task create failed: {:?}", task);
}

#[test]
fn export_dir_writes_layout_and_counts_match_on_reread() {
    let (dir, bin) = common::setup_test_project("export_dir_layout");
    init(&dir, &bin);
    let out = dir.join("ctxpack-dir");

    let exported = common::run_cmd(
        &dir,
        &bin,
        &[
            "export",
            "--pack-format",
            "dir",
            "-o",
            out.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(exported.status.success(), "export failed: {:?}", exported);
    let body = json(&exported);
    assert_eq!(body["command"], "export.create");
    assert_eq!(body["success"], true);

    // Section-2 layout: manifest + project + one file per table of the
    // writer's format version (v2 adds tombstones.jsonl).
    let expected_format = expected_writer_format(&dir);
    assert!(out.join("manifest.json").is_file());
    assert!(out.join("project.json").is_file());
    for table in pack_table_files(expected_format) {
        assert!(
            out.join(format!("{table}.jsonl")).is_file(),
            "missing {table}.jsonl"
        );
    }
    assert_eq!(
        out.join("tombstones.jsonl").exists(),
        expected_format == PACK_FORMAT_VERSION
    );

    // Re-read through the T1 validator: manifest validates and every
    // declared count matches the rows on disk.
    let bundle = read_bundle(&out).expect("exported bundle must validate");
    assert_eq!(bundle.manifest.format, "carryctx-pack-dir");
    assert_eq!(bundle.source_format_version, expected_format);
    // The read path always exposes the current format view.
    assert_eq!(bundle.manifest.format_version, PACK_FORMAT_VERSION);
    let body_counts = body["data"]["counts"].as_object().unwrap();
    let actual = bundle.actual_counts();
    for (table, declared) in body_counts {
        assert_eq!(
            actual.get(table),
            Some(&declared.as_u64().unwrap()),
            "count mismatch for {table}"
        );
    }
    assert_eq!(
        actual["tombstones"],
        body_counts
            .get("tombstones")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    );
    assert!(!bundle.tables["tasks"].is_empty());
    assert!(!bundle.tables["events"].is_empty());
    assert_eq!(
        body["data"]["path"].as_str().unwrap(),
        out.to_string_lossy()
    );
}

#[test]
fn export_appends_exported_event_included_in_next_bundle() {
    let (dir, bin) = common::setup_test_project("export_event_audit");
    init(&dir, &bin);

    let first = dir.join("pack-one");
    let out = common::run_cmd(
        &dir,
        &bin,
        &[
            "export",
            "--pack-format",
            "dir",
            "-o",
            first.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(out.status.success(), "first export failed: {:?}", out);

    // The second bundle must contain the first export's audit event, and
    // its events count must be exactly one more than the first bundle's.
    let second = dir.join("pack-two");
    let out = common::run_cmd(
        &dir,
        &bin,
        &[
            "export",
            "--pack-format",
            "dir",
            "-o",
            second.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(out.status.success(), "second export failed: {:?}", out);

    let one = read_bundle(&first).unwrap();
    let two = read_bundle(&second).unwrap();
    let exported_events = two.tables["events"]
        .iter()
        .filter(|row| row.get("type").and_then(|v| v.as_str()) == Some("project.exported"))
        .count();
    assert!(
        exported_events >= 1,
        "second bundle must carry the project.exported audit event"
    );
    assert_eq!(
        two.actual_counts()["events"],
        one.actual_counts()["events"] + 1
    );
}

#[test]
fn export_dry_run_writes_nothing_and_reports_plan() {
    let (dir, bin) = common::setup_test_project("export_dry_run");
    init(&dir, &bin);
    let out = dir.join("ctxpack-dry");
    let db = dir.join(".git/carryctx/state.sqlite");
    let before = std::fs::metadata(&db).unwrap().modified().unwrap();
    let events_before = json(&common::run_cmd(&dir, &bin, &["event", "list", "--json"]))["data"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0);

    let planned = common::run_cmd(
        &dir,
        &bin,
        &[
            "export",
            "--pack-format",
            "dir",
            "-o",
            out.to_str().unwrap(),
            "--dry-run",
            "--json",
        ],
    );
    assert!(planned.status.success(), "dry-run failed: {:?}", planned);
    let body = json(&planned);
    assert_eq!(body["command"], "export.create");
    assert_eq!(body["data"]["operation"]["applied"], false);
    assert!(body["data"]["counts"].is_object());
    assert_eq!(
        body["data"]["path"].as_str().unwrap(),
        out.to_string_lossy()
    );

    // Cleanliness: no directory, no database change, no audit event.
    assert!(!out.exists(), "dry-run must not create the target dir");
    assert_eq!(
        std::fs::metadata(&db).unwrap().modified().unwrap(),
        before,
        "dry-run must not touch SQLite"
    );
    let events_after = json(&common::run_cmd(&dir, &bin, &["event", "list", "--json"]))["data"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0);
    assert_eq!(events_after, events_before);
}

#[test]
fn export_stdout_reports_unsupported_without_new_dependencies() {
    let (dir, bin) = common::setup_test_project("export_stdout");
    init(&dir, &bin);
    let out = common::run_cmd(&dir, &bin, &["export", "--stdout", "--json"]);
    assert!(!out.status.success());
    let body = json(&out);
    assert_eq!(body["error"]["code"], "UNSUPPORTED_OPERATION");
    assert_eq!(out.status.code(), Some(10));
}

#[test]
fn export_rejects_unknown_pack_format_and_missing_target() {
    let (dir, bin) = common::setup_test_project("export_bad_args");
    init(&dir, &bin);

    let bad_format = common::run_cmd(
        &dir,
        &bin,
        &[
            "export",
            "--pack-format",
            "tar",
            "-o",
            dir.join("x").to_str().unwrap(),
            "--json",
        ],
    );
    assert!(!bad_format.status.success());
    assert_eq!(json(&bad_format)["error"]["code"], "UNSUPPORTED_OPERATION");
    assert_eq!(bad_format.status.code(), Some(10));

    let missing_target = common::run_cmd(&dir, &bin, &["export", "--json"]);
    assert!(!missing_target.status.success());
    assert_eq!(json(&missing_target)["error"]["code"], "INVALID_ARGUMENTS");
    assert_eq!(missing_target.status.code(), Some(2));
}

#[test]
fn export_outside_git_repo_fails_with_git_error() {
    let root =
        std::env::temp_dir().join(format!("carryctx_test_export_nogit_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let bin = common::test_binary();
    let out = common::run_cmd(
        &root,
        &bin,
        &["export", "-o", root.join("x").to_str().unwrap(), "--json"],
    );
    assert!(!out.status.success());
    assert_eq!(json(&out)["error"]["code"], "GIT_ERROR");
    assert_eq!(out.status.code(), Some(4));
    let _ = std::fs::remove_dir_all(&root);
}
