//! CTX-0143 integration tests: `conflict list/show/resolve/apply/abort`
//! over merge sessions staged by CTX-0142 (design
//! `2026-09-10-mergeable-git-managed-state.md` §2.4–§2.6, AC4/AC6/AC9).

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

fn run(repo: &Path, bin: &Path, args: &[&str]) -> std::process::Output {
    common::run_cmd(repo, bin, args)
}

fn db_path(repo: &Path) -> PathBuf {
    repo.join(".git/carryctx/state.sqlite")
}

fn state_dir(repo: &Path) -> PathBuf {
    repo.join(".git/carryctx")
}

fn project_id(repo: &Path) -> String {
    let conn = rusqlite::Connection::open(db_path(repo)).unwrap();
    conn.query_row("SELECT id FROM projects LIMIT 1", [], |row| row.get(0))
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

/// `session_id` of every event of `event_type`, in insertion order.
fn event_session_ids(repo: &Path, event_type: &str) -> Vec<Option<String>> {
    let conn = rusqlite::Connection::open(db_path(repo)).unwrap();
    let mut stmt = conn
        .prepare("SELECT session_id FROM events WHERE type = ?1 ORDER BY occurred_at")
        .unwrap();
    let rows = stmt
        .query_map([event_type], |row| row.get::<_, Option<String>>(0))
        .unwrap();
    rows.map(Result::unwrap).collect()
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

fn journal_count(repo: &Path) -> usize {
    std::fs::read_dir(state_dir(repo).join("journals"))
        .map(|entries| entries.filter_map(Result::ok).count())
        .unwrap_or(0)
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

/// Read `conflicts.json` for the single staged session.
fn conflicts_json(repo: &Path) -> serde_json::Value {
    let dirs = merge_session_dirs(repo);
    assert_eq!(
        dirs.len(),
        1,
        "expected exactly one staged session: {dirs:?}"
    );
    let path = state_dir(repo)
        .join("merges")
        .join(&dirs[0])
        .join("conflicts.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn first_conflict_id(repo: &Path, kind: &str) -> String {
    conflicts_json(repo)
        .as_array()
        .unwrap()
        .iter()
        .find(|conflict| conflict["kind"] == kind)
        .unwrap_or_else(|| panic!("no '{kind}' conflict staged"))
        .get("id")
        .unwrap()
        .as_str()
        .unwrap()
        .to_string()
}

struct Pair {
    src: PathBuf,
    tgt: PathBuf,
    bin: PathBuf,
    base_task: String,
}

/// Two repositories sharing one project identity, both holding "base task".
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
    let base_task = task_id_by_title(&src, "base task");

    let base_bundle = src.join("bundle-base");
    export_bundle(&src, &bin, &base_bundle);

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
        base_task,
    }
}

/// Stage a single strict `row_edit` conflict (source vs target edits).
fn stage_strict_conflict(name: &str) -> (Pair, String) {
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
        "conflict staging: {conflicted:?}"
    );
    let body = json(&conflicted);
    let merge_id = body["error"]["details"]["mergeId"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(db_bytes(&p.tgt), before, "live DB must stay untouched");
    (p, merge_id)
}

fn insert_task_direct(repo: &Path, task_id: &str, display_id: &str, title: &str) {
    let conn = rusqlite::Connection::open(db_path(repo)).unwrap();
    let project_id = project_id(repo);
    conn.execute(
        "INSERT INTO tasks (id, project_id, display_id, title, status, priority, metadata_json, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, 'planned', 'normal', '{}', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        rusqlite::params![task_id, project_id, display_id, title],
    )
    .unwrap();
}

fn delete_task_with_tombstone(repo: &Path, task_id: &str) {
    let conn = rusqlite::Connection::open(db_path(repo)).unwrap();
    let project_id = project_id(repo);
    conn.execute("DELETE FROM tasks WHERE id = ?1", [task_id])
        .unwrap();
    conn.execute(
        "INSERT INTO tombstones (project_id, table_name, row_id, deleted_at, deleted_by, reason)
         VALUES (?1, 'tasks', ?2, '2026-01-02T00:00:00Z', NULL, 'conflict test delete')",
        rusqlite::params![project_id, task_id],
    )
    .unwrap();
}

const DOOMED: &str = "01DOOMEDEDIT00000000000001";

/// Stage a session carrying a blocking `delete_vs_edit` conflict plus at
/// least one auto-resolved `row_edit` (source and target both edited the
/// shared base task).
fn stage_mixed_conflict(name: &str) -> (Pair, String) {
    let p = seed_pair(name);
    let src_edit = run(
        &p.src,
        &p.bin,
        &["task", "edit", &p.base_task, "--title", "source auto"],
    );
    assert!(
        src_edit.status.success(),
        "src auto edit failed: {src_edit:?}"
    );
    let tgt_edit = run(
        &p.tgt,
        &p.bin,
        &["task", "edit", &p.base_task, "--title", "target auto"],
    );
    assert!(
        tgt_edit.status.success(),
        "tgt auto edit failed: {tgt_edit:?}"
    );
    insert_task_direct(&p.src, DOOMED, "CTX-9002", "doomed");
    insert_task_direct(&p.tgt, DOOMED, "CTX-9002", "doomed");
    delete_task_with_tombstone(&p.src, DOOMED);
    let doomed_edit = run(
        &p.tgt,
        &p.bin,
        &["task", "edit", DOOMED, "--title", "target edited"],
    );
    assert!(
        doomed_edit.status.success(),
        "doomed edit failed: {doomed_edit:?}"
    );
    let bundle = p.src.join("bundle-src-mixed");
    export_bundle(&p.src, &p.bin, &bundle);

    let conflicted = run(
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
    assert_eq!(
        conflicted.status.code(),
        Some(3),
        "mixed conflict staging: {conflicted:?}"
    );
    let merge_id = json(&conflicted)["error"]["details"]["mergeId"]
        .as_str()
        .unwrap()
        .to_string();
    (p, merge_id)
}

/// Stage a session whose only blocking conflict is a `delete_vs_edit`.
fn stage_delete_vs_edit(name: &str) -> (Pair, String) {
    let p = seed_pair(name);
    insert_task_direct(&p.src, DOOMED, "CTX-9002", "doomed");
    insert_task_direct(&p.tgt, DOOMED, "CTX-9002", "doomed");
    delete_task_with_tombstone(&p.src, DOOMED);
    let doomed_edit = run(
        &p.tgt,
        &p.bin,
        &["task", "edit", DOOMED, "--title", "target edited"],
    );
    assert!(
        doomed_edit.status.success(),
        "doomed edit failed: {doomed_edit:?}"
    );
    let bundle = p.src.join("bundle-src-dv");
    export_bundle(&p.src, &p.bin, &bundle);
    let conflicted = run(
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
    assert_eq!(
        conflicted.status.code(),
        Some(3),
        "delete_vs_edit staging: {conflicted:?}"
    );
    let merge_id = json(&conflicted)["error"]["details"]["mergeId"]
        .as_str()
        .unwrap()
        .to_string();
    (p, merge_id)
}

// ── list / show ──────────────────────────────────────────────────────────

#[test]
fn conflict_list_open_and_all_includes_auto_resolutions() {
    let (p, merge_id) = stage_mixed_conflict("conflict_list");

    let open = run(&p.tgt, &p.bin, &["conflict", "list", "--json"]);
    assert!(open.status.success(), "list failed: {open:?}");
    let data = &json(&open)["data"];
    assert_eq!(data["mergeId"], merge_id);
    assert_eq!(data["status"], "conflicts_open");
    assert!(data["open"].as_u64().unwrap() >= 1);
    assert_eq!(data["resolved"], 0);
    assert_eq!(
        data["conflicts"].as_array().unwrap().len() as u64,
        data["open"].as_u64().unwrap()
    );
    assert!(
        data["autoResolutions"].as_array().unwrap().is_empty(),
        "default list must not include auto-resolutions"
    );
    assert_eq!(data["conflicts"][0]["kind"], "delete_vs_edit");
    assert!(data["conflicts"][0]["resolution"].is_null());

    let all = run(&p.tgt, &p.bin, &["conflict", "list", "--all", "--json"]);
    assert!(all.status.success(), "list --all failed: {all:?}");
    let all_data = &json(&all)["data"];
    assert!(
        !all_data["autoResolutions"].as_array().unwrap().is_empty(),
        "--all must surface the staged auto-resolutions: {all_data}"
    );

    let selected = run(
        &p.tgt,
        &p.bin,
        &["conflict", "list", "--merge", &merge_id, "--json"],
    );
    assert!(
        selected.status.success(),
        "--merge list failed: {selected:?}"
    );
    assert_eq!(json(&selected)["data"]["mergeId"], merge_id);
}

#[test]
fn conflict_list_without_session_is_resource_not_found() {
    let (src, bin) = common::setup_test_project("conflict_no_session");
    init(&src, &bin);
    let output = run(&src, &bin, &["conflict", "list", "--json"]);
    assert_eq!(output.status.code(), Some(7), "no session: {output:?}");
    assert_eq!(json(&output)["error"]["code"], "RESOURCE_NOT_FOUND");
}

#[test]
fn conflict_show_exposes_sides_and_unknown_id_is_exit_7() {
    let (p, _) = stage_strict_conflict("conflict_show");
    let conflict_id = first_conflict_id(&p.tgt, "row_edit");

    let shown = run(
        &p.tgt,
        &p.bin,
        &["conflict", "show", &conflict_id, "--json"],
    );
    assert!(shown.status.success(), "show failed: {shown:?}");
    let conflict = &json(&shown)["data"]["conflict"];
    assert_eq!(conflict["id"], conflict_id);
    assert_eq!(conflict["table"], "tasks");
    assert!(conflict["ours"].is_object());
    assert!(conflict["theirs"].is_object());
    assert_eq!(conflict["ours"]["title"], "target edit");
    assert_eq!(conflict["theirs"]["title"], "source edit");
    assert!(conflict["reason"].as_str().unwrap().contains("changed"));
    assert!(conflict["resolution"].is_null());

    let markdown = run(
        &p.tgt,
        &p.bin,
        &["conflict", "show", &conflict_id, "--format", "markdown"],
    );
    assert!(
        markdown.status.success(),
        "markdown show failed: {markdown:?}"
    );
    let text = String::from_utf8_lossy(&markdown.stdout);
    assert!(text.contains(&conflict_id), "markdown: {text}");
    assert!(
        text.contains("ours") && text.contains("theirs"),
        "markdown: {text}"
    );

    let missing = run(&p.tgt, &p.bin, &["conflict", "show", "nope", "--json"]);
    assert_eq!(missing.status.code(), Some(7), "unknown id: {missing:?}");
    assert_eq!(json(&missing)["error"]["code"], "RESOURCE_NOT_FOUND");
}

// ── resolve / apply ──────────────────────────────────────────────────────

#[test]
fn conflict_resolve_theirs_applies_their_row_and_records_audit() {
    let (p, _) = stage_strict_conflict("resolve_theirs");
    let conflict_id = first_conflict_id(&p.tgt, "row_edit");

    let resolved = run(
        &p.tgt,
        &p.bin,
        &["conflict", "resolve", &conflict_id, "--theirs", "--json"],
    );
    assert!(resolved.status.success(), "resolve failed: {resolved:?}");
    let data = &json(&resolved)["data"];
    assert_eq!(data["choice"], "theirs");
    assert_eq!(data["openCount"], 0);
    assert_eq!(data["resolvedCount"], 1);

    let listed = run(&p.tgt, &p.bin, &["conflict", "list", "--json"]);
    let list_data = &json(&listed)["data"];
    assert_eq!(list_data["open"], 0);
    assert_eq!(list_data["resolved"], 1);

    let applied = run(&p.tgt, &p.bin, &["conflict", "apply", "--json"]);
    assert!(applied.status.success(), "apply failed: {applied:?}");
    let apply_data = &json(&applied)["data"];
    assert_eq!(apply_data["applied"], true);
    assert_eq!(apply_data["operation"]["applied"], true);
    assert_eq!(apply_data["resolvedCount"], 1);
    assert_eq!(apply_data["conflicts"], 0);
    assert_eq!(
        task_title(&p.tgt, &p.base_task).as_deref(),
        Some("source edit"),
        "the chosen (theirs) row must be live"
    );
    assert!(
        merge_session_dirs(&p.tgt).is_empty(),
        "staging must be removed"
    );

    let merged = event_payloads(&p.tgt, "project.merged");
    assert_eq!(merged.len(), 1, "exactly one project.merged: {merged:?}");
    assert_eq!(merged[0]["resolvedCount"], 1);
    let resolved_events = event_payloads(&p.tgt, "merge.conflict_resolved");
    assert_eq!(resolved_events.len(), 1);
    assert_eq!(resolved_events[0]["choice"], "theirs");
    assert_eq!(resolved_events[0]["table"], "tasks");
    assert_eq!(resolved_events[0]["key"], p.base_task);
}

#[test]
fn conflict_resolve_ours_applies_the_local_row() {
    let (p, _) = stage_strict_conflict("resolve_ours");
    let conflict_id = first_conflict_id(&p.tgt, "row_edit");
    let resolved = run(
        &p.tgt,
        &p.bin,
        &["conflict", "resolve", &conflict_id, "--ours", "--json"],
    );
    assert!(resolved.status.success(), "resolve failed: {resolved:?}");
    let applied = run(&p.tgt, &p.bin, &["conflict", "apply", "--json"]);
    assert!(applied.status.success(), "apply failed: {applied:?}");
    assert_eq!(
        task_title(&p.tgt, &p.base_task).as_deref(),
        Some("target edit"),
        "the chosen (ours) row must be live"
    );
}

#[test]
fn conflict_resolve_set_override_lands_and_unknown_field_is_exit_8() {
    let (p, _) = stage_strict_conflict("resolve_set");
    let conflict_id = first_conflict_id(&p.tgt, "row_edit");

    let rejected = run(
        &p.tgt,
        &p.bin,
        &[
            "conflict",
            "resolve",
            &conflict_id,
            "--theirs",
            "--set",
            "nope=1",
            "--json",
        ],
    );
    assert_eq!(
        rejected.status.code(),
        Some(8),
        "unknown field: {rejected:?}"
    );
    assert_eq!(json(&rejected)["error"]["code"], "VALIDATION_FAILED");
    // The rejected resolve must not have recorded a resolution.
    assert!(conflicts_json(&p.tgt)[0]["resolution"].is_null());

    let resolved = run(
        &p.tgt,
        &p.bin,
        &[
            "conflict",
            "resolve",
            &conflict_id,
            "--theirs",
            "--set",
            "title=override title",
            "--set",
            "priority=high",
            "--json",
        ],
    );
    assert!(resolved.status.success(), "resolve failed: {resolved:?}");
    let applied = run(&p.tgt, &p.bin, &["conflict", "apply", "--json"]);
    assert!(applied.status.success(), "apply failed: {applied:?}");
    assert_eq!(
        task_title(&p.tgt, &p.base_task).as_deref(),
        Some("override title")
    );
    let conn = rusqlite::Connection::open(db_path(&p.tgt)).unwrap();
    let priority: String = conn
        .query_row(
            "SELECT priority FROM tasks WHERE id = ?1",
            [&p.base_task],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(priority, "high", "--set must parse JSON scalars");
}

#[test]
fn conflict_apply_with_open_conflict_refuses_and_leaves_state() {
    let (p, merge_id) = stage_strict_conflict("apply_open");
    let before = db_bytes(&p.tgt);
    let output = run(&p.tgt, &p.bin, &["conflict", "apply", "--json"]);
    assert_eq!(output.status.code(), Some(3), "open apply: {output:?}");
    let body = json(&output);
    assert_eq!(body["error"]["code"], "MERGE_CONFLICTS");
    assert_eq!(body["error"]["details"]["mergeId"], merge_id);
    assert_eq!(body["error"]["details"]["conflicts"], 1);
    assert_eq!(db_bytes(&p.tgt), before, "live DB must be untouched");
    assert_eq!(merge_session_dirs(&p.tgt).len(), 1, "staging must remain");
}

#[test]
fn conflict_apply_skip_open_settles_ours_and_warns() {
    let (p, _) = stage_strict_conflict("apply_skip_open");
    let output = run(
        &p.tgt,
        &p.bin,
        &["conflict", "apply", "--skip-open", "--json"],
    );
    assert!(output.status.success(), "skip-open apply: {output:?}");
    let data = &json(&output)["data"];
    assert_eq!(data["applied"], true);
    let warnings: Vec<&str> = data["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|value| value.as_str())
        .collect();
    assert!(
        warnings.iter().any(|warning| warning.contains("Skipped")),
        "skip-open warning missing: {warnings:?}"
    );
    assert_eq!(
        task_title(&p.tgt, &p.base_task).as_deref(),
        Some("target edit"),
        "skipped conflicts stay at ours"
    );
    assert!(merge_session_dirs(&p.tgt).is_empty());
    let merged = event_payloads(&p.tgt, "project.merged");
    assert_eq!(merged[0]["skipOpen"], true);
}

#[test]
fn conflict_apply_dry_run_writes_nothing() {
    let (p, merge_id) = stage_strict_conflict("apply_dry_run");
    let conflict_id = first_conflict_id(&p.tgt, "row_edit");
    let resolved = run(
        &p.tgt,
        &p.bin,
        &["conflict", "resolve", &conflict_id, "--theirs", "--json"],
    );
    assert!(resolved.status.success(), "resolve failed: {resolved:?}");

    let session = state_dir(&p.tgt).join("merges").join(&merge_id);
    let candidate_before = std::fs::read(session.join("candidate.sqlite")).unwrap();
    let conflicts_before = std::fs::read(session.join("conflicts.json")).unwrap();
    let db_before = db_bytes(&p.tgt);

    let preview = run(
        &p.tgt,
        &p.bin,
        &["conflict", "apply", "--dry-run", "--json"],
    );
    assert!(preview.status.success(), "dry-run failed: {preview:?}");
    let data = &json(&preview)["data"];
    assert_eq!(data["applied"], false);
    assert_eq!(data["operation"]["applied"], false);
    assert_eq!(data["resolvedCount"], 1);

    assert_eq!(db_bytes(&p.tgt), db_before, "dry-run must not swap the DB");
    assert_eq!(
        std::fs::read(session.join("candidate.sqlite")).unwrap(),
        candidate_before,
        "dry-run must not touch the candidate"
    );
    assert_eq!(
        std::fs::read(session.join("conflicts.json")).unwrap(),
        conflicts_before,
        "dry-run must not touch the session"
    );
    assert_eq!(merge_session_dirs(&p.tgt).len(), 1);
    assert_eq!(journal_count(&p.tgt), 0, "dry-run must not journal");
}

#[test]
fn conflict_apply_dry_run_with_open_conflict_reports_without_writing() {
    let (p, merge_id) = stage_strict_conflict("apply_dry_run_open");
    let session = state_dir(&p.tgt).join("merges").join(&merge_id);
    let candidate_before = std::fs::read(session.join("candidate.sqlite")).unwrap();
    let db_before = db_bytes(&p.tgt);

    let preview = run(
        &p.tgt,
        &p.bin,
        &["conflict", "apply", "--dry-run", "--json"],
    );
    assert!(preview.status.success(), "dry-run failed: {preview:?}");
    let data = &json(&preview)["data"];
    assert_eq!(data["applied"], false);
    assert_eq!(data["wouldApply"], false);
    assert_eq!(data["openConflicts"], 1);
    assert_eq!(db_bytes(&p.tgt), db_before);
    assert_eq!(
        std::fs::read(session.join("candidate.sqlite")).unwrap(),
        candidate_before
    );
    assert_eq!(merge_session_dirs(&p.tgt).len(), 1);
    assert_eq!(journal_count(&p.tgt), 0);
}

#[test]
fn conflict_abort_removes_staging_and_leaves_db_byte_identical() {
    let (p, _) = stage_strict_conflict("abort");
    let before = db_bytes(&p.tgt);
    let output = run(&p.tgt, &p.bin, &["conflict", "abort", "--json"]);
    assert!(output.status.success(), "abort failed: {output:?}");
    let data = &json(&output)["data"];
    assert_eq!(data["aborted"], true);
    assert_eq!(data["operation"]["applied"], true);
    assert_eq!(db_bytes(&p.tgt), before, "abort must not change the DB");
    assert!(
        merge_session_dirs(&p.tgt).is_empty(),
        "staging must be removed"
    );
    assert_eq!(journal_count(&p.tgt), 0, "abort must not journal");
}

#[test]
fn conflict_delete_vs_edit_resolved_to_delete_removes_row_and_tombstones() {
    let (p, _) = stage_delete_vs_edit("delete_vs_edit");
    let conflict_id = first_conflict_id(&p.tgt, "delete_vs_edit");
    let resolved = run(
        &p.tgt,
        &p.bin,
        &["conflict", "resolve", &conflict_id, "--theirs", "--json"],
    );
    assert!(resolved.status.success(), "resolve failed: {resolved:?}");
    let applied = run(&p.tgt, &p.bin, &["conflict", "apply", "--json"]);
    assert!(applied.status.success(), "apply failed: {applied:?}");
    assert_eq!(task_title(&p.tgt, DOOMED), None, "chosen delete must win");
    assert_eq!(tombstone_count(&p.tgt, "tasks", DOOMED), 1);
    let merged = event_payloads(&p.tgt, "project.merged");
    assert_eq!(merged.len(), 1);
}

// ── crash recovery ───────────────────────────────────────────────────────

#[test]
fn conflict_apply_journal_recovery_installs_staged_candidate() {
    let (p, merge_id) = stage_strict_conflict("journal_recovery");
    let conflict_id = first_conflict_id(&p.tgt, "row_edit");
    let resolved = run(
        &p.tgt,
        &p.bin,
        &["conflict", "resolve", &conflict_id, "--theirs", "--json"],
    );
    assert!(resolved.status.success(), "resolve failed: {resolved:?}");

    let db = db_path(&p.tgt);
    let session = state_dir(&p.tgt).join("merges").join(&merge_id);
    // `apply` stages its swap candidate as the trusted sibling name the
    // restore journal accepts, so the kill-recovery fixture mirrors that.
    let candidate = state_dir(&p.tgt).join(format!("state.sqlite.restore_{merge_id}"));
    std::fs::copy(session.join("candidate.sqlite"), &candidate).unwrap();
    let candidate_bytes = std::fs::read(&candidate).unwrap();

    // Simulate a kill after the prepared journal but before the atomic rename:
    // the live database is gone and the journal must install the candidate.
    let operation_id = ulid::Ulid::generate().to_string();
    let original = state_dir(&p.tgt).join(format!("state.sqlite.original_{operation_id}"));
    std::fs::remove_file(&db).unwrap();
    write_journal(
        &state_dir(&p.tgt).join("journals"),
        &JournalEntry {
            operation_id: operation_id.clone(),
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
    let common_dir = p.tgt.join(".git");
    project_mgmt::recover_restore_journals(&xdg, &common_dir).unwrap();

    assert_eq!(std::fs::read(&db).unwrap(), candidate_bytes);
    assert!(!candidate.exists(), "recovery must consume the candidate");
}

// ── doctor ───────────────────────────────────────────────────────────────

#[test]
fn doctor_reports_active_merge_session_and_flags_stale() {
    let (p, merge_id) = stage_strict_conflict("doctor_merge");

    let report = run(&p.tgt, &p.bin, &["doctor", "--json"]);
    assert!(report.status.success(), "doctor failed: {report:?}");
    let body = json(&report);
    let check = body["data"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["check"] == "merges.active")
        .expect("merges.active check missing");
    assert_eq!(check["status"], "warning");
    let sessions = check["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["mergeId"], merge_id);
    assert_eq!(sessions[0]["conflictCount"], 1);
    assert_eq!(sessions[0]["stale"], false);

    // Age the session beyond the 24h threshold; doctor must flag it stale.
    let merge_json_path = state_dir(&p.tgt)
        .join("merges")
        .join(&merge_id)
        .join("merge.json");
    let mut merge_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&merge_json_path).unwrap()).unwrap();
    let stale_at = (chrono::Utc::now() - chrono::Duration::hours(48)).to_rfc3339();
    merge_json["createdAt"] = serde_json::json!(stale_at);
    std::fs::write(
        &merge_json_path,
        serde_json::to_string_pretty(&merge_json).unwrap(),
    )
    .unwrap();

    let aged = run(&p.tgt, &p.bin, &["doctor", "--json"]);
    assert!(aged.status.success(), "aged doctor failed: {aged:?}");
    let aged_check = json(&aged)["data"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["check"] == "merges.active")
        .cloned()
        .unwrap();
    assert_eq!(aged_check["sessions"][0]["stale"], true);
}

// ── CTX-0143 review fixes: unique_key, task-delete cascade, dry-run, retry ──

fn team_ids(repo: &Path) -> Vec<String> {
    let conn = rusqlite::Connection::open(db_path(repo)).unwrap();
    let mut stmt = conn.prepare("SELECT id FROM teams ORDER BY id").unwrap();
    let rows = stmt.query_map([], |row| row.get::<_, String>(0)).unwrap();
    rows.map(|row| row.unwrap()).collect()
}

fn event_count(repo: &Path, event_type: &str) -> i64 {
    let conn = rusqlite::Connection::open(db_path(repo)).unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM events WHERE type = ?1",
        [event_type],
        |row| row.get(0),
    )
    .unwrap()
}

fn tombstone_row_ids(repo: &Path, table: &str) -> Vec<String> {
    let conn = rusqlite::Connection::open(db_path(repo)).unwrap();
    let mut stmt = conn
        .prepare("SELECT row_id FROM tombstones WHERE table_name = ?1 ORDER BY row_id")
        .unwrap();
    let rows = stmt
        .query_map([table], |row| row.get::<_, String>(0))
        .unwrap();
    rows.map(|row| row.unwrap()).collect()
}

fn write_conflicts(repo: &Path, conflicts: &serde_json::Value) {
    let dirs = merge_session_dirs(repo);
    let path = state_dir(repo)
        .join("merges")
        .join(&dirs[0])
        .join("conflicts.json");
    std::fs::write(&path, serde_json::to_string_pretty(conflicts).unwrap()).unwrap();
}

fn conflicts_path(repo: &Path) -> PathBuf {
    let dirs = merge_session_dirs(repo);
    state_dir(repo)
        .join("merges")
        .join(&dirs[0])
        .join("conflicts.json")
}

fn create_team(repo: &Path, bin: &Path, name: &str) -> String {
    let output = run(repo, bin, &["team", "create", "--name", name, "--json"]);
    assert!(output.status.success(), "team create failed: {output:?}");
    json(&output)["data"]["team"]["id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Two clones each add a team named `core` (distinct ULIDs) so the merge sees
/// a `unique_key` collision. Returns the pair and the staged conflict facts.
struct UniqueKey {
    pair: Pair,
    merge_id: String,
    conflict_id: String,
    ours_id: String,
    theirs_id: String,
}

fn stage_unique_key(name: &str) -> UniqueKey {
    let pair = seed_pair(name);
    create_team(&pair.src, &pair.bin, "core");
    create_team(&pair.tgt, &pair.bin, "core");
    let bundle = pair.src.join("bundle-uk");
    export_bundle(&pair.src, &pair.bin, &bundle);
    let staged = run(
        &pair.tgt,
        &pair.bin,
        &[
            "import",
            bundle.to_str().unwrap(),
            "--mode",
            "merge",
            "--json",
        ],
    );
    assert_eq!(
        staged.status.code(),
        Some(3),
        "unique_key staging: {staged:?}"
    );
    let merge_id = json(&staged)["error"]["details"]["mergeId"]
        .as_str()
        .unwrap()
        .to_string();
    let conflicts = conflicts_json(&pair.tgt);
    let conflict = conflicts
        .as_array()
        .unwrap()
        .iter()
        .find(|conflict| conflict["kind"] == "unique_key")
        .expect("unique_key conflict staged");
    assert_eq!(conflict["table"], "teams");
    UniqueKey {
        pair,
        merge_id,
        conflict_id: conflict["id"].as_str().unwrap().to_string(),
        ours_id: conflict["ours"]["id"].as_str().unwrap().to_string(),
        theirs_id: conflict["theirs"]["id"].as_str().unwrap().to_string(),
    }
}

#[test]
fn unique_key_resolve_theirs_applies_chosen_and_tombstones_loser() {
    let uk = stage_unique_key("uk_theirs");
    let resolved = run(
        &uk.pair.tgt,
        &uk.pair.bin,
        &["conflict", "resolve", &uk.conflict_id, "--theirs", "--json"],
    );
    assert!(resolved.status.success(), "resolve failed: {resolved:?}");
    let listed = run(&uk.pair.tgt, &uk.pair.bin, &["conflict", "list", "--json"]);
    assert_eq!(json(&listed)["data"]["mergeId"], uk.merge_id);
    let applied = run(&uk.pair.tgt, &uk.pair.bin, &["conflict", "apply", "--json"]);
    assert!(applied.status.success(), "apply failed: {applied:?}");

    let teams = team_ids(&uk.pair.tgt);
    assert_eq!(teams, vec![uk.theirs_id.clone()], "exactly the chosen team");
    assert_eq!(
        tombstone_row_ids(&uk.pair.tgt, "teams"),
        vec![uk.ours_id.clone()]
    );
    assert_eq!(event_count(&uk.pair.tgt, "project.merged"), 1);
    assert!(merge_session_dirs(&uk.pair.tgt).is_empty());
}

#[test]
fn unique_key_resolve_ours_applies_chosen_and_tombstones_loser() {
    let uk = stage_unique_key("uk_ours");
    let resolved = run(
        &uk.pair.tgt,
        &uk.pair.bin,
        &["conflict", "resolve", &uk.conflict_id, "--ours", "--json"],
    );
    assert!(resolved.status.success(), "resolve failed: {resolved:?}");
    let applied = run(&uk.pair.tgt, &uk.pair.bin, &["conflict", "apply", "--json"]);
    assert!(applied.status.success(), "apply failed: {applied:?}");

    let teams = team_ids(&uk.pair.tgt);
    assert_eq!(teams, vec![uk.ours_id.clone()], "exactly the chosen team");
    assert_eq!(
        tombstone_row_ids(&uk.pair.tgt, "teams"),
        vec![uk.theirs_id.clone()]
    );
}

#[test]
fn unique_key_skip_open_genuinely_settles_at_ours() {
    let uk = stage_unique_key("uk_skip_open");
    let applied = run(
        &uk.pair.tgt,
        &uk.pair.bin,
        &["conflict", "apply", "--skip-open", "--json"],
    );
    assert!(applied.status.success(), "skip-open apply: {applied:?}");
    assert_eq!(
        team_ids(&uk.pair.tgt),
        vec![uk.ours_id.clone()],
        "skip-open must keep the local (ours) row, not the merge survivor"
    );
    assert_eq!(
        tombstone_row_ids(&uk.pair.tgt, "teams"),
        vec![uk.theirs_id.clone()]
    );
}

/// Stage a `delete_vs_edit` for one task, optionally attaching a `scopes` and a
/// `progress_items` child to the target so the cascade is exercised.
fn stage_task_delete_with_children(
    name: &str,
    with_scope: bool,
    with_progress: bool,
) -> (Pair, String, String) {
    let p = seed_pair(name);
    const DOOMED: &str = "01DOOMEDCHILD00000000000001";
    insert_task_direct(&p.src, DOOMED, "CTX-9100", "doomed children");
    insert_task_direct(&p.tgt, DOOMED, "CTX-9100", "doomed children");
    if with_scope || with_progress {
        let conn = rusqlite::Connection::open(db_path(&p.tgt)).unwrap();
        let project_id = project_id(&p.tgt);
        if with_scope {
            conn.execute(
                "INSERT INTO scopes (id, project_id, task_id, pattern, kind, created_at)
                 VALUES ('01SCOPECHILD00000000000001', ?1, ?2, 'src/**', 'include', '2026-01-01T00:00:00Z')",
                rusqlite::params![project_id, DOOMED],
            )
            .unwrap();
        }
        if with_progress {
            conn.execute(
                "INSERT INTO progress_items (id, project_id, display_id, task_id, type, status, content, position, created_at, updated_at)
                 VALUES ('01PROGCHILD00000000000000001', ?1, 'PX-9100', ?2, 'todo', 'open', 'child work', 0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
                rusqlite::params![project_id, DOOMED],
            )
            .unwrap();
        }
    }
    delete_task_with_tombstone(&p.src, DOOMED);

    let bundle = p.src.join("bundle-children");
    export_bundle(&p.src, &p.bin, &bundle);
    let staged = run(
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
    assert_eq!(staged.status.code(), Some(3), "staging: {staged:?}");
    let merge_id = json(&staged)["error"]["details"]["mergeId"]
        .as_str()
        .unwrap()
        .to_string();
    let conflict_id = first_conflict_id(&p.tgt, "delete_vs_edit");
    (p, merge_id, conflict_id)
}

#[test]
fn task_delete_resolution_cascades_scopes_and_tombstones_them() {
    let (p, _merge_id, conflict_id) =
        stage_task_delete_with_children("delete_scope_child", true, false);
    let resolved = run(
        &p.tgt,
        &p.bin,
        &["conflict", "resolve", &conflict_id, "--theirs", "--json"],
    );
    assert!(resolved.status.success(), "resolve failed: {resolved:?}");
    let applied = run(&p.tgt, &p.bin, &["conflict", "apply", "--json"]);
    assert!(
        applied.status.success(),
        "apply with a scopes child must not fail closed: {applied:?}"
    );
    assert_eq!(
        tombstone_count(&p.tgt, "tasks", "01DOOMEDCHILD00000000000001"),
        1
    );
    assert_eq!(
        tombstone_row_ids(&p.tgt, "scopes"),
        vec!["01SCOPECHILD00000000000001".to_string()]
    );
    assert_eq!(event_count(&p.tgt, "project.merged"), 1);
}

#[test]
fn task_delete_resolution_tombstones_progress_child() {
    let (p, _merge_id, conflict_id) =
        stage_task_delete_with_children("delete_progress_child", false, true);
    let resolved = run(
        &p.tgt,
        &p.bin,
        &["conflict", "resolve", &conflict_id, "--theirs", "--json"],
    );
    assert!(resolved.status.success(), "resolve failed: {resolved:?}");
    let applied = run(&p.tgt, &p.bin, &["conflict", "apply", "--json"]);
    assert!(applied.status.success(), "apply failed: {applied:?}");
    assert_eq!(
        tombstone_row_ids(&p.tgt, "progress_items"),
        vec!["01PROGCHILD00000000000000001".to_string()],
        "cascade-deleted progress rows must be tombstoned (design §1.3 rule 1)"
    );
}

#[test]
fn resolve_dry_run_leaves_conflicts_json_byte_identical() {
    let (p, _) = stage_strict_conflict("resolve_dry_run");
    let conflict_id = first_conflict_id(&p.tgt, "row_edit");
    let before = std::fs::read(conflicts_path(&p.tgt)).unwrap();

    let output = run(
        &p.tgt,
        &p.bin,
        &[
            "conflict",
            "resolve",
            &conflict_id,
            "--theirs",
            "--dry-run",
            "--json",
        ],
    );
    assert!(output.status.success(), "dry-run resolve: {output:?}");
    let data = &json(&output)["data"];
    assert_eq!(data["operation"]["applied"], false);
    assert_eq!(
        std::fs::read(conflicts_path(&p.tgt)).unwrap(),
        before,
        "dry-run resolve must not write"
    );
}

#[test]
fn abort_dry_run_leaves_staging_intact() {
    let (p, merge_id) = stage_strict_conflict("abort_dry_run");
    let conflicts_before = std::fs::read(conflicts_path(&p.tgt)).unwrap();

    let output = run(
        &p.tgt,
        &p.bin,
        &["conflict", "abort", "--dry-run", "--json"],
    );
    assert!(output.status.success(), "dry-run abort: {output:?}");
    let data = &json(&output)["data"];
    assert_eq!(data["aborted"], false);
    assert_eq!(data["operation"]["applied"], false);
    assert_eq!(merge_session_dirs(&p.tgt), vec![merge_id.clone()]);
    assert_eq!(
        std::fs::read(conflicts_path(&p.tgt)).unwrap(),
        conflicts_before
    );
}

#[test]
fn apply_retry_after_mid_apply_failure_appends_exactly_one_merged() {
    let (p, merge_id) = stage_strict_conflict("apply_retry");
    let conflict_id = first_conflict_id(&p.tgt, "row_edit");
    let resolved = run(
        &p.tgt,
        &p.bin,
        &["conflict", "resolve", &conflict_id, "--theirs", "--json"],
    );
    assert!(resolved.status.success(), "resolve failed: {resolved:?}");

    let session = state_dir(&p.tgt).join("merges").join(&merge_id);
    let candidate_before = std::fs::read(session.join("candidate.sqlite")).unwrap();
    let live_before = db_bytes(&p.tgt);

    // A directory where the swap's `original_` hard link goes makes the swap
    // fail AFTER the candidate was built and events appended to the throwaway
    // copy — i.e. a mid-apply failure.
    let injected = state_dir(&p.tgt).join(format!("state.sqlite.original_{merge_id}"));
    std::fs::create_dir_all(&injected).unwrap();

    let failed = run(&p.tgt, &p.bin, &["conflict", "apply", "--json"]);
    assert!(
        !failed.status.success(),
        "injected apply must fail: {failed:?}"
    );
    assert_eq!(db_bytes(&p.tgt), live_before, "live DB must be untouched");
    assert_eq!(
        std::fs::read(session.join("candidate.sqlite")).unwrap(),
        candidate_before,
        "staged candidate must be untouched (fresh copy per attempt)"
    );
    assert_eq!(event_count(&p.tgt, "project.merged"), 0);
    assert_eq!(
        merge_session_dirs(&p.tgt).len(),
        1,
        "session stays retryable"
    );
    assert_eq!(
        journal_count(&p.tgt),
        0,
        "a failed swap must not leave a journal"
    );

    std::fs::remove_dir(&injected).unwrap();
    let retried = run(&p.tgt, &p.bin, &["conflict", "apply", "--json"]);
    assert!(retried.status.success(), "retry failed: {retried:?}");
    assert_eq!(
        event_count(&p.tgt, "project.merged"),
        1,
        "retry must not double-append events"
    );
    assert_eq!(
        task_title(&p.tgt, &p.base_task).as_deref(),
        Some("source edit")
    );
    assert!(merge_session_dirs(&p.tgt).is_empty());
}

#[test]
fn apply_refuses_a_session_already_marked_applied() {
    let (p, merge_id) = stage_strict_conflict("apply_applied_status");
    let merge_json_path = state_dir(&p.tgt)
        .join("merges")
        .join(&merge_id)
        .join("merge.json");
    let mut merge_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&merge_json_path).unwrap()).unwrap();
    merge_json["status"] = serde_json::json!("applied");
    std::fs::write(
        &merge_json_path,
        serde_json::to_string_pretty(&merge_json).unwrap(),
    )
    .unwrap();

    let applied = run(&p.tgt, &p.bin, &["conflict", "apply", "--json"]);
    assert_eq!(applied.status.code(), Some(7), "re-apply: {applied:?}");
    assert_eq!(json(&applied)["error"]["code"], "RESOURCE_NOT_FOUND");

    let listed = run(
        &p.tgt,
        &p.bin,
        &["conflict", "list", "--merge", &merge_id, "--json"],
    );
    assert_eq!(
        listed.status.code(),
        Some(7),
        "--merge respects status: {listed:?}"
    );

    let aborted = run(&p.tgt, &p.bin, &["conflict", "abort", "--json"]);
    assert_eq!(
        aborted.status.code(),
        Some(7),
        "abort respects status: {aborted:?}"
    );
}

#[test]
fn resolve_set_on_a_deletion_side_is_exit_8() {
    let (p, _merge_id, conflict_id) = stage_task_delete_with_children("set_on_delete", true, false);
    let output = run(
        &p.tgt,
        &p.bin,
        &[
            "conflict",
            "resolve",
            &conflict_id,
            "--theirs",
            "--set",
            "title=nope",
            "--json",
        ],
    );
    assert_eq!(output.status.code(), Some(8), "--set on delete: {output:?}");
    assert_eq!(json(&output)["error"]["code"], "VALIDATION_FAILED");
}

#[test]
fn merge_selection_on_mutating_commands() {
    let (p, merge_id) = stage_strict_conflict("merge_selection");
    let conflict_id = first_conflict_id(&p.tgt, "row_edit");

    let bogus = run(
        &p.tgt,
        &p.bin,
        &[
            "conflict",
            "resolve",
            &conflict_id,
            "--ours",
            "--merge",
            "01BOGUSMERGE0000000000000",
            "--json",
        ],
    );
    assert_eq!(bogus.status.code(), Some(7), "bogus --merge: {bogus:?}");

    let resolved = run(
        &p.tgt,
        &p.bin,
        &[
            "conflict",
            "resolve",
            &conflict_id,
            "--ours",
            "--merge",
            &merge_id,
            "--json",
        ],
    );
    assert!(resolved.status.success(), "--merge resolve: {resolved:?}");
    let applied = run(
        &p.tgt,
        &p.bin,
        &["conflict", "apply", "--merge", &merge_id, "--json"],
    );
    assert!(applied.status.success(), "--merge apply: {applied:?}");
}

#[test]
fn tampered_conflict_table_fails_closed() {
    let (p, _) = stage_strict_conflict("tampered_table");
    let conflict_id = first_conflict_id(&p.tgt, "row_edit");
    let resolved = run(
        &p.tgt,
        &p.bin,
        &["conflict", "resolve", &conflict_id, "--ours", "--json"],
    );
    assert!(resolved.status.success(), "resolve failed: {resolved:?}");

    let mut conflicts = conflicts_json(&p.tgt);
    conflicts[0]["table"] = serde_json::json!("tasks; DROP TABLE tasks; --");
    write_conflicts(&p.tgt, &conflicts);

    let before = db_bytes(&p.tgt);
    let output = run(&p.tgt, &p.bin, &["conflict", "apply", "--json"]);
    assert_eq!(output.status.code(), Some(8), "tampered table: {output:?}");
    assert_eq!(json(&output)["error"]["code"], "VALIDATION_FAILED");
    assert_eq!(db_bytes(&p.tgt), before, "live DB must be untouched");
    assert!(
        task_title(&p.tgt, &p.base_task).is_some(),
        "the projects/tasks tables must survive"
    );
}

#[test]
fn incomplete_session_directory_does_not_panic() {
    let (repo, bin) = common::setup_test_project("incomplete_session");
    init(&repo, &bin);
    let merges = state_dir(&repo).join("merges");
    let partial = merges.join("01PARTIALSESSION0000000000");
    std::fs::create_dir_all(&partial).unwrap();
    std::fs::write(partial.join("merge.json"), r#"{"mergeId":"01PARTIAL"}"#).unwrap();

    let listed = run(&repo, &bin, &["conflict", "list", "--json"]);
    assert_eq!(listed.status.code(), Some(7), "partial session: {listed:?}");

    let selected = run(
        &repo,
        &bin,
        &[
            "conflict",
            "list",
            "--merge",
            "01PARTIALSESSION0000000000",
            "--json",
        ],
    );
    assert_eq!(
        selected.status.code(),
        Some(7),
        "partial --merge: {selected:?}"
    );

    // A malformed marker is a typed error, never a panic.
    let broken = merges.join("01BROKENSESSION0000000000");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(broken.join("merge.json"), "not json").unwrap();
    std::fs::write(broken.join("conflicts.json"), "[]").unwrap();
    let malformed = run(&repo, &bin, &["conflict", "list", "--json"]);
    assert_eq!(malformed.status.code(), Some(5), "malformed: {malformed:?}");
    assert_eq!(json(&malformed)["error"]["code"], "DATABASE_ERROR");
}

/// CTX-0151: `conflict apply` is a direct-lock command, so `--session`
/// bypasses the pre-dispatch canonicalization. A unique short prefix must
/// still resolve to the full ULID before it reaches `events.session_id`.
#[test]
fn conflict_apply_canonicalizes_unique_short_session_ref() {
    let (p, _merge_id) = stage_strict_conflict("apply_session_ref");

    let started = run(
        &p.tgt,
        &p.bin,
        &["--json", "session", "start", "--task", &p.base_task],
    );
    assert!(
        started.status.success(),
        "session start failed: {started:?}"
    );
    let session = json(&started)["data"]["id"].as_str().unwrap().to_string();
    let short = &session[..8];

    let applied = run(
        &p.tgt,
        &p.bin,
        &[
            "--json",
            "--session",
            short,
            "conflict",
            "apply",
            "--skip-open",
        ],
    );
    assert!(applied.status.success(), "apply failed: {applied:?}");
    assert_eq!(
        event_session_ids(&p.tgt, "project.merged"),
        vec![Some(session)],
        "apply events must persist the canonical full session ULID"
    );
}

/// CTX-0151: an unknown `--session` ref on `conflict apply` must fail closed
/// (`RESOURCE_NOT_FOUND`, exit 7) and leave the staged session and live
/// database untouched.
#[test]
fn conflict_apply_unknown_session_ref_fails_closed() {
    let (p, _merge_id) = stage_strict_conflict("apply_session_ref_unknown");
    let before = db_bytes(&p.tgt);

    let applied = run(
        &p.tgt,
        &p.bin,
        &[
            "--json",
            "--session",
            "ZZZZZZZZ",
            "conflict",
            "apply",
            "--skip-open",
        ],
    );
    assert_eq!(applied.status.code(), Some(7), "unknown ref: {applied:?}");
    assert_eq!(json(&applied)["error"]["code"], "RESOURCE_NOT_FOUND");
    assert_eq!(db_bytes(&p.tgt), before, "live DB must stay untouched");
    assert_eq!(
        merge_session_dirs(&p.tgt).len(),
        1,
        "the staged session must survive a failed apply"
    );
}
