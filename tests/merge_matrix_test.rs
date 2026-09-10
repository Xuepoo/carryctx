//! CTX-0146 merge milestone matrix: consolidated CLI-boundary coverage for the
//! acceptance criteria and conflict taxonomy in
//! `design/2026-09-10-mergeable-git-managed-state.md` §5 (AC1–AC9) and §6.
//!
//! The suite is additive: it targets the genuine gaps left by the CTX-0142/
//! 0143/0144/0145 suites (see the per-test `AC`/`§` citations) without
//! rewriting any passing test. Every fixture is a disposable Git repository
//! under the system temp directory and touches no network.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

// ── generic helpers ──────────────────────────────────────────────────────

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

fn run(repo: &Path, bin: &Path, args: &[&str]) -> Output {
    common::run_cmd(repo, bin, args)
}

fn ok(repo: &Path, bin: &Path, args: &[&str]) -> Output {
    let output = run(repo, bin, args);
    assert!(
        output.status.success(),
        "carryctx {args:?} failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn git_out(repo: &Path, args: &[&str]) -> Output {
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
    command.args(args).current_dir(repo);
    for var in GIT_STATE_VARS {
        command.env_remove(var);
    }
    command.output().expect("git should spawn")
}

fn git_ok(repo: &Path, args: &[&str]) -> String {
    let output = git_out(repo, args);
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn db_path(repo: &Path) -> PathBuf {
    repo.join(".git/carryctx/state.sqlite")
}

fn state_dir(repo: &Path) -> PathBuf {
    repo.join(".git/carryctx")
}

fn db_bytes(repo: &Path) -> Vec<u8> {
    std::fs::read(db_path(repo)).unwrap()
}

fn open_db(repo: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open(db_path(repo)).unwrap()
}

fn project_id(repo: &Path) -> String {
    open_db(repo)
        .query_row("SELECT id FROM projects LIMIT 1", [], |row| row.get(0))
        .unwrap()
}

fn task_id_by_title(repo: &Path, title: &str) -> String {
    open_db(repo)
        .query_row("SELECT id FROM tasks WHERE title = ?1", [title], |row| {
            row.get(0)
        })
        .unwrap()
}

fn task_title(repo: &Path, id: &str) -> Option<String> {
    open_db(repo)
        .query_row("SELECT title FROM tasks WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .ok()
}

fn display_id(repo: &Path, task_id: &str) -> String {
    open_db(repo)
        .query_row(
            "SELECT display_id FROM tasks WHERE id = ?1",
            [task_id],
            |row| row.get(0),
        )
        .unwrap()
}

fn event_payloads(repo: &Path, event_type: &str) -> Vec<serde_json::Value> {
    let conn = open_db(repo);
    let mut stmt = conn
        .prepare("SELECT payload_json FROM events WHERE type = ?1 ORDER BY occurred_at")
        .unwrap();
    let rows = stmt
        .query_map([event_type], |row| row.get::<_, String>(0))
        .unwrap();
    rows.map(|row| serde_json::from_str(&row.unwrap()).unwrap())
        .collect()
}

/// All `(id, type, payload_json)` event rows, for union/no-mutation checks.
fn event_rows(repo: &Path) -> Vec<(String, String, String)> {
    let conn = open_db(repo);
    let mut stmt = conn
        .prepare("SELECT id, type, payload_json FROM events ORDER BY id")
        .unwrap();
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .unwrap();
    rows.map(|row| row.unwrap()).collect()
}

fn sequence_kind(repo: &Path) -> String {
    let prefix: String = open_db(repo)
        .query_row("SELECT task_prefix FROM projects LIMIT 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    format!("display_id_{prefix}")
}

fn sequence_next_value(repo: &Path, kind: &str) -> i64 {
    open_db(repo)
        .query_row(
            "SELECT next_value FROM sequences WHERE kind = ?1",
            [kind],
            |row| row.get(0),
        )
        .unwrap()
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

fn conflict_kinds(repo: &Path) -> Vec<String> {
    let dirs = merge_session_dirs(repo);
    assert_eq!(dirs.len(), 1, "expected exactly one staged session");
    let raw = std::fs::read_to_string(
        state_dir(repo)
            .join("merges")
            .join(&dirs[0])
            .join("conflicts.json"),
    )
    .unwrap();
    let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|conflict| conflict["kind"].as_str().unwrap().to_string())
        .collect()
}

fn journal_count(repo: &Path) -> usize {
    std::fs::read_dir(state_dir(repo).join("journals"))
        .map(|entries| entries.filter_map(Result::ok).count())
        .unwrap_or(0)
}

// ── bundle helpers ───────────────────────────────────────────────────────

fn export_bundle(repo: &Path, bin: &Path, out: &Path) {
    ok(
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
}

fn export_snapshot(repo: &Path, bin: &Path, out: &Path) -> serde_json::Value {
    let output = ok(
        repo,
        bin,
        &[
            "export",
            "--pack-format",
            "dir",
            "-o",
            out.to_str().unwrap(),
            "--snapshot",
            "--json",
        ],
    );
    json(&output)["data"].clone()
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

/// Read tombstones from an exported bundle as `(table_name, row_id)` pairs.
fn bundle_tombstones(bundle: &Path) -> Vec<(String, String)> {
    let raw = std::fs::read_to_string(bundle.join("tombstones.jsonl")).unwrap_or_default();
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let row: serde_json::Value = serde_json::from_str(line).unwrap();
            (
                row["table_name"].as_str().unwrap().to_string(),
                row["row_id"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

// ── pair fixture ─────────────────────────────────────────────────────────

struct Pair {
    src: PathBuf,
    tgt: PathBuf,
    bin: PathBuf,
    base_bundle: PathBuf,
    base_task: String,
}

/// Two clones sharing one project identity, both holding "base task". The
/// source's first export is the explicit base for three-way merges.
fn seed_pair(name: &str) -> Pair {
    let (src, bin) = common::setup_test_project(&format!("{name}_src"));
    common::run_cmd(&src, &bin, &["init", "--force"]);
    ok(
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
    ok(&src, &bin, &["task", "create", "--title", "base task"]);
    let base_task = task_id_by_title(&src, "base task");

    let base_bundle = src.join("bundle-base");
    export_bundle(&src, &bin, &base_bundle);

    let (tgt, _) = common::setup_test_project(&format!("{name}_tgt"));
    ok(
        &tgt,
        &bin,
        &["import", base_bundle.to_str().unwrap(), "--json"],
    );
    assert_eq!(project_id(&src), project_id(&tgt));

    Pair {
        src,
        tgt,
        bin,
        base_bundle,
        base_task,
    }
}

fn create_task(repo: &Path, bin: &Path, title: &str) -> String {
    ok(repo, bin, &["task", "create", "--title", title]);
    task_id_by_title(repo, title)
}

// ── direct-SQL seed helpers ──────────────────────────────────────────────

fn set_task_status(repo: &Path, task_id: &str, status: &str) {
    open_db(repo)
        .execute(
            "UPDATE tasks SET status = ?1, updated_at = '2030-01-01T00:00:00Z' WHERE id = ?2",
            rusqlite::params![status, task_id],
        )
        .unwrap();
}

fn insert_task_direct(repo: &Path, task_id: &str, display: &str, title: &str) {
    let project = project_id(repo);
    open_db(repo)
        .execute(
            "INSERT INTO tasks (id, project_id, display_id, title, status, priority, metadata_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'planned', 'normal', '{}', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            rusqlite::params![task_id, project, display, title],
        )
        .unwrap();
}

fn insert_agent_direct(repo: &Path, agent: &str, name: &str) {
    let project = project_id(repo);
    open_db(repo)
        .execute(
            "INSERT INTO agents (id, project_id, name, provider, status, metadata_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'test', 'active', '{}', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            rusqlite::params![agent, project, name],
        )
        .unwrap();
}

/// Seed a team plus one membership directly, so the same team id can exist on
/// two clones. `commander` promotes the member after the row exists to satisfy
/// the `teams -> team_members` composite FK.
fn seed_shared_team(
    repo: &Path,
    team: &str,
    name: &str,
    member: &str,
    role: &str,
    updated_at: &str,
    commander: bool,
) {
    let project = project_id(repo);
    let conn = open_db(repo);
    conn.execute(
        "INSERT INTO teams (id, project_id, name, commander_agent_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, NULL, '2026-01-01T00:00:00Z', ?4)",
        rusqlite::params![team, project, name, updated_at],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO team_members (project_id, team_id, agent_id, role, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, '2026-01-01T00:00:00Z', ?5)",
        rusqlite::params![project, team, member, role, updated_at],
    )
    .unwrap();
    if commander {
        conn.execute(
            "UPDATE teams SET commander_agent_id = ?1 WHERE id = ?2",
            rusqlite::params![member, team],
        )
        .unwrap();
    }
}

// ═════════════════════════════════════════════════════════════════════════
// AC1 — v1/v2 format compatibility
// ═════════════════════════════════════════════════════════════════════════

/// AC1 / §1.8: a v2 `export --snapshot` round-trips `parents`, `sequences`,
/// and tombstones. v1 fresh + replace compatibility and the v1→v2 degraded
/// merge are already covered by `import_test::*` and
/// `import_merge_test::merge_v1_bundle_degrades_with_warning`.
#[test]
fn ac1_v2_snapshot_round_trips_parents_sequences_and_tombstones() {
    let (repo, bin) = common::setup_test_project("matrix_v2_roundtrip");
    common::run_cmd(&repo, &bin, &["init", "--force"]);
    ok(
        &repo,
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
    ok(&repo, &bin, &["task", "create", "--title", "keep"]);
    ok(&repo, &bin, &["task", "create", "--title", "doomed"]);
    // A scope add/remove pair leaves a tombstone in the exported bundle.
    ok(&repo, &bin, &["task", "scope", "add", "CTX-0002", "src/**"]);
    ok(
        &repo,
        &bin,
        &["task", "scope", "remove", "CTX-0002", "src/**"],
    );

    let first = export_snapshot(&repo, &bin, &repo.join("snap-1"));
    let first_export = first["manifest"]["export_id"].as_str().unwrap().to_string();
    let second = export_snapshot(&repo, &bin, &repo.join("snap-2"));

    let manifest = read_manifest(&repo.join("snap-2"));
    assert_eq!(manifest["format_version"], serde_json::json!(2));
    assert_eq!(
        manifest["parents"],
        serde_json::json!([first_export]),
        "second snapshot's parent is the first export id"
    );
    let sequences = manifest["sequences"].as_object().unwrap();
    assert!(
        sequences
            .values()
            .any(|value| value.as_u64().unwrap_or(0) >= 1),
        "sequences must round-trip a non-zero floor: {sequences:?}"
    );
    assert!(
        manifest["counts"]["tombstones"].as_u64().unwrap_or(0) >= 1,
        "tombstone count must be present and non-zero: {manifest}"
    );
    let tombstones = bundle_tombstones(&repo.join("snap-2"));
    assert!(
        tombstones.iter().any(|(table, _)| table == "scopes"),
        "removed scope must be exported as a tombstone: {tombstones:?}"
    );

    // The same bundle imports fresh and is recorded as format v2.
    let (fresh, _) = common::setup_test_project("matrix_v2_roundtrip_target");
    ok(
        &fresh,
        &bin,
        &["import", repo.join("snap-2").to_str().unwrap(), "--json"],
    );
    let payloads = event_payloads(&fresh, "project.imported");
    assert!(
        payloads
            .iter()
            .any(|payload| payload["formatVersion"] == serde_json::json!(2)),
        "import must record format v2: {payloads:?}"
    );
    let _ = second;
}

// ═════════════════════════════════════════════════════════════════════════
// AC5 — deletes and tombstones
// ═════════════════════════════════════════════════════════════════════════

/// AC5 / §1.3 rule 1: delete-then-export records a tombstone for the task and
/// every row removed by cascade (progress item, scope, dependency edge).
/// The three-way delete policies are covered by
/// `import_merge_test::merge_applies_three_way_delete_from_tombstones`,
/// `merge_delete_vs_edit_*`, and `merge_both_delete_keeps_earliest_deleted_at`.
#[test]
fn ac5_delete_then_export_tombstones_cascaded_children() {
    let (repo, bin) = common::setup_test_project("matrix_delete_export");
    common::run_cmd(&repo, &bin, &["init", "--force"]);
    ok(
        &repo,
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
    ok(&repo, &bin, &["task", "create", "--title", "victim"]);
    ok(&repo, &bin, &["task", "create", "--title", "keeper"]);
    ok(
        &repo,
        &bin,
        &["progress", "note", "child progress", "--task", "CTX-0001"],
    );
    ok(&repo, &bin, &["task", "scope", "add", "CTX-0001", "src/**"]);
    ok(
        &repo,
        &bin,
        &["task", "depend", "CTX-0002", "--on", "CTX-0001"],
    );

    let victim = task_id_by_title(&repo, "victim");
    let conn = open_db(&repo);
    let progress_id: String = conn
        .query_row("SELECT id FROM progress_items", [], |row| row.get(0))
        .unwrap();
    let scope_id: String = conn
        .query_row("SELECT id FROM scopes", [], |row| row.get(0))
        .unwrap();
    let dependency_id: String = conn
        .query_row("SELECT id FROM task_dependencies", [], |row| row.get(0))
        .unwrap();
    conn.execute(
        "UPDATE tasks SET status = 'completed', updated_at = '2020-01-01T00:00:00Z' WHERE id = ?1",
        [&victim],
    )
    .unwrap();
    drop(conn);

    ok(
        &repo,
        &bin,
        &["project", "prune", "--older-than-days", "30", "--json"],
    );

    let bundle = repo.join("bundle-pruned");
    export_bundle(&repo, &bin, &bundle);
    let tombstones = bundle_tombstones(&bundle);
    for (table, row_id) in [
        ("tasks", victim.as_str()),
        ("progress_items", progress_id.as_str()),
        ("scopes", scope_id.as_str()),
        ("task_dependencies", dependency_id.as_str()),
    ] {
        assert!(
            tombstones.iter().any(|(t, id)| t == table && id == row_id),
            "missing tombstone for {table}/{row_id}: {tombstones:?}"
        );
    }
    assert!(task_title(&repo, &victim).is_none(), "victim must be gone");
}

// ═════════════════════════════════════════════════════════════════════════
// AC6 — identity: renumbering, aliasing, sequences
// ═════════════════════════════════════════════════════════════════════════

/// AC6 / §1.1: a dependency that points at a renumbered task's ULID still
/// resolves after the incoming row is renumbered, and the sequence floor
/// moves past the renumbered display id. The basic renumber + floor case is
/// covered by `import_merge_test::merge_display_id_collision_renumbers_*`.
#[test]
fn ac6_renumber_keeps_dependency_edge_and_sequence_floor() {
    let p = seed_pair("matrix_renumber_dep");
    // Fixed ULIDs make the collision deterministic: the smaller target id is
    // canonical and keeps `CTX-0002`, so the incoming source row is renumbered.
    let tgt_collision = "01AAAAAAAAAAAAAAAAAAAAAAAA";
    let src_collision = "01ZZZZZZZZZZZZZZZZZZZZZZZZ";
    insert_task_direct(&p.tgt, tgt_collision, "CTX-0002", "tgt collision");
    insert_task_direct(&p.src, src_collision, "CTX-0002", "src collision");
    let colliding = "CTX-0002";

    // The edge references ULIDs, so renumbering must not break it.
    ok(
        &p.src,
        &p.bin,
        &["task", "depend", colliding, "--on", "CTX-0001"],
    );

    let bundle = p.src.join("bundle-src");
    export_bundle(&p.src, &p.bin, &bundle);
    let merged = ok(
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
    let body = json(&merged);
    assert_eq!(body["data"]["renumbers"].as_u64(), Some(1));

    let renumbers = event_payloads(&p.tgt, "merge.display_id_renumbered");
    assert_eq!(renumbers.len(), 1, "exactly one renumber audit event");
    assert_eq!(
        renumbers[0]["rowId"].as_str(),
        Some(src_collision),
        "the incoming row is the one renumbered"
    );

    // The target row keeps the contested display id; the incoming row moves.
    assert_eq!(display_id(&p.tgt, tgt_collision), colliding);
    let new_display = display_id(&p.tgt, src_collision);
    assert_ne!(new_display, colliding, "incoming row must be renumbered");

    // The edge still points at the renumbered ULID.
    let conn = open_db(&p.tgt);
    let edge: (String, String) = conn
        .query_row(
            "SELECT task_id, prerequisite_task_id FROM task_dependencies",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        edge.0, src_collision,
        "edge must reference the renumbered ULID"
    );
    assert_eq!(edge.1, p.base_task);
    drop(conn);

    let kind = sequence_kind(&p.tgt);
    let next = sequence_next_value(&p.tgt, &kind);
    assert!(
        next > 2,
        "sequence must never rewind after a renumber: {next}"
    );
}

/// AC6 / §1.7: an agent-name collision aliases the incoming agent into ours
/// and remaps every FK column, not just `tasks.owner_agent_id`: sessions,
/// handoffs, events, and team membership. The single-column `tasks.owner`
/// case and the engine-level `sessions.agent_id` case are already covered by
/// `import_merge_test::merge_agent_name_collision_*` and the pure engine.
#[test]
fn ac6_agent_alias_remaps_references_across_all_tables() {
    let p = seed_pair("matrix_agent_alias");
    // Fixed ULIDs make the survivor deterministic (`min`): the target's id is
    // smaller, so the incoming (source) agent is the one aliased away.
    let survivor = "01AAAAAAAAAAAAAAAAAAAAAAAA";
    let incoming = "01ZZZZZZZZZZZZZZZZZZZZZZZZ";
    assert_eq!(survivor.len(), 26);
    assert_eq!(incoming.len(), 26);
    insert_agent_direct(&p.src, incoming, "dup");
    insert_agent_direct(&p.tgt, survivor, "dup");

    let project = project_id(&p.src);
    let owned = create_task(&p.src, &p.bin, "owned by dup");
    let conn = open_db(&p.src);
    conn.execute(
        "UPDATE tasks SET owner_agent_id = ?1 WHERE id = ?2",
        rusqlite::params![incoming, owned],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sessions (id, project_id, agent_id, task_id, worktree_id, state, provider, working_directory, metadata_json, started_at, last_activity_at, updated_at)
         VALUES ('01ALIASSESSION00000000001', ?1, ?2, NULL, NULL, 'active', 'test', '/x', '{}', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        rusqlite::params![project, incoming],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO handoffs (id, project_id, from_agent_id, to_agent_id, task_id, session_id, state, display_id, summary, context_json, created_at, updated_at)
         VALUES ('01ALIASHANDOFF00000000001', ?1, ?2, NULL, ?3, NULL, 'pending', 'HO-0001', 'alias probe', '{}', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        rusqlite::params![project, incoming, owned],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO events (id, project_id, type, aggregate_type, aggregate_id, payload_json, occurred_at, actor_agent_id)
         VALUES ('01ALIASEVENT0000000000001', ?1, 'alias.probe', 'task', ?2, '{\"probe\":true}', '2026-01-01T00:00:00Z', ?3)",
        rusqlite::params![project, owned, incoming],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO teams (id, project_id, name, commander_agent_id, created_at, updated_at)
         VALUES ('TEAM-DUP', ?1, 'dup-team', NULL, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        rusqlite::params![project],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO team_members (project_id, team_id, agent_id, role, created_at, updated_at)
         VALUES (?1, 'TEAM-DUP', ?2, 'dev', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        rusqlite::params![project, incoming],
    )
    .unwrap();
    conn.execute(
        "UPDATE teams SET commander_agent_id = ?1 WHERE id = 'TEAM-DUP'",
        rusqlite::params![incoming],
    )
    .unwrap();
    drop(conn);

    let bundle = p.src.join("bundle-src");
    export_bundle(&p.src, &p.bin, &bundle);
    let merged = ok(
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
    assert_eq!(json(&merged)["data"]["aliases"].as_u64(), Some(1));

    // One surviving `dup`, and every reference column points at it.
    let conn = open_db(&p.tgt);
    let dup_ids: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT id FROM agents WHERE name = 'dup'")
            .unwrap();
        let rows = stmt.query_map([], |row| row.get::<_, String>(0)).unwrap();
        rows.map(|row| row.unwrap()).collect()
    };
    assert_eq!(dup_ids, vec![survivor.to_string()]);
    let owner: Option<String> = conn
        .query_row(
            "SELECT owner_agent_id FROM tasks WHERE id = ?1",
            [&owned],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(owner.as_deref(), Some(survivor));
    let session_agent: String = conn
        .query_row("SELECT agent_id FROM sessions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(session_agent, survivor);
    let handoff_from: String = conn
        .query_row("SELECT from_agent_id FROM handoffs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(handoff_from, survivor);
    let event_actor: String = conn
        .query_row(
            "SELECT actor_agent_id FROM events WHERE id = '01ALIASEVENT0000000000001'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(event_actor, survivor);
    let member_agent: String = conn
        .query_row("SELECT agent_id FROM team_members", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        member_agent, survivor,
        "team_members.agent_id must be remapped or the candidate FK fails"
    );
    let commander: String = conn
        .query_row("SELECT commander_agent_id FROM teams", [], |row| row.get(0))
        .unwrap();
    assert_eq!(commander, survivor);
    drop(conn);
}

/// AC6 / §2.3 MAJOR regression: when the alias survivor and the losing agent
/// are BOTH members of the same team, the `agent_id` remap produces two rows
/// with one `(project_id, team_id, agent_id)` key. Before the dedup this made
/// the candidate insert fail `UNIQUE constraint failed` (`DATABASE_ERROR`,
/// exit 5); now it auto-resolves to one deterministic row.
#[test]
fn ac6_agent_alias_dedups_dual_team_membership() {
    let p = seed_pair("matrix_alias_dedup");
    let survivor = "01AAAAAAAAAAAAAAAAAAAAAAAA";
    let incoming = "01ZZZZZZZZZZZZZZZZZZZZZZZZ";
    insert_agent_direct(&p.src, incoming, "dup");
    insert_agent_direct(&p.tgt, survivor, "dup");
    let team = "01TEAMDEDUP00000000000000";
    seed_shared_team(
        &p.tgt,
        team,
        "dup-team",
        survivor,
        "dev",
        "2026-01-01T00:00:00Z",
        false,
    );
    seed_shared_team(
        &p.src,
        team,
        "dup-team",
        incoming,
        "lead",
        "2026-01-05T00:00:00Z",
        false,
    );

    let bundle = p.src.join("bundle-src");
    export_bundle(&p.src, &p.bin, &bundle);
    let merged = ok(
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
    assert_eq!(json(&merged)["data"]["aliases"].as_u64(), Some(1));

    let conn = open_db(&p.tgt);
    let (count, role): (i64, String) = conn
        .query_row(
            "SELECT COUNT(*), MAX(role) FROM team_members WHERE team_id = ?1 AND agent_id = ?2",
            rusqlite::params![team, survivor],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        count, 1,
        "exactly one deduped membership row for the survivor"
    );
    assert_eq!(role, "lead", "the newer membership wins the dedup");
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM team_members", [], |row| row.get(0))
        .unwrap();
    assert_eq!(total, 1, "the loser's duplicate row is dropped");
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        0,
        "the deduped candidate must satisfy every foreign key"
    );
    drop(conn);
}

/// AC6 / §2.3 MAJOR regression: the duplicate membership can be the commander
/// membership. The dedup keeps the survivor-keyed row so
/// `teams.commander_agent_id -> team_members` stays valid.
#[test]
fn ac6_agent_alias_dedups_commander_membership() {
    let p = seed_pair("matrix_alias_commander");
    let survivor = "01AAAAAAAAAAAAAAAAAAAAAAAA";
    let incoming = "01ZZZZZZZZZZZZZZZZZZZZZZZZ";
    insert_agent_direct(&p.src, incoming, "dup");
    insert_agent_direct(&p.tgt, survivor, "dup");
    let team = "01TEAMDEDUP00000000000000";
    seed_shared_team(
        &p.tgt,
        team,
        "dup-team",
        survivor,
        "dev",
        "2026-01-01T00:00:00Z",
        true,
    );
    seed_shared_team(
        &p.src,
        team,
        "dup-team",
        incoming,
        "dev",
        "2026-01-05T00:00:00Z",
        true,
    );

    let bundle = p.src.join("bundle-src");
    export_bundle(&p.src, &p.bin, &bundle);
    let merged = ok(
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
    assert_eq!(json(&merged)["data"]["aliases"].as_u64(), Some(1));

    let conn = open_db(&p.tgt);
    let commander: String = conn
        .query_row("SELECT commander_agent_id FROM teams", [], |row| row.get(0))
        .unwrap();
    assert_eq!(commander, survivor);
    let memberships: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM team_members WHERE team_id = ?1 AND agent_id = ?2",
            rusqlite::params![team, survivor],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(memberships, 1, "the commander stays a member");
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        0
    );
    drop(conn);
}

// ═════════════════════════════════════════════════════════════════════════
// AC7 — append-only audit
// ═════════════════════════════════════════════════════════════════════════

/// AC7 / §1.4: events merge by union with byte-stable payloads; a merge never
/// updates or deletes an event; exactly one `project.merged` is appended. The
/// single-merged-event count is also asserted by several CTX-0142 tests.
#[test]
fn ac7_events_union_is_byte_stable_and_never_mutated() {
    let p = seed_pair("matrix_events_union");
    let tgt_before = event_rows(&p.tgt);
    create_task(&p.src, &p.bin, "src event task");
    let src_rows = event_rows(&p.src);
    assert!(!src_rows.is_empty());

    let bundle = p.src.join("bundle-src");
    export_bundle(&p.src, &p.bin, &bundle);
    ok(
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

    let after: std::collections::BTreeMap<String, (String, String)> = event_rows(&p.tgt)
        .into_iter()
        .map(|(id, event_type, payload)| (id, (event_type, payload)))
        .collect();

    // Union: every pre-existing target event and every source event survives
    // with a byte-identical payload and type.
    for (id, event_type, payload) in tgt_before.iter().chain(src_rows.iter()) {
        let (after_type, after_payload) = after
            .get(id)
            .unwrap_or_else(|| panic!("event {id} was dropped by the merge"));
        assert_eq!(after_type, event_type, "event {id} type changed");
        assert_eq!(
            after_payload, payload,
            "event {id} payload changed; events are append-only"
        );
    }
    assert_eq!(
        event_payloads(&p.tgt, "project.merged").len(),
        1,
        "exactly one project.merged event"
    );
}

// ═════════════════════════════════════════════════════════════════════════
// AC9 — fail closed
// ═════════════════════════════════════════════════════════════════════════

/// AC9 / §1.8, §2.6: an invalid bundle and a newer-than-reader format each
/// leave the live database and the local snapshot ref untouched. Kill-during-
/// apply recovery is covered by the journal tests in `import_merge_test` and
/// `conflict_test`.
#[test]
fn ac9_invalid_and_newer_bundles_leave_db_and_ref_untouched() {
    let p = seed_pair("matrix_fail_closed");
    create_task(&p.src, &p.bin, "fail-closed edit");

    // Establish a local snapshot ref so "refs untouched" is observable.
    let snap = export_snapshot(&p.tgt, &p.bin, &p.tgt.join("tgt-snap"));
    assert!(snap["snapshot"]["commit"].is_string());
    let ref_before = git_ok(&p.tgt, &["rev-parse", "refs/carryctx/local"]);
    let db_before = db_bytes(&p.tgt);

    let good = p.src.join("bundle-good");
    export_bundle(&p.src, &p.bin, &good);

    // Invalid: one manifest count disagrees with the actual rows.
    let tampered = p.src.join("bundle-tampered");
    std::fs::create_dir_all(&tampered).unwrap();
    for entry in std::fs::read_dir(&good).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            std::fs::copy(entry.path(), tampered.join(entry.file_name())).unwrap();
        }
    }
    let mut manifest = read_manifest(&tampered);
    manifest["counts"]["tasks"] = serde_json::json!(9999);
    write_manifest(&tampered, &manifest);
    let invalid = run(
        &p.tgt,
        &p.bin,
        &[
            "import",
            tampered.to_str().unwrap(),
            "--mode",
            "merge",
            "--snapshot-ref=refs/carryctx/local",
            "--json",
        ],
    );
    assert_eq!(invalid.status.code(), Some(8), "invalid: {invalid:?}");
    assert_eq!(json(&invalid)["error"]["code"], "VALIDATION_FAILED");
    assert_eq!(db_bytes(&p.tgt), db_before);
    assert!(merge_session_dirs(&p.tgt).is_empty());
    assert_eq!(
        git_ok(&p.tgt, &["rev-parse", "refs/carryctx/local"]),
        ref_before
    );

    // Newer bundle format than the reader supports.
    let future = p.src.join("bundle-future");
    std::fs::create_dir_all(&future).unwrap();
    for entry in std::fs::read_dir(&good).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            std::fs::copy(entry.path(), future.join(entry.file_name())).unwrap();
        }
    }
    let mut manifest = read_manifest(&future);
    manifest["format_version"] = serde_json::json!(99);
    write_manifest(&future, &manifest);
    let newer = run(
        &p.tgt,
        &p.bin,
        &[
            "import",
            future.to_str().unwrap(),
            "--mode",
            "merge",
            "--snapshot-ref=refs/carryctx/local",
            "--json",
        ],
    );
    assert_eq!(newer.status.code(), Some(10), "newer: {newer:?}");
    assert_eq!(json(&newer)["error"]["code"], "UNSUPPORTED_OPERATION");
    assert_eq!(db_bytes(&p.tgt), db_before);
    assert!(merge_session_dirs(&p.tgt).is_empty());
    assert_eq!(
        git_ok(&p.tgt, &["rev-parse", "refs/carryctx/local"]),
        ref_before
    );
}

// ═════════════════════════════════════════════════════════════════════════
// §2.3 conflict matrix — CLI-boundary cases not covered elsewhere
// ═════════════════════════════════════════════════════════════════════════

/// §2.3 `status_gap`: completed vs cancelled is a blocking conflict at the
/// CLI boundary (exit 3, envelope, DB untouched). The pure engine case is
/// `carryctx-pack::merge::tests::completed_versus_cancelled_*`.
#[test]
fn matrix_status_gap_blocks_at_cli() {
    let p = seed_pair("matrix_status_gap");
    set_task_status(&p.src, &p.base_task, "completed");
    set_task_status(&p.tgt, &p.base_task, "cancelled");
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
            "--base",
            p.base_bundle.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(conflicted.status.code(), Some(3), "{conflicted:?}");
    assert_eq!(json(&conflicted)["error"]["code"], "MERGE_CONFLICTS");
    assert_eq!(conflict_kinds(&p.tgt), vec!["status_gap"]);
    assert_eq!(db_bytes(&p.tgt), before);
}

/// §2.3 `immutable_edit`: the same immutable row key with differing content on
/// both sides blocks at the CLI boundary.
#[test]
fn matrix_immutable_edit_blocks_at_cli() {
    let p = seed_pair("matrix_immutable_edit");
    let dep = "01MATRIXDEP00000000000001";
    let prereq = "01MATRIXPREREQ00000000001";
    let base_task = p.base_task.clone();
    insert_task_direct(&p.src, prereq, "CTX-9500", "prereq");
    insert_task_direct(&p.tgt, prereq, "CTX-9500", "prereq");
    let insert = |repo: &Path, created_at: &str| {
        open_db(repo)
            .execute(
                "INSERT INTO task_dependencies (id, project_id, task_id, prerequisite_task_id, kind, created_at)
                 VALUES (?1, ?2, ?3, ?4, 'strong', ?5)",
                rusqlite::params![dep, project_id(repo), base_task.as_str(), prereq, created_at],
            )
            .unwrap();
    };
    insert(&p.src, "2026-01-01T00:00:00Z");
    insert(&p.tgt, "2026-06-01T00:00:00Z");

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
            "--base",
            p.base_bundle.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(conflicted.status.code(), Some(3), "{conflicted:?}");
    assert_eq!(json(&conflicted)["error"]["code"], "MERGE_CONFLICTS");
    assert_eq!(conflict_kinds(&p.tgt), vec!["immutable_edit"]);
    assert_eq!(db_bytes(&p.tgt), before);
}

/// §2.3 `dependency_kind`: distinct ULIDs claiming one semantic edge with
/// `strong` vs `informational` auto-resolves to `strong` (exit 0, one audit
/// resolution, `project.merged` appended).
#[test]
fn matrix_dependency_kind_auto_resolves_at_cli() {
    let p = seed_pair("matrix_dependency_kind");
    insert_task_direct(
        &p.src,
        "01MATRIXTASK100000000000001",
        "CTX-9510",
        "dep task",
    );
    insert_task_direct(
        &p.tgt,
        "01MATRIXTASK100000000000001",
        "CTX-9510",
        "dep task",
    );
    // Distinct ULIDs claim one semantic edge with the same frame except `kind`,
    // which the engine auto-resolves to `strong` (design §2.3).
    let base_task = p.base_task.clone();
    let seed = |repo: &Path, id: &str, kind: &str| {
        open_db(repo)
            .execute(
                "INSERT INTO task_dependencies (id, project_id, task_id, prerequisite_task_id, kind, created_at)
                 VALUES (?1, ?2, '01MATRIXTASK100000000000001', ?3, ?4, '2026-01-01T00:00:00Z')",
                rusqlite::params![id, project_id(repo), base_task.as_str(), kind],
            )
            .unwrap();
    };
    seed(&p.src, "01MATRIXDEPINFO00000000001", "informational");
    seed(&p.tgt, "01MATRIXDEPSTRONG000000001", "strong");

    let bundle = p.src.join("bundle-src");
    export_bundle(&p.src, &p.bin, &bundle);
    let merged = ok(
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
    let body = json(&merged);
    assert_eq!(body["data"]["conflicts"].as_u64(), Some(0));
    assert!(body["data"]["autoResolutions"].as_u64().unwrap() >= 1);
    assert_eq!(event_payloads(&p.tgt, "project.merged").len(), 1);

    let rows: Vec<String> = {
        let conn = open_db(&p.tgt);
        let mut stmt = conn
            .prepare(
                "SELECT kind FROM task_dependencies WHERE task_id = '01MATRIXTASK100000000000001'",
            )
            .unwrap();
        let rows = stmt.query_map([], |row| row.get::<_, String>(0)).unwrap();
        rows.map(|row| row.unwrap()).collect()
    };
    assert_eq!(rows, vec!["strong".to_string()], "strong must win the edge");
}

// ═════════════════════════════════════════════════════════════════════════
// §2.4 staging concurrency + dry-run
// ═════════════════════════════════════════════════════════════════════════

/// §2.4/§2.6: a dry run writes nothing in replace and merge mode, and a
/// second concurrent merge refuses while a session is active. The one-active-
/// session refusal is also asserted by
/// `import_merge_test::merge_second_run_refuses_while_session_active`.
#[test]
fn matrix_dry_run_writes_nothing_and_second_merge_refuses() {
    let p = seed_pair("matrix_dry_run");
    create_task(&p.src, &p.bin, "dry run addition");
    ok(
        &p.src,
        &p.bin,
        &["task", "edit", &p.base_task, "--title", "source edit"],
    );
    ok(
        &p.tgt,
        &p.bin,
        &["task", "edit", &p.base_task, "--title", "target edit"],
    );
    let bundle = p.src.join("bundle-src");
    export_bundle(&p.src, &p.bin, &bundle);

    let before = db_bytes(&p.tgt);
    let replace_dry = run(
        &p.tgt,
        &p.bin,
        &[
            "import",
            bundle.to_str().unwrap(),
            "--mode",
            "replace",
            "--yes",
            "--dry-run",
            "--json",
        ],
    );
    assert!(replace_dry.status.success(), "{replace_dry:?}");
    assert_eq!(json(&replace_dry)["data"]["operation"]["applied"], false);
    assert_eq!(db_bytes(&p.tgt), before);
    assert!(merge_session_dirs(&p.tgt).is_empty());
    assert_eq!(journal_count(&p.tgt), 0);

    // A strict merge dry run previews a conflict but stages nothing.
    let merge_dry = run(
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
    assert!(merge_dry.status.success(), "{merge_dry:?}");
    assert_eq!(json(&merge_dry)["data"]["wouldConflict"], true);
    assert_eq!(db_bytes(&p.tgt), before);
    assert!(merge_session_dirs(&p.tgt).is_empty());
    assert_eq!(journal_count(&p.tgt), 0);

    // Staging the conflict now blocks a second merge entry point until the
    // session is resolved or aborted (design §2.4).
    let staged = run(
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
    assert_eq!(staged.status.code(), Some(3), "{staged:?}");
    let second = run(
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
    assert_eq!(second.status.code(), Some(3), "{second:?}");
    assert_eq!(json(&second)["error"]["code"], "STATE_CONFLICT");
}
