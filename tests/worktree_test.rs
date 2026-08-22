mod common;

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
    assert!(!doctor.status.success());
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

    let no_mutation = common::run_cmd(&dir, &bin, &["doctor", "--json"]);
    assert!(!no_mutation.status.success());

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
