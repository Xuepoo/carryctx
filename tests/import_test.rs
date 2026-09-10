mod common;

use std::path::{Path, PathBuf};

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

fn task_count(repo: &Path, bin: &Path) -> usize {
    let output = common::run_cmd(repo, bin, &["task", "list", "--json"]);
    assert!(output.status.success(), "task list failed: {output:?}");
    json(&output)["data"].as_array().map(Vec::len).unwrap_or(0)
}

fn event_count(db: &Path) -> i64 {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
        .unwrap()
}

fn imported_event_count(db: &Path) -> i64 {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM events WHERE type = 'project.imported'",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

/// Minimal ctxpack dumper for tests: mirrors the Section 2 layout
/// (`manifest.json`, `project.json`, one `*.jsonl` per table) by reading
/// `SELECT *` rows directly. Production `export` owns the real dumper;
/// this helper exists so import tests do not depend on T2's binary output.
fn dump_bundle(src_repo: &Path, bundle_dir: &Path) {
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
    let src_db = db_path(src_repo);
    let conn = rusqlite::Connection::open(&src_db).unwrap();
    std::fs::create_dir_all(bundle_dir).unwrap();

    let project_row = read_row(&conn, "projects");
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

    let schema_version: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 17) FROM schema_migrations",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let manifest = serde_json::json!({
        "format": "carryctx-pack-dir",
        "format_version": 1,
        "carryctx_version": env!("CARGO_PKG_VERSION"),
        "schema_version": schema_version,
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

fn read_row(conn: &rusqlite::Connection, table: &str) -> serde_json::Value {
    let rows = read_all_rows(conn, table);
    assert!(!rows.is_empty(), "table {table} must have a row");
    rows.into_iter().next().unwrap()
}

fn read_all_rows(conn: &rusqlite::Connection, table: &str) -> Vec<serde_json::Value> {
    let columns = column_names(conn, table);
    if columns.is_empty() {
        return Vec::new();
    }
    let sql = format!("SELECT * FROM {table}");
    let mut stmt = conn.prepare(&sql).unwrap();
    let rows = stmt
        .query_map([], |row| {
            let mut map = serde_json::Map::new();
            for (i, col) in columns.iter().enumerate() {
                let value: rusqlite::types::Value = row.get(i)?;
                map.insert(col.clone(), sql_to_json(value));
            }
            Ok(serde_json::Value::Object(map))
        })
        .unwrap();
    rows.map(|r| r.unwrap()).collect()
}

fn column_names(conn: &rusqlite::Connection, table: &str) -> Vec<String> {
    // Table may not exist on very old fixtures; treat as empty.
    let mut stmt = match conn.prepare(&format!("PRAGMA table_info({table})")) {
        Ok(stmt) => stmt,
        Err(_) => return Vec::new(),
    };
    stmt.query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
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

fn seed_source(name: &str) -> (PathBuf, PathBuf, PathBuf) {
    let (dir, bin) = common::setup_test_project(name);
    init(&dir, &bin);
    let agent = common::run_cmd(
        &dir,
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
    let task = common::run_cmd(&dir, &bin, &["task", "create", "--title", "pack me"]);
    assert!(task.status.success(), "task create failed: {task:?}");
    let bundle = dir.join("bundle");
    dump_bundle(&dir, &bundle);
    (dir, bin, bundle)
}

#[test]
fn fresh_import_round_trips_tasks_and_events() {
    let (src, bin, bundle) = seed_source("import_fresh_roundtrip");
    let src_tasks = task_count(&src, &bin);
    let src_events = event_count(&db_path(&src));
    assert!(src_tasks >= 1);

    let (fresh, _) = common::setup_test_project("import_fresh_target");
    // Fresh target: git repo only, no carryctx init yet.
    assert!(!db_path(&fresh).exists());

    let imported = common::run_cmd(
        &fresh,
        &bin,
        &["import", bundle.to_str().unwrap(), "--json"],
    );
    assert!(imported.status.success(), "import failed: {imported:?}");
    let body = json(&imported);
    assert_eq!(body["command"], "import.create");
    assert_eq!(body["success"], true);
    assert_eq!(body["data"]["operation"]["applied"], true);

    assert_eq!(task_count(&fresh, &bin), src_tasks);
    // Bundle events + exactly one project.imported audit event.
    assert_eq!(event_count(&db_path(&fresh)), src_events + 1);
    assert_eq!(imported_event_count(&db_path(&fresh)), 1);

    let doctor = common::run_cmd(&fresh, &bin, &["doctor", "--json"]);
    assert!(doctor.status.success(), "doctor failed: {doctor:?}");
}

/// CTX-0113 regression: tasks carrying `team_id` must survive a fresh
/// import. `LOAD_ORDER` once inserted `tasks` before `teams`, so the
/// `tasks_reject_cross_project_team` trigger aborted the load with
/// "task team must belong to task project" even for a consistent bundle.
#[test]
fn fresh_import_round_trips_team_associated_tasks() {
    let (src, bin) = common::setup_test_project("import_team_src");
    init(&src, &bin);
    let agent = common::run_cmd(
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

    let created = common::run_cmd(
        &src,
        &bin,
        &[
            "team",
            "create",
            "--name",
            "alpha",
            "--commander",
            "tester",
            "--json",
        ],
    );
    assert!(created.status.success(), "team create failed: {created:?}");
    let team_id = json(&created)["data"]["team"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(!team_id.is_empty());

    let task = common::run_cmd(
        &src,
        &bin,
        &[
            "task",
            "create",
            "--title",
            "teamed work",
            "--team",
            "alpha",
            "--json",
        ],
    );
    assert!(task.status.success(), "task create failed: {task:?}");
    assert_eq!(
        json(&task)["data"]["team_id"].as_str().unwrap(),
        team_id.as_str(),
        "seeded task must carry the team id"
    );

    let bundle = src.join("bundle-team");
    dump_bundle(&src, &bundle);

    let (fresh, _) = common::setup_test_project("import_team_target");
    assert!(!db_path(&fresh).exists());
    let imported = common::run_cmd(
        &fresh,
        &bin,
        &["import", bundle.to_str().unwrap(), "--json"],
    );
    assert!(imported.status.success(), "import failed: {imported:?}");
    let body = json(&imported);
    assert_eq!(body["command"], "import.create");
    assert_eq!(body["success"], true);

    // Team row survives with its commander, and the task still points at it.
    let status = common::run_cmd(&fresh, &bin, &["team", "status", "alpha", "--json"]);
    assert!(status.status.success(), "team status failed: {status:?}");
    let conn = rusqlite::Connection::open(db_path(&fresh)).unwrap();
    let loaded_team: String = conn
        .query_row(
            "SELECT team_id FROM tasks WHERE title = 'teamed work'",
            [],
            |row| row.get(0),
        )
        .expect("teamed task must exist after import");
    assert_eq!(loaded_team, team_id);
    let commander: Option<String> = conn
        .query_row(
            "SELECT commander_agent_id FROM teams WHERE id = ?1",
            [&team_id],
            |row| row.get(0),
        )
        .expect("team must exist after import");
    assert!(commander.is_some(), "team commander must survive import");

    let doctor = common::run_cmd(&fresh, &bin, &["doctor", "--json"]);
    assert!(doctor.status.success(), "doctor failed: {doctor:?}");
}

/// CTX-0113 follow-up found via the live-DB repro: the source database can
/// hold `events.task_id` values whose task is long gone (legacy manual
/// deletes predate the prune-time unlink; `prune` itself nulls these links
/// while keeping the audit rows). Import must converge to that same state —
/// keep the audit row, null the dangling link — instead of refusing the
/// whole bundle with `FOREIGN KEY constraint failed`.
#[test]
fn fresh_import_nulls_dangling_event_task_refs_and_keeps_history() {
    let (src, bin) = common::setup_test_project("import_event_orphan_src");
    init(&src, &bin);
    let agent = common::run_cmd(
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
    let task = common::run_cmd(&src, &bin, &["task", "create", "--title", "pack me"]);
    assert!(task.status.success(), "task create failed: {task:?}");
    let bundle = src.join("bundle-orphan");
    dump_bundle(&src, &bundle);

    // Append one audit row whose task_id names a task absent from the bundle.
    let manifest_path = bundle.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    let project_id = manifest["project_id"].as_str().unwrap().to_string();
    let orphan_id = ulid::Ulid::generate().to_string();
    let orphan = serde_json::json!({
        "id": orphan_id,
        "project_id": project_id,
        "type": "task.started",
        "aggregate_type": "task",
        "aggregate_id": "missing-task",
        "payload_json": "{}",
        "occurred_at": chrono::Utc::now().to_rfc3339(),
        "actor_agent_id": serde_json::Value::Null,
        "session_id": serde_json::Value::Null,
        "task_id": "missing-task",
    });
    let mut events_text = std::fs::read_to_string(bundle.join("events.jsonl")).unwrap();
    events_text.push_str(&serde_json::to_string(&orphan).unwrap());
    events_text.push('\n');
    std::fs::write(bundle.join("events.jsonl"), events_text).unwrap();
    let next = manifest["counts"]["events"].as_u64().unwrap() + 1;
    manifest["counts"]["events"] = serde_json::json!(next);
    std::fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let src_events = event_count(&db_path(&src));
    let (fresh, _) = common::setup_test_project("import_event_orphan_target");
    let imported = common::run_cmd(
        &fresh,
        &bin,
        &["import", bundle.to_str().unwrap(), "--json"],
    );
    assert!(imported.status.success(), "import failed: {imported:?}");

    // Orphan audit row survives with its link nulled; history is preserved.
    let conn = rusqlite::Connection::open(db_path(&fresh)).unwrap();
    let task_id: Option<String> = conn
        .query_row(
            "SELECT task_id FROM events WHERE id = ?1",
            [&orphan_id],
            |row| row.get(0),
        )
        .expect("orphan event must exist after import");
    assert_eq!(task_id, None, "dangling event task link must be nulled");
    assert_eq!(
        event_count(&db_path(&fresh)),
        src_events + 2,
        "orphan row plus exactly one project.imported event"
    );

    let doctor = common::run_cmd(&fresh, &bin, &["doctor", "--json"]);
    assert!(doctor.status.success(), "doctor failed: {doctor:?}");
}

#[test]
fn initialized_bare_import_refuses_with_state_conflict_hint() {
    let (_src, bin, bundle) = seed_source("import_refuse_src");
    let (target, _) = common::setup_test_project("import_refuse_target");
    init(&target, &bin);
    let before = std::fs::read(db_path(&target)).unwrap();

    let refused = common::run_cmd(
        &target,
        &bin,
        &["import", bundle.to_str().unwrap(), "--json"],
    );
    assert!(!refused.status.success());
    assert_eq!(refused.status.code(), Some(3));
    let body = json(&refused);
    assert_eq!(body["error"]["code"], "STATE_CONFLICT");
    let message = body["error"]["message"].as_str().unwrap_or("");
    assert!(
        message.contains("--mode replace"),
        "hint must mention --mode replace: {message}"
    );
    assert_eq!(std::fs::read(db_path(&target)).unwrap(), before);
}

#[test]
fn replace_apply_leaves_pre_import_backup_and_event() {
    let (src, bin, bundle) = seed_source("import_replace_src");
    let (target, _) = common::setup_test_project("import_replace_target");
    init(&target, &bin);
    // Diverge the target so replacement is observable.
    let extra = common::run_cmd(
        &target,
        &bin,
        &[
            "agent",
            "register",
            "--name",
            "local-only",
            "--provider",
            "test",
        ],
    );
    assert!(extra.status.success());

    // Replace without --yes refuses on non-TTY (tests are piped).
    let need_yes = common::run_cmd(
        &target,
        &bin,
        &[
            "import",
            bundle.to_str().unwrap(),
            "--mode",
            "replace",
            "--json",
        ],
    );
    assert!(!need_yes.status.success());
    assert_eq!(json(&need_yes)["error"]["code"], "STATE_CONFLICT");

    let replaced = common::run_cmd(
        &target,
        &bin,
        &[
            "import",
            bundle.to_str().unwrap(),
            "--mode",
            "replace",
            "--yes",
            "--json",
        ],
    );
    assert!(replaced.status.success(), "replace failed: {replaced:?}");
    let body = json(&replaced);
    assert_eq!(body["data"]["mode"], "replace");
    assert_eq!(body["data"]["operation"]["applied"], true);

    let backup_dir = target.join(".git/carryctx/backups");
    let pre_import = std::fs::read_dir(&backup_dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with("pre_import_"))
        .count();
    assert_eq!(pre_import, 1, "expected one pre_import backup");
    assert_eq!(imported_event_count(&db_path(&target)), 1);

    // Target now mirrors the source: local-only agent is gone, tasks match.
    let agents = common::run_cmd(&target, &bin, &["agent", "list", "--json"]);
    assert!(agents.status.success());
    assert!(
        !String::from_utf8_lossy(&agents.stdout).contains("local-only"),
        "replace must drop diverged local state"
    );
    assert_eq!(task_count(&target, &bin), task_count(&src, &bin));
}

#[test]
fn tampered_manifest_refuses_without_partial_state() {
    let (_src, bin, bundle_src) = seed_source("import_tamper_src");
    let tampered = bundle_src.join("tampered");
    // Copy the valid bundle, then skew one count.
    let copy_dir = tampered.clone();
    std::fs::create_dir_all(&copy_dir).unwrap();
    for entry in std::fs::read_dir(&bundle_src).unwrap() {
        let entry = entry.unwrap();
        if entry.path() == copy_dir {
            continue;
        }
        if entry.file_type().unwrap().is_file() {
            std::fs::copy(entry.path(), copy_dir.join(entry.file_name())).unwrap();
        }
    }
    let manifest_path = copy_dir.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    // Claim 9999 tasks while the bundle holds fewer.
    manifest["counts"]["tasks"] = serde_json::json!(9999);
    std::fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let (fresh, _) = common::setup_test_project("import_tamper_target");
    let refused = common::run_cmd(
        &fresh,
        &bin,
        &["import", copy_dir.to_str().unwrap(), "--json"],
    );
    assert!(!refused.status.success());
    assert_eq!(refused.status.code(), Some(8));
    assert_eq!(json(&refused)["error"]["code"], "VALIDATION_FAILED");
    assert!(
        !db_path(&fresh).exists(),
        "tampered import must write no partial state"
    );
}

#[test]
fn future_format_version_refuses_as_unsupported() {
    let (_src, bin, bundle_src) = seed_source("import_future_src");
    let future_dir = bundle_src.join("future");
    std::fs::create_dir_all(&future_dir).unwrap();
    for entry in std::fs::read_dir(&bundle_src).unwrap() {
        let entry = entry.unwrap();
        if entry.path() == future_dir || !entry.file_type().unwrap().is_file() {
            continue;
        }
        std::fs::copy(entry.path(), future_dir.join(entry.file_name())).unwrap();
    }
    let manifest_path = future_dir.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    manifest["format_version"] = serde_json::json!(999);
    std::fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let (fresh, _) = common::setup_test_project("import_future_target");
    let refused = common::run_cmd(
        &fresh,
        &bin,
        &["import", future_dir.to_str().unwrap(), "--json"],
    );
    assert!(!refused.status.success());
    assert_eq!(refused.status.code(), Some(10));
    assert_eq!(json(&refused)["error"]["code"], "UNSUPPORTED_OPERATION");
    assert!(!db_path(&fresh).exists());
}

// `--mode merge` moved from UNSUPPORTED_OPERATION to the CTX-0142 merge
// path; merge behavior is covered by `tests/import_merge_test.rs`.

#[test]
fn dry_run_validates_without_writing() {
    let (_src, bin, bundle) = seed_source("import_dryrun_src");

    // Fresh target dry-run: validates, diffs, writes nothing.
    let (fresh, _) = common::setup_test_project("import_dryrun_fresh");
    let preview = common::run_cmd(
        &fresh,
        &bin,
        &["import", bundle.to_str().unwrap(), "--dry-run", "--json"],
    );
    assert!(preview.status.success(), "dry-run failed: {preview:?}");
    let body = json(&preview);
    assert_eq!(body["data"]["operation"]["applied"], false);
    assert_eq!(body["data"]["would_replace"], false);
    assert!(!db_path(&fresh).exists());
    assert!(!fresh.join(".carryctx/config.toml").exists());

    // Initialized target dry-run: diffs would_replace, leaves DB untouched.
    let (target, _) = common::setup_test_project("import_dryrun_init");
    init(&target, &bin);
    let before = std::fs::read(db_path(&target)).unwrap();
    let preview = common::run_cmd(
        &target,
        &bin,
        &[
            "import",
            bundle.to_str().unwrap(),
            "--mode",
            "replace",
            "--dry-run",
            "--json",
        ],
    );
    assert!(preview.status.success(), "dry-run failed: {preview:?}");
    assert_eq!(json(&preview)["data"]["operation"]["applied"], false);
    assert_eq!(json(&preview)["data"]["would_replace"], true);
    assert_eq!(std::fs::read(db_path(&target)).unwrap(), before);
}

#[test]
fn import_requires_a_git_repo() {
    let (_src, bin, bundle) = seed_source("import_git_src");
    let root = tempfile::tempdir().unwrap();
    let plain = root.path().join("nogit");
    std::fs::create_dir_all(&plain).unwrap();
    let refused = common::run_cmd(
        &plain,
        &bin,
        &["import", bundle.to_str().unwrap(), "--json"],
    );
    assert!(!refused.status.success());
    assert_eq!(refused.status.code(), Some(4));
    assert_eq!(json(&refused)["error"]["code"], "GIT_ERROR");
}

/// CTX-0137 fixture: seed a source project with one worktree whose
/// `normalized_path` is `worktree_path`, plus a session and a checkpoint
/// carrying that `worktree_id`. Rows are inserted directly so the test
/// controls the FK link independently of worktree/session command flows.
fn seed_session_bound_to_worktree(name: &str, worktree_path: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let (src, bin) = common::setup_test_project(name);
    init(&src, &bin);
    let agent = common::run_cmd(
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
    let task = common::run_cmd(&src, &bin, &["task", "create", "--title", "worktree-bound"]);
    assert!(task.status.success(), "task create failed: {task:?}");

    let conn = rusqlite::Connection::open(db_path(&src)).unwrap();
    let project_id: String = conn
        .query_row("SELECT id FROM projects", [], |row| row.get(0))
        .unwrap();
    let agent_id: String = conn
        .query_row("SELECT id FROM agents WHERE name = 'tester'", [], |row| {
            row.get(0)
        })
        .unwrap();
    let task_id: String = conn
        .query_row(
            "SELECT id FROM tasks WHERE title = 'worktree-bound'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let worktree_id = ulid::Ulid::generate().to_string();
    let session_id = ulid::Ulid::generate().to_string();
    let checkpoint_id = ulid::Ulid::generate().to_string();
    let worktree_path = worktree_path.to_string_lossy().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO worktrees (id, project_id, task_id, normalized_path, git_common_dir, bound_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        rusqlite::params![
            worktree_id,
            project_id,
            task_id,
            worktree_path,
            src.join(".git").to_string_lossy(),
            now,
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sessions (id, project_id, agent_id, task_id, worktree_id, state, provider, working_directory, metadata_json, started_at, last_activity_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'active', 'test', ?6, '{}', ?7, ?7, ?7)",
        rusqlite::params![session_id, project_id, agent_id, task_id, worktree_id, worktree_path, now],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO checkpoints (id, project_id, task_id, session_id, worktree_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            checkpoint_id,
            project_id,
            task_id,
            session_id,
            worktree_id,
            now
        ],
    )
    .unwrap();
    drop(conn);

    let bundle = src.join("bundle-worktree");
    dump_bundle(&src, &bundle);
    (src, bin, bundle)
}

/// CTX-0137 regression: a consistent bundle whose session references a
/// worktree that is live at the import target must round-trip. `LOAD_ORDER`
/// once inserted `sessions` before `worktrees`, so the FK to a worktree that
/// was about to be inserted aborted the load with
/// "Failed to load pack table 'sessions': FOREIGN KEY constraint failed".
#[test]
fn fresh_import_keeps_session_worktree_link_when_worktree_is_live() {
    let (target, _) = common::setup_test_project("import_wt_live_target");
    let (_src, bin, bundle) = seed_session_bound_to_worktree("import_wt_live_src", &target);

    let imported = common::run_cmd(
        &target,
        &bin,
        &["import", bundle.to_str().unwrap(), "--json"],
    );
    assert!(imported.status.success(), "import failed: {imported:?}");

    let conn = rusqlite::Connection::open(db_path(&target)).unwrap();
    let worktree_id: String = conn
        .query_row("SELECT id FROM worktrees", [], |row| row.get(0))
        .expect("live worktree must survive import");
    let session_worktree: Option<String> = conn
        .query_row("SELECT worktree_id FROM sessions", [], |row| row.get(0))
        .expect("session must survive import");
    assert_eq!(
        session_worktree.as_deref(),
        Some(worktree_id.as_str()),
        "session must keep its live worktree link"
    );
    let checkpoint_worktree: Option<String> = conn
        .query_row("SELECT worktree_id FROM checkpoints", [], |row| row.get(0))
        .expect("checkpoint must survive import");
    assert_eq!(
        checkpoint_worktree.as_deref(),
        Some(worktree_id.as_str()),
        "checkpoint must keep its live worktree link"
    );

    let doctor = common::run_cmd(&target, &bin, &["doctor", "--json"]);
    assert!(doctor.status.success(), "doctor failed: {doctor:?}");
}

/// CTX-0137 regression: on a fresh machine every source worktree path is
/// absent, so the Section 4 re-anchor policy prunes them. A session whose
/// `worktree_id` names a pruned worktree must still import — the history row
/// is kept and only the link to the dropped worktree is nulled, mirroring
/// the dangling `events.task_id` convergence. Fail-closed for every other FK.
#[test]
fn fresh_import_nulls_pruned_worktree_refs_and_keeps_history() {
    let missing = Path::new("/nonexistent/carryctx-ctx0137-pruned-worktree");
    let (_src, bin, bundle) = seed_session_bound_to_worktree("import_wt_pruned_src", missing);

    let (fresh, _) = common::setup_test_project("import_wt_pruned_target");
    let imported = common::run_cmd(
        &fresh,
        &bin,
        &["import", bundle.to_str().unwrap(), "--json"],
    );
    assert!(imported.status.success(), "import failed: {imported:?}");

    let body = json(&imported);
    let warnings: Vec<String> = body["data"]["warnings"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let warning_text = warnings.join("\n");
    assert!(
        warning_text.contains("Pruned worktree"),
        "expected worktree prune warning: {warning_text}"
    );
    assert!(
        warning_text.contains("session worktree reference"),
        "expected nulled session reference warning: {warning_text}"
    );

    let conn = rusqlite::Connection::open(db_path(&fresh)).unwrap();
    let worktree_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM worktrees", [], |row| row.get(0))
        .unwrap();
    assert_eq!(worktree_count, 0, "pruned worktree must not be live");
    let session_worktree: Option<String> = conn
        .query_row("SELECT worktree_id FROM sessions", [], |row| row.get(0))
        .expect("session history row must survive import");
    assert_eq!(
        session_worktree, None,
        "link to a pruned worktree must be nulled"
    );
    let checkpoint_worktree: Option<String> = conn
        .query_row("SELECT worktree_id FROM checkpoints", [], |row| row.get(0))
        .expect("checkpoint history row must survive import");
    assert_eq!(
        checkpoint_worktree, None,
        "checkpoint link to a pruned worktree must be nulled"
    );
    let sessions: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
        .unwrap();
    let checkpoints: i64 = conn
        .query_row("SELECT COUNT(*) FROM checkpoints", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        (sessions, checkpoints),
        (1, 1),
        "history rows are kept, only the dropped links are nulled"
    );

    let doctor = common::run_cmd(&fresh, &bin, &["doctor", "--json"]);
    assert!(doctor.status.success(), "doctor failed: {doctor:?}");
}

#[test]
fn v1_bundle_without_tombstones_file_imports_unchanged() {
    let (_src, bin, bundle_src) = seed_source("import_v1_compat_src");
    assert!(
        !bundle_src.join("tombstones.jsonl").exists(),
        "v1 fixture must not ship v2-only files"
    );
    let (fresh, _) = common::setup_test_project("import_v1_compat_target");
    let out = common::run_cmd(
        &fresh,
        &bin,
        &["import", bundle_src.to_str().unwrap(), "--json"],
    );
    assert!(out.status.success(), "v1 import failed: {out:?}");

    let conn = rusqlite::Connection::open(db_path(&fresh)).unwrap();
    let payload: String = conn
        .query_row(
            "SELECT payload_json FROM events WHERE type = 'project.imported'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        payload.contains("\"formatVersion\":1"),
        "source format must be recorded verbatim: {payload}"
    );
    assert!(
        payload.contains("\"tombstones\":0"),
        "v1 bundles carry an empty tombstone set: {payload}"
    );
}

#[test]
fn v2_bundle_with_parents_imports_and_records_format_v2() {
    let (_src, bin, bundle_src) = seed_source("import_v2_src");
    let v2_dir = bundle_src.join("v2");
    std::fs::create_dir_all(&v2_dir).unwrap();
    for entry in std::fs::read_dir(&bundle_src).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            std::fs::copy(entry.path(), v2_dir.join(entry.file_name())).unwrap();
        }
    }
    let manifest_path = v2_dir.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    manifest["format_version"] = serde_json::json!(2);
    manifest["parents"] = serde_json::json!(["01PARENT"]);
    manifest["counts"]["tombstones"] = serde_json::json!(0);
    std::fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    std::fs::write(v2_dir.join("tombstones.jsonl"), "").unwrap();

    let (fresh, _) = common::setup_test_project("import_v2_target");
    let out = common::run_cmd(
        &fresh,
        &bin,
        &["import", v2_dir.to_str().unwrap(), "--json"],
    );
    assert!(out.status.success(), "v2 import failed: {out:?}");
    assert_eq!(task_count(&fresh, &bin), 1);

    let conn = rusqlite::Connection::open(db_path(&fresh)).unwrap();
    let payload: String = conn
        .query_row(
            "SELECT payload_json FROM events WHERE type = 'project.imported'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        payload.contains("\"formatVersion\":2"),
        "source format must be recorded verbatim: {payload}"
    );
}
