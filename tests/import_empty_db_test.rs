//! CTX-0162 integration tests: `carryctx import` must initialize an
//! empty-but-migrated database (schema present, zero project rows) instead of
//! failing the exactly-one-project validation on the replace path.
//!
//! Fresh-clone state: any command that opens the runtime creates and migrates
//! `<git-common-dir>/carryctx/state.sqlite` before `init` runs, leaving a
//! database with zero project rows. The documented restore
//! (`carryctx import --from-git origin/carryctx-snapshots --mode replace --yes`)
//! used to take the initialized replace path and fail with
//! `DATABASE_ERROR: Database must contain exactly one project row` (exit 5).
//!
//! Non-empty databases must stay fail-closed behind `--mode replace --yes`;
//! this file covers the empty-database path, the `--from-git` path, and the
//! merge-on-empty refusal hint.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Default local-only unredacted snapshot ref used by `export --snapshot`.
const LOCAL_SNAP_REF: &str = "refs/carryctx/local";
/// Remote-tracking ref the fixture fetches the snapshot into.
const FETCHED_SNAP_REF: &str = "refs/remotes/origin/carryctx-snapshots";

fn json(output: &Output) -> serde_json::Value {
    let stream = if output.stdout.is_empty() {
        &output.stderr
    } else {
        &output.stdout
    };
    serde_json::from_slice(stream).unwrap_or_else(|error| {
        panic!(
            "expected JSON output, got {error}: {}",
            String::from_utf8_lossy(stream)
        )
    })
}

fn db_path(repo: &Path) -> PathBuf {
    repo.join(".git/carryctx/state.sqlite")
}

fn project_rows(repo: &Path) -> i64 {
    let conn = rusqlite::Connection::open(db_path(repo)).unwrap();
    conn.query_row("SELECT COUNT(*) FROM projects", [], |row| row.get(0))
        .unwrap()
}

fn task_count(repo: &Path, bin: &Path) -> usize {
    let output = common::run_cmd(repo, bin, &["task", "list", "--json"]);
    assert!(output.status.success(), "task list failed: {output:?}");
    json(&output)["data"].as_array().map(Vec::len).unwrap_or(0)
}

fn imported_event_count(repo: &Path) -> i64 {
    let conn = rusqlite::Connection::open(db_path(repo)).unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM events WHERE type = 'project.imported'",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

/// Reproduce the fresh-clone state: open the runtime (which creates and
/// migrates the state DB) without initializing a project. `task list` on an
/// uninitialized repo succeeds with an empty list and leaves a schema-only
/// database behind, exactly what upstream read/stat commands do.
fn seed_empty_migrated_db(repo: &Path, bin: &Path) {
    let out = common::run_cmd(repo, bin, &["task", "list", "--json"]);
    assert!(
        out.status.success(),
        "task list should succeed on empty state: {out:?}"
    );
    assert!(db_path(repo).exists(), "task list must create the state db");
    assert_eq!(
        project_rows(repo),
        0,
        "seeded database must carry zero project rows"
    );
}

fn seed_source(name: &str) -> (PathBuf, PathBuf) {
    let (src, bin) = common::setup_test_project(name);
    common::init_and_agent(&src, &bin);
    let created = common::run_cmd(
        &src,
        &bin,
        &["task", "create", "--title", "restore me", "--json"],
    );
    assert!(created.status.success(), "task create failed: {created:?}");
    (src, bin)
}

fn export_bundle(src: &Path, bin: &Path, out: &Path) {
    let output = common::run_cmd(src, bin, &["export", "-o", out.to_str().unwrap(), "--json"]);
    assert!(output.status.success(), "export failed: {output:?}");
}

fn export_snapshot(src: &Path, bin: &Path, out: &Path) {
    let output = common::run_cmd(
        src,
        bin,
        &[
            "export",
            "-o",
            out.to_str().unwrap(),
            "--snapshot",
            "--snapshot-ref",
            LOCAL_SNAP_REF,
            "--json",
        ],
    );
    assert!(
        output.status.success(),
        "snapshot export failed: {output:?}"
    );
}

/// Fetch one repo's snapshot ref into another as a remote-tracking ref using
/// local Git transport only (no network).
fn fetch_snapshot(src: &Path, dest: &Path) {
    const GIT_STATE_VARS: &[&str] = &[
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_COMMON_DIR",
        "GIT_NAMESPACE",
        "GIT_CEILING_DIRECTORIES",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_SYSTEM",
    ];
    let mut command = Command::new("git");
    command
        .args([
            "fetch",
            "--quiet",
            src.to_str().unwrap(),
            &format!("{LOCAL_SNAP_REF}:{FETCHED_SNAP_REF}"),
        ])
        .current_dir(dest);
    for var in GIT_STATE_VARS {
        command.env_remove(var);
    }
    let output = command.output().expect("git should spawn");
    assert!(
        output.status.success(),
        "fetch snapshot failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn import_initializes_empty_migrated_database() {
    let (src, bin) = seed_source("import_empty_src");
    let bundle = src.join("bundle");
    export_bundle(&src, &bin, &bundle);
    let src_tasks = task_count(&src, &bin);

    let (target, _) = common::setup_test_project("import_empty_dir_target");
    seed_empty_migrated_db(&target, &bin);

    let imported = common::run_cmd(
        &target,
        &bin,
        &["import", bundle.to_str().unwrap(), "--json"],
    );
    assert!(
        imported.status.success(),
        "import into empty migrated db failed: {imported:?}"
    );
    let body = json(&imported);
    assert_eq!(body["success"], true);
    assert_eq!(body["data"]["mode"], "init");
    assert_eq!(body["data"]["operation"]["applied"], true);
    assert_eq!(project_rows(&target), 1, "project must be initialized");
    assert_eq!(imported_event_count(&target), 1);
    assert_eq!(task_count(&target, &bin), src_tasks);

    let doctor = common::run_cmd(&target, &bin, &["doctor", "--json"]);
    assert!(doctor.status.success(), "doctor failed: {doctor:?}");
}

#[test]
fn import_from_git_mode_replace_initializes_empty_migrated_database() {
    let (src, bin) = seed_source("import_empty_git_src");
    let snapshot_out = src.join("snapshot-bundle");
    export_snapshot(&src, &bin, &snapshot_out);
    let src_tasks = task_count(&src, &bin);

    let (target, _) = common::setup_test_project("import_empty_git_target");
    seed_empty_migrated_db(&target, &bin);
    fetch_snapshot(&src, &target);

    // The documented restore command: fresh clone, empty-but-migrated db.
    let imported = common::run_cmd(
        &target,
        &bin,
        &[
            "import",
            "--from-git",
            FETCHED_SNAP_REF,
            "--mode",
            "replace",
            "--yes",
            "--json",
        ],
    );
    assert!(
        imported.status.success(),
        "--from-git restore into empty migrated db failed: {imported:?}"
    );
    let body = json(&imported);
    assert_eq!(body["success"], true);
    assert_eq!(body["data"]["mode"], "init");
    assert_eq!(project_rows(&target), 1);
    assert_eq!(imported_event_count(&target), 1);
    assert_eq!(task_count(&target, &bin), src_tasks);

    let doctor = common::run_cmd(&target, &bin, &["doctor", "--json"]);
    assert!(doctor.status.success(), "doctor failed: {doctor:?}");
}

#[test]
fn import_into_empty_migrated_db_stays_fail_closed_once_populated() {
    let (src, bin) = seed_source("import_empty_then_refuse_src");
    let bundle = src.join("bundle");
    export_bundle(&src, &bin, &bundle);

    let (target, _) = common::setup_test_project("import_empty_then_refuse_target");
    seed_empty_migrated_db(&target, &bin);

    let first = common::run_cmd(
        &target,
        &bin,
        &["import", bundle.to_str().unwrap(), "--json"],
    );
    assert!(first.status.success(), "first import failed: {first:?}");

    // Now populated: a bare import must refuse, never silently overwrite.
    let refused = common::run_cmd(
        &target,
        &bin,
        &["import", bundle.to_str().unwrap(), "--json"],
    );
    assert!(!refused.status.success());
    assert_eq!(refused.status.code(), Some(3));
    assert_eq!(json(&refused)["error"]["code"], "STATE_CONFLICT");
    assert_eq!(project_rows(&target), 1);
}

#[test]
fn import_mode_merge_on_empty_db_hints_exact_from_git_command() {
    let (src, bin) = seed_source("import_empty_merge_hint_src");
    let snapshot_out = src.join("snapshot-bundle");
    export_snapshot(&src, &bin, &snapshot_out);

    let (target, _) = common::setup_test_project("import_empty_merge_hint_target");
    seed_empty_migrated_db(&target, &bin);
    fetch_snapshot(&src, &target);

    let refused = common::run_cmd(
        &target,
        &bin,
        &[
            "import",
            "--from-git",
            FETCHED_SNAP_REF,
            "--mode",
            "merge",
            "--json",
        ],
    );
    assert!(!refused.status.success());
    assert_eq!(refused.status.code(), Some(3));
    let body = json(&refused);
    assert_eq!(body["error"]["code"], "STATE_CONFLICT");
    let message = body["error"]["message"].as_str().unwrap_or("");
    assert!(
        message.contains("bare import"),
        "refusal must keep the bare-import hint: {message}"
    );
    let suggestions = body["error"]["suggestions"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    assert!(
        suggestions.contains(&format!("import --from-git {FETCHED_SNAP_REF}")),
        "refusal must name the exact restore command: {suggestions}"
    );
    assert_eq!(
        project_rows(&target),
        0,
        "merge refusal must not create project state"
    );
}

#[test]
fn dry_run_on_empty_migrated_database_reports_no_replace() {
    let (src, bin) = seed_source("import_empty_dryrun_src");
    let bundle = src.join("bundle");
    export_bundle(&src, &bin, &bundle);

    let (target, _) = common::setup_test_project("import_empty_dryrun_target");
    seed_empty_migrated_db(&target, &bin);
    let before = std::fs::read(db_path(&target)).unwrap();

    let preview = common::run_cmd(
        &target,
        &bin,
        &["import", bundle.to_str().unwrap(), "--dry-run", "--json"],
    );
    assert!(preview.status.success(), "dry-run failed: {preview:?}");
    let body = json(&preview);
    assert_eq!(body["data"]["operation"]["applied"], false);
    assert_eq!(
        body["data"]["would_replace"], false,
        "an empty migrated database has nothing to replace"
    );
    assert_eq!(std::fs::read(db_path(&target)).unwrap(), before);
}
