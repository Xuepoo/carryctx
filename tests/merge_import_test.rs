//! CTX-0142 integration tests: `import --mode merge` staging, base
//! resolution, conflict refusal, and atomic apply (design
//! `2026-09-10-mergeable-git-managed-state.md` §2.1–§2.4).

mod common;

use std::path::{Path, PathBuf};

use carryctx_cli::adapter::filesystem::{JournalEntry, write_journal};
use carryctx_cli::adapter::xdg::XdgPaths;
use carryctx_cli::application::project_mgmt;

fn json(output: &std::process::Output) -> serde_json::Value {
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

fn init(dir: &Path, bin: &Path) {
    let output = common::run_cmd(dir, bin, &["init", "--force"]);
    assert!(output.status.success(), "init failed: {output:?}");
}

fn db_path(repo: &Path) -> PathBuf {
    repo.join(".git/carryctx/state.sqlite")
}

fn state_dir(repo: &Path) -> PathBuf {
    repo.join(".git/carryctx")
}

fn run(repo: &Path, bin: &Path, args: &[&str]) -> std::process::Output {
    common::run_cmd(repo, bin, args)
}

fn project_id(repo: &Path) -> String {
    let conn = rusqlite::Connection::open(db_path(repo)).unwrap();
    conn.query_row("SELECT id FROM projects LIMIT 1", [], |row| row.get(0))
        .unwrap()
}

fn agent_id(repo: &Path, name: &str) -> String {
    let conn = rusqlite::Connection::open(db_path(repo)).unwrap();
    conn.query_row("SELECT id FROM agents WHERE name = ?1", [name], |row| {
        row.get(0)
    })
    .unwrap()
}

fn task_id_by_title(repo: &Path, title: &str) -> String {
    let conn = rusqlite::Connection::open(db_path(repo)).unwrap();
    conn.query_row("SELECT id FROM tasks WHERE title = ?1", [title], |row| {
        row.get(0)
    })
    .unwrap()
}

fn task_title(repo: &Path, id: &str) -> Option<String> {
    let conn = rusqlite::Connection::open(db_path(repo)).unwrap();
    conn.query_row("SELECT title FROM tasks WHERE id = ?1", [id], |row| {
        row.get(0)
    })
    .ok()
}

fn event_payloads(repo: &Path, event_type: &str) -> Vec<serde_json::Value> {
    let conn = rusqlite::Connection::open(db_path(repo)).unwrap();
    let mut stmt = conn
        .prepare("SELECT payload_json FROM events WHERE type = ?1 ORDER BY occurred_at")
        .unwrap();
    let rows = stmt
        .query_map([event_type], |row| row.get::<_, String>(0))
        .unwrap();
    rows.map(|row| serde_json::from_str(&row.unwrap()).unwrap())
        .collect()
}

fn db_bytes(repo: &Path) -> Vec<u8> {
    std::fs::read(db_path(repo)).unwrap()
}

fn export_bundle(repo: &Path, bin: &Path, out: &Path) {
    let output = run(
        repo,
        bin,
        &[
            "export",
            "--pack-format",
            "dir",
            "-o",
            out.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(output.status.success(), "export failed: {output:?}");
}

fn read_manifest(bundle: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(bundle.join("manifest.json")).unwrap()).unwrap()
}

fn write_manifest(bundle: &Path, manifest: &serde_json::Value) {
    std::fs::write(
        bundle.join("manifest.json"),
        serde_json::to_string_pretty(manifest).unwrap(),
    )
    .unwrap();
}

/// Override the manifest export id / parent edges (the production export
/// mints its own id; base-resolution tests need a controlled DAG).
fn set_manifest_edges(bundle: &Path, export_id: &str, parents: &[&str]) {
    let mut manifest = read_manifest(bundle);
    manifest["export_id"] = serde_json::json!(export_id);
    manifest["parents"] = serde_json::json!(parents);
    write_manifest(bundle, &manifest);
}

fn set_manifest_redacted(bundle: &Path) {
    let mut manifest = read_manifest(bundle);
    manifest["redacted"] = serde_json::json!(true);
    write_manifest(bundle, &manifest);
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// Materialize a bundle as the local snapshot-cache entry `<export_id>`.
fn seed_snapshot_cache(repo: &Path, export_id: &str, bundle: &Path) {
    copy_dir(bundle, &state_dir(repo).join("snapshots").join(export_id));
}

fn set_last_export_id(repo: &Path, export_id: &str) {
    let conn = rusqlite::Connection::open(db_path(repo)).unwrap();
    conn.execute(
        "INSERT INTO snapshot_state (project_id, key, value, updated_at)
         SELECT id, 'last_export_id', ?1, ?2 FROM projects WHERE 1=1
         ON CONFLICT(project_id, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        rusqlite::params![export_id, chrono::Utc::now().to_rfc3339()],
    )
    .unwrap();
}

fn merge_session_dirs(repo: &Path) -> Vec<String> {
    let dir = state_dir(repo).join("merges");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().unwrap().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

struct Pair {
    src: PathBuf,
    tgt: PathBuf,
    bin: PathBuf,
    base_bundle: PathBuf,
    base_export_id: String,
    base_task: String,
}

/// Two repositories sharing one project identity: `src` initialized fresh,
/// `tgt` fresh-imported from `src`'s first export. The shared state is one
/// task titled "base task".
fn seed_pair(name: &str) -> Pair {
    let (src, bin) = common::setup_test_project(&format!("{name}_src"));
    init(&src, &bin);
    let agent = run(
        &src,
        &bin,
        &[
            "agent",
            "register",
            "--name",
            "tester",
            "--provider",
            "test",
        ],
    );
    assert!(agent.status.success(), "agent register failed: {agent:?}");
    let task = run(&src, &bin, &["task", "create", "--title", "base task"]);
    assert!(task.status.success(), "task create failed: {task:?}");

    let base_bundle = src.join("bundle-base");
    export_bundle(&src, &bin, &base_bundle);
    let base_export_id = read_manifest(&base_bundle)["export_id"]
        .as_str()
        .unwrap()
        .to_string();
    let base_task = task_id_by_title(&src, "base task");

    let (tgt, _) = common::setup_test_project(&format!("{name}_tgt"));
    let imported = run(
        &tgt,
        &bin,
        &["import", base_bundle.to_str().unwrap(), "--json"],
    );
    assert!(
        imported.status.success(),
        "target import failed: {imported:?}"
    );
    assert_eq!(project_id(&src), project_id(&tgt));

    Pair {
        src,
        tgt,
        bin,
        base_bundle,
        base_export_id,
        base_task,
    }
}

fn create_task(repo: &Path, bin: &Path, title: &str) -> String {
    let output = run(repo, bin, &["task", "create", "--title", title]);
    assert!(output.status.success(), "task create failed: {output:?}");
    task_id_by_title(repo, title)
}

// ── Base resolution ──────────────────────────────────────────────────────

#[test]
fn merge_on_fresh_target_uses_fresh_import_path() {
    let (src, bin) = common::setup_test_project("merge_fresh_src");
    init(&src, &bin);
    let agent = run(
        &src,
        &bin,
        &[
            "agent",
            "register",
            "--name",
            "tester",
            "--provider",
            "test",
        ],
    );
    assert!(agent.status.success(), "agent register failed: {agent:?}");
    let bundle = src.join("bundle");
    export_bundle(&src, &bin, &bundle);

    let (fresh, _) = common::setup_test_project("merge_fresh_tgt");
    assert!(!db_path(&fresh).exists());
    let merged = run(
        &fresh,
        &bin,
        &[
            "import",
            bundle.to_str().unwrap(),
            "--mode",
            "merge",
            "--json",
        ],
    );
    assert!(merged.status.success(), "merge on fresh failed: {merged:?}");
    let body = json(&merged);
    assert_eq!(body["data"]["mode"], "init");
    assert_eq!(body["data"]["operation"]["applied"], true);
    assert!(db_path(&fresh).exists());
}

#[test]
fn merge_disjoint_edits_applies_union_and_records_provenance() {
    let p = seed_pair("merge_disjoint");
    create_task(&p.src, &p.bin, "source only");
    create_task(&p.tgt, &p.bin, "target only");

    let bundle = p.src.join("bundle-src");
    export_bundle(&p.src, &p.bin, &bundle);

    let merged = run(
        &p.tgt,
        &p.bin,
        &[
            "import",
            bundle.to_str().unwrap(),
            "--mode",
            "merge",
            "--json",
        ],
    );
    assert!(merged.status.success(), "merge failed: {merged:?}");
    let body = json(&merged);
    let data = &body["data"];
    assert_eq!(data["mode"], "merge");
    assert_eq!(data["operation"]["applied"], true);
    assert_eq!(data["baseSource"], "none");
    assert_eq!(data["degraded"], true);
    assert!(data["mergeId"].as_str().is_some_and(|id| id.len() == 26));
    let warnings: Vec<&str> = data["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        warnings.iter().any(|w| w.contains("degraded")),
        "degraded warning missing: {warnings:?}"
    );

    assert!(task_title(&p.tgt, &task_id_by_title(&p.tgt, "base task")).is_some());
    assert!(task_title(&p.tgt, &task_id_by_title(&p.tgt, "source only")).is_some());
    assert!(task_title(&p.tgt, &task_id_by_title(&p.tgt, "target only")).is_some());

    let payloads = event_payloads(&p.tgt, "project.merged");
    assert_eq!(payloads.len(), 1, "exactly one project.merged event");
    assert_eq!(payloads[0]["baseSource"], "none");
    assert_eq!(payloads[0]["degraded"], true);
    assert_eq!(
        payloads[0]["theirsExportId"],
        read_manifest(&bundle)["export_id"]
    );
}

#[test]
fn merge_v1_bundle_degrades_with_warning() {
    let p = seed_pair("merge_v1");
    create_task(&p.src, &p.bin, "v1 addition");
    // v1 dumper: manifest without v2 fields and without tombstones.jsonl.
    let bundle = p.src.join("bundle-v1");
    dump_bundle_v1(&p.src, &bundle);

    let merged = run(
        &p.tgt,
        &p.bin,
        &[
            "import",
            bundle.to_str().unwrap(),
            "--mode",
            "merge",
            "--json",
        ],
    );
    assert!(merged.status.success(), "v1 merge failed: {merged:?}");
    let body = json(&merged);
    assert_eq!(body["data"]["baseSource"], "none");
    assert_eq!(body["data"]["degraded"], true);
    assert!(task_title(&p.tgt, &task_id_by_title(&p.tgt, "v1 addition")).is_some());
}

#[test]
fn merge_explicit_base_from_directory() {
    let p = seed_pair("merge_explicit");
    let edited = run(
        &p.src,
        &p.bin,
        &["task", "edit", &p.base_task, "--title", "source edit"],
    );
    assert!(edited.status.success(), "src edit failed: {edited:?}");
    let bundle = p.src.join("bundle-src");
    export_bundle(&p.src, &p.bin, &bundle);

    let merged = run(
        &p.tgt,
        &p.bin,
        &[
            "import",
            bundle.to_str().unwrap(),
            "--mode",
            "merge",
            "--base",
            p.base_bundle.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(merged.status.success(), "explicit merge failed: {merged:?}");
    let body = json(&merged);
    assert_eq!(body["data"]["baseSource"], "explicit");
    assert_eq!(body["data"]["degraded"], false);
    assert_eq!(
        task_title(&p.tgt, &p.base_task).as_deref(),
        Some("source edit")
    );
}

#[test]
fn merge_ancestor_base_from_snapshot_cache() {
    let p = seed_pair("merge_ancestor");
    // `ours` is at the base export and its bundle is in the local cache.
    set_last_export_id(&p.tgt, &p.base_export_id);
    seed_snapshot_cache(&p.tgt, &p.base_export_id, &p.base_bundle);

    let edited = run(
        &p.src,
        &p.bin,
        &["task", "edit", &p.base_task, "--title", "source edit"],
    );
    assert!(edited.status.success(), "src edit failed: {edited:?}");
    let bundle = p.src.join("bundle-src");
    export_bundle(&p.src, &p.bin, &bundle);
    set_manifest_edges(&bundle, "01MERGEANCESTOR00000000002", &[&p.base_export_id]);

    let merged = run(
        &p.tgt,
        &p.bin,
        &[
            "import",
            bundle.to_str().unwrap(),
            "--mode",
            "merge",
            "--json",
        ],
    );
    assert!(merged.status.success(), "ancestor merge failed: {merged:?}");
    let body = json(&merged);
    assert_eq!(body["data"]["baseSource"], "ancestor");
    assert_eq!(body["data"]["baseExportId"], p.base_export_id);
    assert_eq!(body["data"]["degraded"], false);
    assert_eq!(
        task_title(&p.tgt, &p.base_task).as_deref(),
        Some("source edit")
    );
    let payloads = event_payloads(&p.tgt, "project.merged");
    assert_eq!(payloads[0]["baseSource"], "ancestor");
}

#[test]
fn merge_snapshot_base_when_no_common_ancestor() {
    let p = seed_pair("merge_snapshot");
    set_last_export_id(&p.tgt, &p.base_export_id);
    seed_snapshot_cache(&p.tgt, &p.base_export_id, &p.base_bundle);

    create_task(&p.src, &p.bin, "disjoint source task");
    let bundle = p.src.join("bundle-src");
    export_bundle(&p.src, &p.bin, &bundle);
    set_manifest_edges(&bundle, "01MERGESNAPSHOT000000000002", &[]);

    let merged = run(
        &p.tgt,
        &p.bin,
        &[
            "import",
            bundle.to_str().unwrap(),
            "--mode",
            "merge",
            "--json",
        ],
    );
    assert!(merged.status.success(), "snapshot merge failed: {merged:?}");
    let body = json(&merged);
    assert_eq!(body["data"]["baseSource"], "snapshot");
    assert_eq!(body["data"]["degraded"], false);
}

#[test]
fn merge_require_base_refuses_without_base_and_leaves_no_state() {
    let p = seed_pair("merge_require_base");
    create_task(&p.src, &p.bin, "no base edit");
    let bundle = p.src.join("bundle-src");
    export_bundle(&p.src, &p.bin, &bundle);

    let before = db_bytes(&p.tgt);
    let refused = run(
        &p.tgt,
        &p.bin,
        &[
            "import",
            bundle.to_str().unwrap(),
            "--mode",
            "merge",
            "--require-base",
            "--json",
        ],
    );
    assert_eq!(refused.status.code(), Some(8), "refused: {refused:?}");
    let body = json(&refused);
    assert_eq!(body["error"]["code"], "VALIDATION_FAILED");
    assert_eq!(body["error"]["details"]["kind"], "base_required_missing");
    assert_eq!(db_bytes(&p.tgt), before);
    assert!(merge_session_dirs(&p.tgt).is_empty());
}

// ── Conflict staging ─────────────────────────────────────────────────────

fn stage_strict_conflict(name: &str) -> (Pair, PathBuf, serde_json::Value) {
    let p = seed_pair(name);
    let src_edit = run(
        &p.src,
        &p.bin,
        &["task", "edit", &p.base_task, "--title", "source edit"],
    );
    assert!(src_edit.status.success(), "src edit failed: {src_edit:?}");
    let tgt_edit = run(
        &p.tgt,
        &p.bin,
        &["task", "edit", &p.base_task, "--title", "target edit"],
    );
    assert!(tgt_edit.status.success(), "tgt edit failed: {tgt_edit:?}");
    let bundle = p.src.join("bundle-src");
    export_bundle(&p.src, &p.bin, &bundle);

    let before = db_bytes(&p.tgt);
    let conflicted = run(
        &p.tgt,
        &p.bin,
        &[
            "import",
            bundle.to_str().unwrap(),
            "--mode",
            "merge",
            "--strict-edits",
            "--json",
        ],
    );
    assert_eq!(
        conflicted.status.code(),
        Some(3),
        "conflict: {conflicted:?}"
    );
    let body = json(&conflicted);
    assert_eq!(body["error"]["code"], "MERGE_CONFLICTS");
    assert_eq!(db_bytes(&p.tgt), before, "live DB must stay untouched");
    // Store the baseline bytes alongside the envelope for later assertions.
    assert_eq!(body["error"]["details"]["conflicts"], 1);
    (p, bundle, body)
}

#[test]
fn merge_strict_edits_stages_conflict_and_leaves_live_db_untouched() {
    let (p, _bundle, body) = stage_strict_conflict("merge_conflict");
    let merge_id = body["error"]["details"]["mergeId"].as_str().unwrap();
    assert_eq!(merge_id.len(), 26);

    let dir = state_dir(&p.tgt).join("merges").join(merge_id);
    let merge_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("merge.json")).unwrap()).unwrap();
    assert_eq!(merge_json["status"], "conflicts_open");
    assert_eq!(merge_json["mergeId"], merge_id);
    assert_eq!(merge_json["conflictCount"], 1);

    let conflicts: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("conflicts.json")).unwrap())
            .unwrap();
    let conflicts = conflicts.as_array().unwrap();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0]["kind"], "row_edit");
    assert_eq!(conflicts[0]["table"], "tasks");
    assert!(conflicts[0]["resolution"].is_null());
    assert!(conflicts[0]["base"].is_object() || conflicts[0]["base"].is_null());
    assert!(dir.join("candidate.sqlite").exists());
    assert!(dir.join("theirs").join("manifest.json").exists());

    // The live DB still has the target-side edit.
    assert_eq!(
        task_title(&p.tgt, &p.base_task).as_deref(),
        Some("target edit")
    );
    // No merge completion event was appended.
    assert!(event_payloads(&p.tgt, "project.merged").is_empty());
}

#[test]
fn merge_second_run_refuses_while_session_active() {
    let (p, _bundle, body) = stage_strict_conflict("merge_one_active");
    let merge_id = body["error"]["details"]["mergeId"]
        .as_str()
        .unwrap()
        .to_string();

    let second = run(
        &p.tgt,
        &p.bin,
        &[
            "import",
            p.src.join("bundle-src").to_str().unwrap(),
            "--mode",
            "merge",
            "--json",
        ],
    );
    assert_eq!(second.status.code(), Some(3), "second: {second:?}");
    let second_body = json(&second);
    assert_eq!(second_body["error"]["code"], "STATE_CONFLICT");
    assert!(
        second_body["error"]["message"]
            .as_str()
            .unwrap()
            .contains(&merge_id),
        "refusal must name the pending merge: {second_body}"
    );
}

#[test]
fn merge_dry_run_reports_conflicts_and_writes_nothing() {
    let p = seed_pair("merge_dry_run");
    let src_edit = run(
        &p.src,
        &p.bin,
        &["task", "edit", &p.base_task, "--title", "source edit"],
    );
    assert!(src_edit.status.success());
    let tgt_edit = run(
        &p.tgt,
        &p.bin,
        &["task", "edit", &p.base_task, "--title", "target edit"],
    );
    assert!(tgt_edit.status.success());
    let bundle = p.src.join("bundle-src");
    export_bundle(&p.src, &p.bin, &bundle);

    let before = db_bytes(&p.tgt);
    let preview = run(
        &p.tgt,
        &p.bin,
        &[
            "import",
            bundle.to_str().unwrap(),
            "--mode",
            "merge",
            "--strict-edits",
            "--dry-run",
            "--json",
        ],
    );
    assert!(
        preview.status.success(),
        "dry-run must not fail on conflicts: {preview:?}"
    );
    let body = json(&preview);
    assert_eq!(body["data"]["operation"]["applied"], false);
    assert_eq!(body["data"]["wouldConflict"], true);
    assert_eq!(body["data"]["conflictCount"], 1);
    assert_eq!(db_bytes(&p.tgt), before);
    assert!(merge_session_dirs(&p.tgt).is_empty());
}

#[test]
fn merge_redacted_bundle_refused() {
    let p = seed_pair("merge_redacted");
    create_task(&p.src, &p.bin, "redacted edit");
    let bundle = p.src.join("bundle-src");
    export_bundle(&p.src, &p.bin, &bundle);
    set_manifest_redacted(&bundle);

    let before = db_bytes(&p.tgt);
    let refused = run(
        &p.tgt,
        &p.bin,
        &[
            "import",
            bundle.to_str().unwrap(),
            "--mode",
            "merge",
            "--json",
        ],
    );
    assert_eq!(refused.status.code(), Some(10), "refused: {refused:?}");
    assert_eq!(json(&refused)["error"]["code"], "UNSUPPORTED_OPERATION");
    assert_eq!(db_bytes(&p.tgt), before);
    assert!(merge_session_dirs(&p.tgt).is_empty());
}

// ── Apply semantics ──────────────────────────────────────────────────────

#[test]
fn merge_lww_election_applies_and_records_auto_resolution() {
    let p = seed_pair("merge_lww");
    let src_edit = run(
        &p.src,
        &p.bin,
        &["task", "edit", &p.base_task, "--title", "source wins"],
    );
    assert!(src_edit.status.success());
    let tgt_edit = run(
        &p.tgt,
        &p.bin,
        &["task", "edit", &p.base_task, "--title", "target loses"],
    );
    assert!(tgt_edit.status.success());
    // Deterministic LWW: the source row carries the newer `updated_at`.
    set_task_updated_at(&p.src, &p.base_task, "2030-01-01T00:00:00Z");
    set_task_updated_at(&p.tgt, &p.base_task, "2020-01-01T00:00:00Z");

    let bundle = p.src.join("bundle-src");
    export_bundle(&p.src, &p.bin, &bundle);
    let merged = run(
        &p.tgt,
        &p.bin,
        &[
            "import",
            bundle.to_str().unwrap(),
            "--mode",
            "merge",
            "--json",
        ],
    );
    assert!(merged.status.success(), "lww merge failed: {merged:?}");
    let body = json(&merged);
    assert_eq!(
        task_title(&p.tgt, &p.base_task).as_deref(),
        Some("source wins")
    );
    assert!(body["data"]["autoResolutions"].as_u64().unwrap() >= 1);
    assert!(body["data"]["conflicts"].as_u64().unwrap() == 0);
    assert_eq!(event_payloads(&p.tgt, "merge.auto_resolved").len(), 1);
}

#[test]
fn merge_applies_three_way_delete_from_tombstones() {
    let p = seed_pair("merge_delete");
    // A task with no audit-event references can be hard-deleted without
    // nulling an append-only event row; seed it directly in both clones.
    let doomed = "01DOOMEDTASK000000000000000";
    insert_task_direct(&p.src, doomed, "CTX-9001", "doomed");
    insert_task_direct(&p.tgt, doomed, "CTX-9001", "doomed");

    // The shared base includes the doomed task.
    let base_bundle = p.src.join("bundle-base2");
    export_bundle(&p.src, &p.bin, &base_bundle);
    let base_id = read_manifest(&base_bundle)["export_id"]
        .as_str()
        .unwrap()
        .to_string();
    set_last_export_id(&p.tgt, &base_id);
    seed_snapshot_cache(&p.tgt, &base_id, &base_bundle);

    // Source deletes it and exports with the delete as a tombstone.
    delete_task_with_tombstone(&p.src, doomed);
    let bundle = p.src.join("bundle-src");
    export_bundle(&p.src, &p.bin, &bundle);
    set_manifest_edges(&bundle, "01MERGEDELETE0000000000002", &[&base_id]);

    let merged = run(
        &p.tgt,
        &p.bin,
        &[
            "import",
            bundle.to_str().unwrap(),
            "--mode",
            "merge",
            "--json",
        ],
    );
    assert!(merged.status.success(), "delete merge failed: {merged:?}");
    assert_eq!(json(&merged)["data"]["baseSource"], "ancestor");
    assert!(task_title(&p.tgt, doomed).is_none(), "task must be deleted");
    assert_eq!(tombstone_count(&p.tgt, "tasks", doomed), 1);
}

#[test]
fn merge_prunes_missing_worktree_and_nulls_dangling_refs() {
    let p = seed_pair("merge_worktree");
    let worktree_id = "01WORKTREE00000000000000001";
    let session_id = "01SESSION000000000000000001";
    insert_worktree_and_session(
        &p.src,
        &p.base_task,
        worktree_id,
        session_id,
        "/nonexistent/carryctx-worktree",
    );
    let bundle = p.src.join("bundle-src");
    export_bundle(&p.src, &p.bin, &bundle);

    let merged = run(
        &p.tgt,
        &p.bin,
        &[
            "import",
            bundle.to_str().unwrap(),
            "--mode",
            "merge",
            "--json",
        ],
    );
    assert!(merged.status.success(), "worktree merge failed: {merged:?}");
    let body = json(&merged);
    let warnings: Vec<&str> = body["data"]["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        warnings.iter().any(|w| w.contains("Pruned worktree")),
        "missing prune warning: {warnings:?}"
    );
    assert!(
        warnings.iter().any(|w| w.contains("worktree reference")),
        "missing nulled-reference warning: {warnings:?}"
    );

    let conn = rusqlite::Connection::open(db_path(&p.tgt)).unwrap();
    let worktrees: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM worktrees WHERE id = ?1",
            [worktree_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(worktrees, 0);
    let session_worktree: Option<String> = conn
        .query_row(
            "SELECT worktree_id FROM sessions WHERE id = ?1",
            [session_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(session_worktree, None);
    drop(conn);
    assert_eq!(event_payloads(&p.tgt, "worktree.pruned").len(), 1);
}

#[test]
fn merge_candidate_failure_rolls_back_and_cleans_up() {
    let p = seed_pair("merge_rollback");
    create_task(&p.src, &p.bin, "rollback edit");
    let bundle = p.src.join("bundle-src");
    export_bundle(&p.src, &p.bin, &bundle);
    append_task_dependency(
        &bundle,
        &project_id(&p.src),
        "01BADDEPENDENCY00000000001",
        "01MISSINGTASK0000000000001",
        "01MISSINGPREREQ000000000001",
    );

    let before = db_bytes(&p.tgt);
    let failed = run(
        &p.tgt,
        &p.bin,
        &[
            "import",
            bundle.to_str().unwrap(),
            "--mode",
            "merge",
            "--json",
        ],
    );
    assert!(
        !failed.status.success(),
        "broken bundle must fail: {failed:?}"
    );
    assert_eq!(json(&failed)["error"]["code"], "DATABASE_ERROR");
    assert_eq!(db_bytes(&p.tgt), before, "live DB must be untouched");
    assert!(merge_session_dirs(&p.tgt).is_empty());
    let leftovers: Vec<String> = std::fs::read_dir(state_dir(&p.tgt))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains("restore_") || name.contains("original_"))
        .collect();
    assert!(leftovers.is_empty(), "swap artifacts left: {leftovers:?}");
    let journals: Vec<String> = std::fs::read_dir(state_dir(&p.tgt).join("journals"))
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    assert!(journals.is_empty(), "journals left: {journals:?}");
}

// ── Crash recovery (restore-journal reuse) ───────────────────────────────

#[test]
fn merge_swap_journal_rolls_back_before_rename() {
    let (repo, _) = common::setup_test_project("merge_journal_rollback");
    init(&repo, &test_bin());
    let db = db_path(&repo);
    let state = state_dir(&repo);
    let operation_id = ulid::Ulid::generate().to_string();
    let candidate = state.join(format!("state.sqlite.restore_{operation_id}"));
    let original = state.join(format!("state.sqlite.original_{operation_id}"));
    std::fs::copy(&db, &candidate).unwrap();
    std::fs::hard_link(&db, &original).unwrap();
    write_journal(
        &state.join("journals"),
        &JournalEntry {
            operation_id: operation_id.to_string(),
            kind: "project.restore".into(),
            status: "prepared".into(),
            created_at: chrono::Utc::now().to_rfc3339(),
            metadata: serde_json::json!({
                "databasePath": db.to_string_lossy(),
                "candidatePath": candidate.to_string_lossy(),
                "originalPath": original.to_string_lossy(),
            }),
        },
    )
    .unwrap();

    let xdg = XdgPaths::new();
    let common_dir = repo.join(".git");
    project_mgmt::recover_restore_journals(&xdg, &common_dir).unwrap();

    // Database still exists, candidate/original cleaned, journal removed.
    assert!(db.exists());
    assert!(!candidate.exists());
    assert!(!original.exists());
    assert!(
        !state
            .join("journals")
            .join(format!("{operation_id}.json"))
            .exists()
    );
}

#[test]
fn merge_swap_journal_recovers_candidate_when_database_missing() {
    let (repo, _) = common::setup_test_project("merge_journal_forward");
    init(&repo, &test_bin());
    let db = db_path(&repo);
    let state = state_dir(&repo);
    let operation_id = ulid::Ulid::generate().to_string();
    let candidate = state.join(format!("state.sqlite.restore_{operation_id}"));
    let original = state.join(format!("state.sqlite.original_{operation_id}"));
    std::fs::copy(&db, &candidate).unwrap();
    let candidate_bytes = std::fs::read(&candidate).unwrap();
    std::fs::remove_file(&db).unwrap();
    write_journal(
        &state.join("journals"),
        &JournalEntry {
            operation_id: operation_id.to_string(),
            kind: "project.restore".into(),
            status: "prepared".into(),
            created_at: chrono::Utc::now().to_rfc3339(),
            metadata: serde_json::json!({
                "databasePath": db.to_string_lossy(),
                "candidatePath": candidate.to_string_lossy(),
                "originalPath": original.to_string_lossy(),
            }),
        },
    )
    .unwrap();

    let xdg = XdgPaths::new();
    let common_dir = repo.join(".git");
    project_mgmt::recover_restore_journals(&xdg, &common_dir).unwrap();

    assert_eq!(std::fs::read(&db).unwrap(), candidate_bytes);
    assert!(!candidate.exists());
}

// ── Test helpers (fixture SQL + v1 dumper) ───────────────────────────────

fn test_bin() -> PathBuf {
    common::test_binary()
}

fn set_task_updated_at(repo: &Path, task_id: &str, updated_at: &str) {
    let conn = rusqlite::Connection::open(db_path(repo)).unwrap();
    conn.execute(
        "UPDATE tasks SET updated_at = ?1 WHERE id = ?2",
        rusqlite::params![updated_at, task_id],
    )
    .unwrap();
}

fn tombstone_count(repo: &Path, table: &str, row_id: &str) -> i64 {
    let conn = rusqlite::Connection::open(db_path(repo)).unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM tombstones WHERE table_name = ?1 AND row_id = ?2",
        rusqlite::params![table, row_id],
        |row| row.get(0),
    )
    .unwrap()
}

fn insert_task_direct(repo: &Path, task_id: &str, display_id: &str, title: &str) {
    let conn = rusqlite::Connection::open(db_path(repo)).unwrap();
    let project_id = project_id(repo);
    // Fixed timestamps keep the row byte-identical across both clones, so the
    // only divergence is the source-side delete.
    let now = "2026-01-01T00:00:00Z";
    conn.execute(
        "INSERT INTO tasks (id, project_id, display_id, title, status, priority, metadata_json, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, 'planned', 'normal', '{}', ?5, ?5)",
        rusqlite::params![task_id, project_id, display_id, title, now],
    )
    .unwrap();
}

fn delete_task_with_tombstone(repo: &Path, task_id: &str) {
    let conn = rusqlite::Connection::open(db_path(repo)).unwrap();
    let project_id = project_id(repo);
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute("DELETE FROM tasks WHERE id = ?1", [task_id])
        .unwrap();
    conn.execute(
        "INSERT INTO tombstones (project_id, table_name, row_id, deleted_at, deleted_by, reason)
         VALUES (?1, 'tasks', ?2, ?3, NULL, 'integration-test delete')",
        rusqlite::params![project_id, task_id, now],
    )
    .unwrap();
}

fn insert_worktree_and_session(
    repo: &Path,
    task_id: &str,
    worktree_id: &str,
    session_id: &str,
    path: &str,
) {
    let conn = rusqlite::Connection::open(db_path(repo)).unwrap();
    let project_id = project_id(repo);
    let tester = agent_id(repo, "tester");
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO worktrees (id, project_id, task_id, normalized_path, git_common_dir, branch, head, bound_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'fixture', 'deadbeef', ?6, ?6)",
        rusqlite::params![
            worktree_id,
            project_id,
            task_id,
            path,
            format!("{path}/.git"),
            now
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sessions (id, project_id, agent_id, task_id, worktree_id, state, provider, working_directory, metadata_json, started_at, last_activity_at, updated_at)
         VALUES (?1, ?2, ?3, NULL, ?4, 'active', 'test', ?5, '{}', ?6, ?6, ?6)",
        rusqlite::params![session_id, project_id, tester, worktree_id, path, now],
    )
    .unwrap();
}

/// Append an FK-breaking row and keep the manifest counts honest.
fn append_task_dependency(
    bundle: &Path,
    project_id: &str,
    id: &str,
    task_id: &str,
    prerequisite_task_id: &str,
) {
    use std::io::Write as _;
    let file = bundle.join("task_dependencies.jsonl");
    let existing = std::fs::read_to_string(&file).unwrap_or_default();
    let count = existing
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count() as u64
        + 1;
    let mut handle = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
        .unwrap();
    let row = serde_json::json!({
        "id": id,
        "project_id": project_id,
        "task_id": task_id,
        "prerequisite_task_id": prerequisite_task_id,
        "kind": "strong",
        "created_at": chrono::Utc::now().to_rfc3339(),
    });
    writeln!(handle, "{}", serde_json::to_string(&row).unwrap()).unwrap();
    let mut manifest = read_manifest(bundle);
    manifest["counts"]["task_dependencies"] = serde_json::json!(count);
    write_manifest(bundle, &manifest);
}

/// v1 bundle dumper (no parents/tombstones), mirroring the writer an older
/// release would have produced.
fn dump_bundle_v1(src_repo: &Path, bundle_dir: &Path) {
    const TABLES: &[&str] = &[
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
    let conn = rusqlite::Connection::open(db_path(src_repo)).unwrap();
    std::fs::create_dir_all(bundle_dir).unwrap();

    let project_row = read_all_rows(&conn, "projects").remove(0);
    let project_id = project_row["id"].as_str().unwrap().to_string();
    std::fs::write(
        bundle_dir.join("project.json"),
        serde_json::to_string(&project_row).unwrap(),
    )
    .unwrap();

    let mut counts = serde_json::Map::new();
    for table in TABLES {
        let rows = read_all_rows(&conn, table);
        if !rows.is_empty() {
            counts.insert(
                (*table).to_string(),
                serde_json::Value::from(rows.len() as u64),
            );
        }
        let mut text = String::new();
        for row in &rows {
            text.push_str(&serde_json::to_string(row).unwrap());
            text.push('\n');
        }
        std::fs::write(bundle_dir.join(format!("{table}.jsonl")), text).unwrap();
    }

    let manifest = serde_json::json!({
        "format": "carryctx-pack-dir",
        "format_version": 1,
        "carryctx_version": env!("CARGO_PKG_VERSION"),
        "schema_version": 18,
        "project_id": project_id,
        "export_id": ulid::Ulid::generate().to_string(),
        "created_at": chrono::Utc::now().to_rfc3339(),
        "parents": [],
        "sequences": {},
        "source": {"hostname": "test"},
        "counts": counts,
    });
    std::fs::write(
        bundle_dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

fn read_all_rows(conn: &rusqlite::Connection, table: &str) -> Vec<serde_json::Value> {
    let sql = format!("SELECT * FROM {table}");
    let mut stmt = conn.prepare(&sql).unwrap();
    let names: Vec<String> = stmt
        .column_names()
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    let rows = stmt
        .query_map([], |row| {
            let mut map = serde_json::Map::new();
            for (i, name) in names.iter().enumerate() {
                let value: rusqlite::types::Value = row.get(i)?;
                map.insert(name.clone(), sql_to_json(value));
            }
            Ok(serde_json::Value::Object(map))
        })
        .unwrap();
    rows.map(|row| row.unwrap()).collect()
}

fn sql_to_json(value: rusqlite::types::Value) -> serde_json::Value {
    match value {
        rusqlite::types::Value::Null => serde_json::Value::Null,
        rusqlite::types::Value::Integer(i) => serde_json::json!(i),
        rusqlite::types::Value::Real(f) => serde_json::json!(f),
        rusqlite::types::Value::Text(s) => serde_json::Value::String(s),
        rusqlite::types::Value::Blob(b) => {
            serde_json::Value::String(String::from_utf8_lossy(&b).into_owned())
        }
    }
}
