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

#[test]
fn merge_mode_is_unsupported() {
    let (_src, bin, bundle) = seed_source("import_merge_src");
    let (fresh, _) = common::setup_test_project("import_merge_target");
    let refused = common::run_cmd(
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
    assert!(!refused.status.success());
    assert_eq!(refused.status.code(), Some(10));
    assert_eq!(json(&refused)["error"]["code"], "UNSUPPORTED_OPERATION");
}

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
