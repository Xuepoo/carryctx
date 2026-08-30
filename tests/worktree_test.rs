mod common;

#[test]
fn cleanup_cli_surface_has_json_envelopes_and_dry_run_is_read_only() {
    let (dir, bin) = common::setup_test_project("cleanup_cli_surface_test");
    common::init_and_agent(&dir, &bin);

    let list = common::run_cmd(&dir, &bin, &["worktree", "cleanup", "list", "--json"]);
    assert!(list.status.success());
    assert!(list.stderr.is_empty(), "JSON list leaked stderr");
    let list_json: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(list_json["success"], true);
    assert!(list_json["data"].is_array());

    let before = std::fs::read(dir.join(".git/carryctx/state.sqlite")).unwrap();
    let dry_run = common::run_cmd(
        &dir,
        &bin,
        &["worktree", "cleanup", "run", "--dry-run", "--json"],
    );
    assert!(dry_run.status.success());
    assert!(dry_run.stderr.is_empty(), "JSON dry-run leaked stderr");
    let dry_json: serde_json::Value = serde_json::from_slice(&dry_run.stdout).unwrap();
    assert_eq!(dry_json["success"], true);
    assert_eq!(dry_json["data"]["operation"]["applied"], false);
    let after = std::fs::read(dir.join(".git/carryctx/state.sqlite")).unwrap();
    assert_eq!(before, after, "dry-run changed the database");

    let global_dry_run = common::run_cmd(
        &dir,
        &bin,
        &["--dry-run", "worktree", "cleanup", "run", "--json"],
    );
    assert!(global_dry_run.status.success());
    assert!(
        global_dry_run.stderr.is_empty(),
        "global JSON dry-run leaked stderr"
    );
    let global_json: serde_json::Value = serde_json::from_slice(&global_dry_run.stdout).unwrap();
    assert_eq!(global_json["success"], true);
    assert_eq!(global_json["data"]["operation"]["applied"], false);
    assert_eq!(
        after,
        std::fs::read(dir.join(".git/carryctx/state.sqlite")).unwrap()
    );

    let missing = common::run_cmd(
        &dir,
        &bin,
        &["worktree", "cleanup", "show", "missing-request", "--json"],
    );
    assert!(!missing.status.success());
    assert!(missing.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&missing.stderr).unwrap();
    assert_eq!(error["success"], false);
    assert_eq!(error["command"], "worktree.cleanup.show");
}

#[test]
fn cleanup_cli_markdown_outputs_tables_for_list_and_dry_run() {
    let (dir, bin) = common::setup_test_project("cleanup_cli_markdown_test");
    common::init_and_agent(&dir, &bin);
    let created = common::run_cmd(
        &dir,
        &bin,
        &["--json", "task", "create", "--title", "markdown cleanup"],
    );
    let task_json: serde_json::Value = serde_json::from_slice(&created.stdout).unwrap();
    let task = task_json["data"]["display_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        common::run_cmd(&dir, &bin, &["task", "start", &task])
            .status
            .success()
    );
    let path = dir.parent().unwrap().join("cleanup-markdown-worktree");
    assert!(
        common::run_cmd(
            &dir,
            &bin,
            &[
                "worktree",
                "create",
                &task,
                "--path",
                path.to_str().unwrap()
            ]
        )
        .status
        .success()
    );
    assert!(
        common::run_cmd(&dir, &bin, &["task", "complete", &task])
            .status
            .success()
    );
    let request_id: String = rusqlite::Connection::open(dir.join(".git/carryctx/state.sqlite"))
        .unwrap()
        .query_row(
            "SELECT id FROM worktree_cleanup_requests LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();

    let commands: Vec<Vec<String>> = vec![
        vec!["worktree", "cleanup", "list", "--format", "markdown"]
            .into_iter()
            .map(String::from)
            .collect(),
        vec![
            "worktree",
            "cleanup",
            "show",
            &request_id,
            "--format",
            "markdown",
        ]
        .into_iter()
        .map(String::from)
        .collect(),
        vec![
            "worktree",
            "cleanup",
            "run",
            "--dry-run",
            "--format",
            "markdown",
        ]
        .into_iter()
        .map(String::from)
        .collect(),
    ];
    for args in commands {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = common::run_cmd(&dir, &bin, &args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("# Cleanup"),
            "expected markdown heading: {stdout}"
        );
        assert!(
            stdout.contains("| Request | State | Path |"),
            "expected markdown table: {stdout}"
        );
        assert!(
            !stdout.trim_start().starts_with('{'),
            "markdown must not be JSON: {stdout}"
        );
    }
}

/// Requires the `jj` binary on PATH. Not run by default in `cargo test`
/// (no CI guarantee jj is installed); run explicitly with
/// `cargo test --test worktree_test -- --ignored`.
///
/// Verifies Phase 3 of carryctx-docs/plans/2026-07-25-jujutsu-compatibility.md:
/// `carryctx worktree create` refuses with a clear, non-panicking error under
/// jj colocation instead of silently creating a directory neither `jj` nor
/// carryctx's own state commands can use from inside (jj secondary
/// workspaces from `jj workspace add` have no `.git/`, and `git worktree add`
/// produces a directory `jj workspace list` never discovers).
#[test]
#[ignore]
fn test_worktree_create_refuses_under_jj_colocation() {
    let (dir, bin) = common::setup_test_project("worktree_jj_test");

    let jj_init = std::process::Command::new("jj")
        .args(["git", "init", "--colocate"])
        .current_dir(&dir)
        .output()
        .expect("jj binary must be on PATH to run this test");
    assert!(
        jj_init.status.success(),
        "jj git init --colocate failed: {}",
        String::from_utf8_lossy(&jj_init.stderr)
    );

    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "WJ"]);
    common::run_cmd(
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
    common::run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", "jj worktree task"],
    );

    let result = common::run_cmd(&dir, &bin, &["worktree", "create", "WJ-0001", "--json"]);
    assert!(
        !result.status.success(),
        "worktree create must fail under jj colocation, not silently create a broken worktree"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    let value: serde_json::Value =
        serde_json::from_str(&stderr).expect("valid JSON error envelope on stderr");
    assert_eq!(value["success"], false);
    assert_eq!(value["error"]["code"], "VALIDATION_FAILED");
    let message = value["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("jj"),
        "error message should explain the jj-specific reason: {message}"
    );

    // The directory carryctx would have created must not exist.
    assert!(
        !dir.join(".worktrees").exists(),
        "no worktree directory should have been created on refusal"
    );
}

/// Companion regression check: plain (non-jj) repos must be completely
/// unaffected by the jj-colocation guard added for the test above.
#[test]
fn test_worktree_create_unaffected_by_jj_guard_on_plain_git() {
    let (dir, bin) = common::setup_test_project("worktree_plain_git_test");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "WP"]);
    common::run_cmd(
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
    common::run_cmd(&dir, &bin, &["task", "create", "--title", "plain git task"]);

    let result = common::run_cmd(&dir, &bin, &["worktree", "create", "WP-0001", "--json"]);
    assert!(
        result.status.success(),
        "worktree create should succeed on plain git: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn test_doctor_detects_and_explicitly_prunes_missing_worktree_registration() {
    let (dir, bin) = common::setup_test_project("worktree_stale_doctor_test");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "ST"]);
    common::run_cmd(
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
    common::run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", "stale worktree task"],
    );

    let created = common::run_cmd(&dir, &bin, &["worktree", "create", "ST-0001", "--json"]);
    assert!(
        created.status.success(),
        "worktree create failed: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    let worktree_path = dir.join(".worktrees/st-0001");
    assert!(worktree_path.exists());
    std::fs::remove_dir_all(&worktree_path).expect("remove only the disposable fixture worktree");

    let doctor = common::run_cmd(&dir, &bin, &["doctor", "--json"]);
    // CTX-0083: the stale-worktree finding is warning-class, so doctor
    // exits 0 while still reporting it with its fix command below.
    assert!(doctor.status.success());
    let doctor_json: serde_json::Value = serde_json::from_slice(&doctor.stdout).unwrap();
    let stale = doctor_json["data"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["check"] == "worktrees.stale")
        .expect("doctor should report stale worktrees");
    assert_eq!(stale["status"], "warning");
    assert_eq!(stale["count"], 1);
    assert_eq!(
        stale["fix_command"],
        "carryctx doctor --prune-stale-worktrees"
    );

    // Re-running without pruning keeps reporting without mutating state.
    let no_mutation = common::run_cmd(&dir, &bin, &["doctor", "--json"]);
    assert!(no_mutation.status.success());

    let dry_run = common::run_cmd(
        &dir,
        &bin,
        &["doctor", "--prune-stale-worktrees", "--dry-run", "--json"],
    );
    assert!(dry_run.status.success());
    let dry_run_json: serde_json::Value = serde_json::from_slice(&dry_run.stdout).unwrap();
    let dry_run_stale = dry_run_json["data"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["check"] == "worktrees.stale")
        .unwrap();
    assert_eq!(dry_run_stale["status"], "warning");
    assert_eq!(
        dry_run_stale["message"],
        "Would prune 1 stale worktree registration(s)"
    );

    let unauthorized = common::run_cmd(
        &dir,
        &bin,
        &[
            "doctor",
            "--prune-stale-worktrees",
            "--non-interactive",
            "--json",
        ],
    );
    assert!(!unauthorized.status.success());
    assert!(
        unauthorized.stdout.is_empty(),
        "unauthorized prune must not write to stdout: {}",
        String::from_utf8_lossy(&unauthorized.stdout)
    );
    assert_eq!(unauthorized.status.code(), Some(9));
    let unauthorized_error: serde_json::Value =
        serde_json::from_slice(&unauthorized.stderr).expect("valid JSON error envelope on stderr");
    assert_eq!(unauthorized_error["command"], "doctor");
    assert_eq!(unauthorized_error["success"], false);
    assert_eq!(unauthorized_error["error"]["code"], "PERMISSION_SCOPE");
    assert!(
        unauthorized_error["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("--yes")
    );

    let prune = common::run_cmd(
        &dir,
        &bin,
        &["doctor", "--prune-stale-worktrees", "--yes", "--json"],
    );
    assert!(
        prune.status.success(),
        "prune failed: {}",
        String::from_utf8_lossy(&prune.stderr)
    );
    let after = common::run_cmd(&dir, &bin, &["doctor", "--json"]);
    assert!(after.status.success());
    let after_json: serde_json::Value = serde_json::from_slice(&after.stdout).unwrap();
    let stale_after = after_json["data"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["check"] == "worktrees.stale")
        .unwrap();
    assert_eq!(stale_after["status"], "ok");
    assert_eq!(
        stale_after["message"],
        "No registered worktrees have missing directories"
    );

    let db = rusqlite::Connection::open(dir.join(".git/carryctx/state.sqlite")).unwrap();
    let pruned_events: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM events WHERE type = 'worktree.pruned'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pruned_events, 1);
}

#[test]
fn test_prune_detaches_fk_references_and_attributes_audit_actor() {
    let (dir, bin) = common::setup_test_project("worktree_prune_fk_test");
    common::init_and_agent(&dir, &bin);
    common::run_cmd(&dir, &bin, &["task", "create", "--title", "dependent task"]);
    common::run_cmd(&dir, &bin, &["worktree", "create", "CTX-0001", "--json"]);
    let worktree_path = dir.join(".worktrees/ctx-0001");
    std::fs::remove_dir_all(&worktree_path).unwrap();

    let db_path = dir.join(".git/carryctx/state.sqlite");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    let worktree_id: String = db
        .query_row("SELECT id FROM worktrees LIMIT 1", [], |row| row.get(0))
        .unwrap();
    let project_id: String = db
        .query_row("SELECT id FROM projects LIMIT 1", [], |row| row.get(0))
        .unwrap();
    let agent_id: String = db
        .query_row("SELECT id FROM agents WHERE name = 'tester'", [], |row| {
            row.get(0)
        })
        .unwrap();
    let task_id: String = db
        .query_row("SELECT id FROM tasks LIMIT 1", [], |row| row.get(0))
        .unwrap();
    db.execute(
        "INSERT INTO sessions (id, project_id, agent_id, worktree_id, state, provider, working_directory, started_at, last_activity_at, updated_at)
         VALUES ('session-prune', ?1, ?2, ?3, 'active', 'test', ?4, 'now', 'now', 'now')",
        rusqlite::params![project_id, agent_id, worktree_id, worktree_path.to_string_lossy()],
    )
    .unwrap();
    db.execute(
        "INSERT INTO checkpoints (id, project_id, task_id, session_id, worktree_id, created_at)
         VALUES ('checkpoint-prune', ?1, ?2, 'session-prune', ?3, 'now')",
        rusqlite::params![project_id, task_id, worktree_id],
    )
    .unwrap();

    let prune = common::run_cmd(
        &dir,
        &bin,
        &[
            "--session",
            "session-prune",
            "doctor",
            "--prune-stale-worktrees",
            "--yes",
            "--json",
        ],
    );
    assert!(
        prune.status.success(),
        "{}",
        String::from_utf8_lossy(&prune.stderr)
    );

    let session_worktree: Option<String> = db
        .query_row(
            "SELECT worktree_id FROM sessions WHERE id = 'session-prune'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let checkpoint_worktree: Option<String> = db
        .query_row(
            "SELECT worktree_id FROM checkpoints WHERE id = 'checkpoint-prune'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(session_worktree.is_none());
    assert!(checkpoint_worktree.is_none());
    let (actor, session): (Option<String>, Option<String>) = db
        .query_row(
            "SELECT actor_agent_id, session_id FROM events WHERE type = 'worktree.pruned'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(actor.as_deref(), Some(agent_id.as_str()));
    assert_eq!(session.as_deref(), Some("session-prune"));
}

#[test]
fn test_prune_rolls_back_registration_and_detach_when_audit_fails() {
    let (dir, bin) = common::setup_test_project("worktree_prune_rollback_test");
    common::init_and_agent(&dir, &bin);
    common::run_cmd(&dir, &bin, &["task", "create", "--title", "rollback task"]);
    common::run_cmd(&dir, &bin, &["worktree", "create", "CTX-0001", "--json"]);
    let worktree_path = dir.join(".worktrees/ctx-0001");
    std::fs::remove_dir_all(&worktree_path).unwrap();

    let db_path = dir.join(".git/carryctx/state.sqlite");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    let worktree_id: String = db
        .query_row("SELECT id FROM worktrees LIMIT 1", [], |row| row.get(0))
        .unwrap();
    db.execute_batch(
        "CREATE TRIGGER reject_prune_audit BEFORE INSERT ON events
         WHEN NEW.type = 'worktree.pruned'
         BEGIN SELECT RAISE(ABORT, 'test audit failure'); END;",
    )
    .unwrap();

    let prune = common::run_cmd(
        &dir,
        &bin,
        &["doctor", "--prune-stale-worktrees", "--yes", "--json"],
    );
    assert!(!prune.status.success());
    assert!(prune.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&prune.stderr).unwrap();
    assert_eq!(error["command"], "doctor");
    assert_eq!(error["success"], false);
    assert_eq!(error["error"]["code"], "DATABASE_ERROR");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("test audit failure")
    );
    let remaining: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM worktrees WHERE id = ?1",
            rusqlite::params![worktree_id],
            |row| row.get(0),
        )
        .unwrap();
    let audits: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM events WHERE type = 'worktree.pruned'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 1);
    assert_eq!(audits, 0);
}

#[test]
fn test_prune_implicitly_attributes_audit_to_active_session() {
    let (dir, bin) = common::setup_test_project("worktree_prune_implicit_session_test");
    common::init_and_agent(&dir, &bin);
    common::run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", "implicit session task"],
    );
    common::run_cmd(&dir, &bin, &["worktree", "create", "CTX-0001", "--json"]);
    let worktree_path = dir.join(".worktrees/ctx-0001");
    std::fs::remove_dir_all(&worktree_path).unwrap();

    let db_path = dir.join(".git/carryctx/state.sqlite");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    let worktree_id: String = db
        .query_row("SELECT id FROM worktrees LIMIT 1", [], |row| row.get(0))
        .unwrap();
    let project_id: String = db
        .query_row("SELECT id FROM projects LIMIT 1", [], |row| row.get(0))
        .unwrap();
    let agent_id: String = db
        .query_row("SELECT id FROM agents WHERE name = 'tester'", [], |row| {
            row.get(0)
        })
        .unwrap();
    db.execute(
        "INSERT INTO sessions (id, project_id, agent_id, state, provider, working_directory, started_at, last_activity_at, updated_at)
         VALUES ('session-implicit', ?1, ?2, 'active', 'test', ?3, 'now', 'now', 'now')",
        rusqlite::params![project_id, agent_id, dir.to_string_lossy()],
    )
    .unwrap();

    let prune = common::run_cmd(
        &dir,
        &bin,
        &["doctor", "--prune-stale-worktrees", "--yes", "--json"],
    );
    assert!(
        prune.status.success(),
        "{}",
        String::from_utf8_lossy(&prune.stderr)
    );

    let (actor, session): (Option<String>, Option<String>) = db
        .query_row(
            "SELECT actor_agent_id, session_id FROM events WHERE type = 'worktree.pruned' AND aggregate_id = ?1",
            rusqlite::params![worktree_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(actor.as_deref(), Some(agent_id.as_str()));
    assert_eq!(session.as_deref(), Some("session-implicit"));
}

/// CTX-0071 / issue #104: the worktree fallback matched `cwd.starts_with(path)`
/// by string prefix, so `/repo/wt-x` matched the worktree bound at `/repo/wt`
/// and silently resolved the WRONG task. Path comparison must respect
/// component boundaries.
#[test]
fn test_worktree_resolution_respects_path_component_boundary() {
    let (dir, bin) = common::setup_test_project("worktree_prefix");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "WP"]);
    common::run_cmd(
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

    let mut task_ids = Vec::new();
    for title in ["first", "second"] {
        let out = common::run_cmd(&dir, &bin, &["task", "create", "--title", title, "--json"]);
        assert!(out.status.success(), "create {title} failed");
        let value: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid envelope");
        task_ids.push(value["data"]["id"].as_str().expect("task id").to_string());
    }
    // data.id may be absent depending on the envelope shape; fall back to display ids.
    if task_ids.iter().any(|t| t.is_empty()) {
        let list = common::run_cmd(&dir, &bin, &["task", "list", "--json"]);
        let value: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
        task_ids = value["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_str().unwrap().to_string())
            .collect();
    }

    let wt_first = dir.join("wt");
    let wt_second = dir.join("wt-x");

    // Bind order matters for regression fidelity: worktree listings are
    // `ORDER BY bound_at DESC`, so binding the PREFIX-COLLIDING worktree
    // ("wt" -> first task) LAST puts it first in the resolver's candidate
    // list. The old string-prefix match then picked the wrong task.
    let mk2 = common::run_cmd(
        &dir,
        &bin,
        &[
            "worktree",
            "create",
            &task_ids[1],
            "--path",
            wt_second.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(
        mk2.status.success(),
        "worktree create 2 failed: {} {}",
        String::from_utf8_lossy(&mk2.stdout),
        String::from_utf8_lossy(&mk2.stderr)
    );
    let mk1 = common::run_cmd(
        &dir,
        &bin,
        &[
            "worktree",
            "create",
            &task_ids[0],
            "--path",
            wt_first.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(
        mk1.status.success(),
        "worktree create 1 failed: {} {}",
        String::from_utf8_lossy(&mk1.stdout),
        String::from_utf8_lossy(&mk1.stderr)
    );

    // From inside wt-x, cwd-based task resolution must bind to the SECOND
    // task; the string-prefix bug resolved it to the first one because
    // "/…/wt-x".starts_with("/…/wt") is true.
    let out = std::process::Command::new(&bin)
        .args([
            "handoff",
            "create",
            "--target",
            "tester",
            "--summary",
            "cwd resolution probe",
            "--json",
        ])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(&wt_second)
        .output()
        .expect("handoff create should execute");
    assert!(
        out.status.success(),
        "handoff create from wt-x failed: {} {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let resolved_task = value["data"]["task_id"].as_str().expect("task_id");
    assert_eq!(
        resolved_task, task_ids[1],
        "cwd inside wt-x must resolve to the second task, not the prefix-colliding first"
    );
}

// ── CTX-0083: `worktree remove` ─────────────────────────────────────────

fn setup_remove_fixture(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let (dir, bin) = common::setup_test_project(name);
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "RM"]);
    common::run_cmd(
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
    (dir, bin)
}

fn json_envelope(out: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).expect("valid JSON envelope on stdout")
}

fn error_envelope(out: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&out.stderr).expect("valid JSON error envelope on stderr")
}

fn worktree_rows(dir: &std::path::Path) -> i64 {
    let db = rusqlite::Connection::open(dir.join(".git/carryctx/state.sqlite")).unwrap();
    db.query_row("SELECT COUNT(*) FROM worktrees", [], |row| row.get(0))
        .unwrap()
}

fn event_count(dir: &std::path::Path, event_type: &str) -> i64 {
    let db = rusqlite::Connection::open(dir.join(".git/carryctx/state.sqlite")).unwrap();
    db.query_row(
        "SELECT COUNT(*) FROM events WHERE type = ?1",
        [event_type],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn test_worktree_remove_deletes_live_clean_worktree_by_path_and_display_id() {
    let (dir, bin) = setup_remove_fixture("worktree_remove_live");

    // Two bound worktrees: one removed via relative path, one via task
    // display id.
    for title in ["first", "second"] {
        let out = common::run_cmd(&dir, &bin, &["task", "create", "--title", title]);
        assert!(out.status.success(), "task create failed");
    }
    let mk1 = common::run_cmd(&dir, &bin, &["worktree", "create", "RM-0001", "--json"]);
    assert!(
        mk1.status.success(),
        "create 1 failed: {}",
        String::from_utf8_lossy(&mk1.stderr)
    );
    let mk2 = common::run_cmd(&dir, &bin, &["worktree", "create", "RM-0002", "--json"]);
    assert!(
        mk2.status.success(),
        "create 2 failed: {}",
        String::from_utf8_lossy(&mk2.stderr)
    );
    assert_eq!(worktree_rows(&dir), 2);

    // Remove the first by relative path: directory and registration both go.
    let wt1 = dir.join(".worktrees/rm-0001");
    assert!(wt1.exists());
    let removed = common::run_cmd(
        &dir,
        &bin,
        &[
            "--format",
            "json",
            "worktree",
            "remove",
            ".worktrees/rm-0001",
        ],
    );
    assert!(
        removed.status.success(),
        "remove failed: {} {}",
        String::from_utf8_lossy(&removed.stdout),
        String::from_utf8_lossy(&removed.stderr)
    );
    let value = json_envelope(&removed);
    assert_eq!(value["success"], true);
    assert!(!wt1.exists(), "live worktree directory must be deleted");

    // Remove the second by task display id (CTX-style ref resolution).
    let wt2 = dir.join(".worktrees/rm-0002");
    let removed2 = common::run_cmd(
        &dir,
        &bin,
        &["--format", "json", "worktree", "remove", "RM-0002"],
    );
    assert!(
        removed2.status.success(),
        "remove by display id failed: {} {}",
        String::from_utf8_lossy(&removed2.stdout),
        String::from_utf8_lossy(&removed2.stderr)
    );
    assert!(!wt2.exists());

    // Registrations are gone entirely; an audit event was appended per repo
    // convention.
    assert_eq!(worktree_rows(&dir), 0, "registrations must be deleted");
    assert_eq!(event_count(&dir, "worktree.removed"), 2);
}

#[test]
fn test_worktree_remove_refuses_dirty_worktree_unless_forced() {
    let (dir, bin) = setup_remove_fixture("worktree_remove_dirty");
    common::run_cmd(&dir, &bin, &["task", "create", "--title", "dirty"]);
    let created = common::run_cmd(&dir, &bin, &["worktree", "create", "RM-0001", "--json"]);
    assert!(created.status.success());

    let wt = dir.join(".worktrees/rm-0001");
    std::fs::write(wt.join("untracked.txt"), "local edits").unwrap();

    // Mirrors git's own guard: dirty/untracked → refusal with a documented rc.
    let refused = common::run_cmd(
        &dir,
        &bin,
        &[
            "--format",
            "json",
            "worktree",
            "remove",
            ".worktrees/rm-0001",
        ],
    );
    assert!(
        !refused.status.success(),
        "dirty worktree removal must be refused"
    );
    assert_eq!(refused.status.code(), Some(3), "rc must be STATE_CONFLICT");
    let err = error_envelope(&refused);
    assert_eq!(err["error"]["code"], "STATE_CONFLICT");
    assert!(
        err["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("--force"),
        "refusal must point at --force"
    );
    assert!(
        wt.exists(),
        "refused removal must keep the worktree directory"
    );
    assert_eq!(worktree_rows(&dir), 1);

    // --force removes anyway.
    let forced = common::run_cmd(
        &dir,
        &bin,
        &[
            "--format",
            "json",
            "worktree",
            "remove",
            ".worktrees/rm-0001",
            "--force",
        ],
    );
    assert!(
        forced.status.success(),
        "forced remove failed: {} {}",
        String::from_utf8_lossy(&forced.stdout),
        String::from_utf8_lossy(&forced.stderr)
    );
    assert!(!wt.exists());
    assert_eq!(worktree_rows(&dir), 0);
    assert_eq!(event_count(&dir, "worktree.removed"), 1);
}

#[test]
fn test_worktree_remove_refuses_live_jj_colocated_worktree_even_when_forced() {
    let (dir, bin) = setup_remove_fixture("worktree_remove_jj");
    common::run_cmd(&dir, &bin, &["task", "create", "--title", "jj removal"]);
    let created = common::run_cmd(&dir, &bin, &["worktree", "create", "RM-0001", "--json"]);
    assert!(created.status.success());
    let wt = dir.join(".worktrees/rm-0001");
    std::fs::create_dir(dir.join(".jj")).unwrap();

    let refused = common::run_cmd(
        &dir,
        &bin,
        &[
            "--format",
            "json",
            "worktree",
            "remove",
            ".worktrees/rm-0001",
            "--force",
        ],
    );
    assert!(!refused.status.success());
    let error = error_envelope(&refused);
    assert_eq!(error["success"], false);
    assert_eq!(error["error"]["code"], "VALIDATION_FAILED");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("jj-colocated")
    );
    assert!(wt.exists());
    assert_eq!(worktree_rows(&dir), 1);
}

#[test]
fn test_worktree_remove_orphaned_registration_when_directory_is_gone() {
    let (dir, bin) = setup_remove_fixture("worktree_remove_orphan");
    common::run_cmd(&dir, &bin, &["task", "create", "--title", "orphan"]);
    let created = common::run_cmd(&dir, &bin, &["worktree", "create", "RM-0001", "--json"]);
    assert!(created.status.success());

    std::fs::remove_dir_all(dir.join(".worktrees/rm-0001")).unwrap();

    let removed = common::run_cmd(
        &dir,
        &bin,
        &["--format", "json", "worktree", "remove", "RM-0001"],
    );
    assert!(
        removed.status.success(),
        "orphan cleanup failed: {} {}",
        String::from_utf8_lossy(&removed.stdout),
        String::from_utf8_lossy(&removed.stderr)
    );
    assert_eq!(worktree_rows(&dir), 0);

    // Doctor no longer warns about stale registrations afterwards.
    let doctor = common::run_cmd(&dir, &bin, &["doctor", "--json"]);
    assert!(
        doctor.status.success(),
        "doctor should be clean after orphan cleanup"
    );
}

#[test]
fn test_worktree_remove_accepts_ulid_and_rejects_unknown_refs() {
    let (dir, bin) = setup_remove_fixture("worktree_remove_ulid");
    common::run_cmd(&dir, &bin, &["task", "create", "--title", "ulid"]);
    let created = common::run_cmd(&dir, &bin, &["worktree", "create", "RM-0001", "--json"]);
    assert!(created.status.success());

    let ulid: String = {
        let db = rusqlite::Connection::open(dir.join(".git/carryctx/state.sqlite")).unwrap();
        db.query_row("SELECT id FROM worktrees LIMIT 1", [], |row| row.get(0))
            .unwrap()
    };

    let unknown = common::run_cmd(
        &dir,
        &bin,
        &["--format", "json", "worktree", "remove", ".worktrees/nope"],
    );
    assert!(!unknown.status.success());
    assert_eq!(
        unknown.status.code(),
        Some(7),
        "rc must be RESOURCE_NOT_FOUND"
    );
    let err = error_envelope(&unknown);
    assert_eq!(err["error"]["code"], "RESOURCE_NOT_FOUND");
    assert_eq!(event_count(&dir, "worktree.removed"), 0);

    let removed = common::run_cmd(
        &dir,
        &bin,
        &["--format", "json", "worktree", "remove", &ulid],
    );
    assert!(
        removed.status.success(),
        "remove by ULID failed: {} {}",
        String::from_utf8_lossy(&removed.stdout),
        String::from_utf8_lossy(&removed.stderr)
    );
    assert_eq!(worktree_rows(&dir), 0);
}
