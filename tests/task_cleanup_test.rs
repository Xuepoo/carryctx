mod common;

use carryctx_cli::adapter::filesystem::AdmissionLock;
use carryctx_cli::adapter::sqlite_repos::SqliteCleanupRepository;
use carryctx_cli::adapter::xdg::XdgPaths;
use carryctx_cli::domain::cleanup::{CleanupReason, CleanupState};
use carryctx_cli::repository::{CleanupRepository, NewCleanupRequest};
use common::{fixture_git, init_and_agent, run_cmd, setup_test_project};
use rusqlite::Connection;

fn create_started_task(dir: &std::path::Path, bin: &std::path::Path, title: &str) -> String {
    let created = run_cmd(dir, bin, &["--json", "task", "create", "--title", title]);
    assert!(created.status.success());
    let value: serde_json::Value = serde_json::from_slice(&created.stdout).unwrap();
    let task = value["data"]["display_id"].as_str().unwrap().to_string();
    assert!(
        run_cmd(dir, bin, &["task", "start", &task])
            .status
            .success()
    );
    task
}

fn state_db(dir: &std::path::Path) -> Connection {
    Connection::open(dir.join(".git/carryctx/state.sqlite")).unwrap()
}

fn create_bound_worktree(
    dir: &std::path::Path,
    bin: &std::path::Path,
    task: &str,
) -> std::path::PathBuf {
    let path = dir.parent().unwrap().join(format!(
        "{}-{task}-cleanup-test",
        dir.file_name().unwrap().to_string_lossy()
    ));
    let _ = std::fs::remove_dir_all(&path);
    let output = run_cmd(
        dir,
        bin,
        &["worktree", "create", task, "--path", path.to_str().unwrap()],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    path
}

fn cleanup_state(dir: &std::path::Path, task: &str) -> Option<(String, Option<String>, i64)> {
    state_db(dir).query_row(
        "SELECT c.state, c.blocked_reason, c.attempt_count FROM worktree_cleanup_requests c JOIN tasks t ON t.id = c.task_id WHERE t.display_id = ?1 ORDER BY c.requested_at DESC LIMIT 1",
        [task],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).ok()
}

#[test]
fn completion_removes_clean_bound_worktree_and_is_idempotent() {
    let (dir, bin) = setup_test_project("task_cleanup_clean");
    init_and_agent(&dir, &bin);
    let task = create_started_task(&dir, &bin, "clean cleanup");
    let path = create_bound_worktree(&dir, &bin, &task);
    let complete = run_cmd(&dir, &bin, &["task", "complete", &task]);
    assert!(complete.status.success());
    assert!(!path.exists());
    assert_eq!(
        cleanup_state(&dir, &task),
        Some(("completed".into(), None, 1))
    );
    let registration_count: i64 = state_db(&dir)
        .query_row("SELECT COUNT(*) FROM worktrees", [], |row| row.get(0))
        .unwrap();
    assert_eq!(registration_count, 0);
    assert!(
        !run_cmd(&dir, &bin, &["task", "complete", &task])
            .status
            .success()
    );
    let count: i64 = state_db(&dir)
        .query_row("SELECT COUNT(*) FROM worktree_cleanup_requests", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(count, 1);
    let events: i64 = state_db(&dir)
        .query_row(
            "SELECT COUNT(*) FROM events WHERE type IN ('worktree.cleanup_requested', 'worktree.removed')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(events, 2);
}

#[test]
fn reconciliation_recovers_running_removed_request_and_cas_allows_one_claimant() {
    let (dir, bin) = setup_test_project("task_cleanup_reconcile");
    init_and_agent(&dir, &bin);
    let task = create_started_task(&dir, &bin, "reconcile cleanup");
    let path = create_bound_worktree(&dir, &bin, &task);
    let complete = run_cmd(&dir, &bin, &["task", "complete", &task]);
    assert!(complete.status.success());
    assert!(!String::from_utf8_lossy(&complete.stderr).contains("cleanup"));
    assert!(!path.exists());

    let db = state_db(&dir);
    let project_id: String = db
        .query_row("SELECT id FROM projects LIMIT 1", [], |r| r.get(0))
        .unwrap();
    let task_id: String = db
        .query_row("SELECT id FROM tasks WHERE display_id = ?1", [&task], |r| {
            r.get(0)
        })
        .unwrap();
    let cleanup_id: String = db
        .query_row(
            "SELECT id FROM worktree_cleanup_requests LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    db.execute(
        "UPDATE worktree_cleanup_requests SET state='running' WHERE id=?1",
        [&cleanup_id],
    )
    .unwrap();
    drop(db);
    let db = state_db(&dir);
    let repo = SqliteCleanupRepository::new(&db);
    let request = repo.find_by_id(&project_id, &cleanup_id).unwrap().unwrap();
    let claimed = repo
        .claim_for_attempt(
            &cleanup_id,
            &project_id,
            request.state,
            request.last_attempt_at.as_deref(),
            "later",
        )
        .unwrap();
    assert!(claimed.is_some());
    let second = repo
        .claim_for_attempt(
            &cleanup_id,
            &project_id,
            CleanupState::Running,
            None,
            "later-2",
        )
        .unwrap();
    assert!(second.is_none());
    drop(repo);
    drop(db);

    let xdg = XdgPaths::new();
    let git_common: std::path::PathBuf = state_db(&dir)
        .query_row("SELECT git_common_dir FROM projects LIMIT 1", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap()
        .into();
    let lock = AdmissionLock::acquire(
        &xdg.admission_lock_dir(&git_common),
        "test-reconcile",
        std::process::id(),
        "test",
        "now",
    )
    .unwrap();
    let mut db = state_db(&dir);
    let warnings = carryctx_cli::application::cleanup::reconcile_pending_cleanup(
        &mut db,
        &project_id,
        &dir,
        None,
        &lock,
    )
    .unwrap();
    assert!(warnings.is_empty());
    let state: String = db
        .query_row(
            "SELECT state FROM worktree_cleanup_requests WHERE id=?1",
            [&cleanup_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(state, "completed");
    assert!(!task_id.is_empty());
}

#[test]
fn failed_cleanup_request_is_retryable() {
    let (dir, bin) = setup_test_project("task_cleanup_failed_retry");
    init_and_agent(&dir, &bin);
    let task = create_started_task(&dir, &bin, "failed retry cleanup");
    let path = create_bound_worktree(&dir, &bin, &task);
    assert!(
        run_cmd(&dir, &bin, &["task", "complete", &task])
            .status
            .success()
    );
    let db = state_db(&dir);
    let project_id: String = db
        .query_row("SELECT id FROM projects LIMIT 1", [], |r| r.get(0))
        .unwrap();
    let task_id: String = db
        .query_row("SELECT id FROM tasks WHERE display_id=?1", [&task], |r| {
            r.get(0)
        })
        .unwrap();
    let request = NewCleanupRequest {
        id: "retry-request".into(),
        project_id: project_id.clone(),
        worktree_id: None,
        worktree_path: path.to_string_lossy().into(),
        branch: None,
        task_id: Some(task_id.clone()),
        reason: CleanupReason::TaskCompleted,
        requested_at: "later".into(),
    };
    SqliteCleanupRepository::new(&db).create(&request).unwrap();
    db.execute(
        "UPDATE worktree_cleanup_requests SET state='failed' WHERE id='retry-request'",
        [],
    )
    .unwrap();
    let repo = SqliteCleanupRepository::new(&db);
    let claimed = repo
        .claim_for_attempt(
            "retry-request",
            &project_id,
            CleanupState::Failed,
            None,
            "retry",
        )
        .unwrap();
    assert_eq!(claimed.unwrap().state, CleanupState::Running);
    drop(repo);
    drop(db);
    let git_common: std::path::PathBuf = state_db(&dir)
        .query_row("SELECT git_common_dir FROM projects LIMIT 1", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap()
        .into();
    let xdg = XdgPaths::new();
    let lock = AdmissionLock::acquire(
        &xdg.admission_lock_dir(&git_common),
        "test-failed-retry",
        std::process::id(),
        "test",
        "now",
    )
    .unwrap();
    let mut db = state_db(&dir);
    let warnings = carryctx_cli::application::cleanup::reconcile_pending_cleanup(
        &mut db,
        &project_id,
        &dir,
        None,
        &lock,
    )
    .unwrap();
    assert!(warnings.is_empty());
    let state: String = db
        .query_row(
            "SELECT state FROM worktree_cleanup_requests WHERE id='retry-request'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(state, "completed");
}

#[test]
fn reconciliation_processes_taskless_manual_request() {
    let (dir, bin) = setup_test_project("task_cleanup_manual");
    init_and_agent(&dir, &bin);
    let db = state_db(&dir);
    let project_id: String = db
        .query_row("SELECT id FROM projects LIMIT 1", [], |r| r.get(0))
        .unwrap();
    let request = NewCleanupRequest {
        id: "manual-reconcile".into(),
        project_id: project_id.clone(),
        worktree_id: None,
        worktree_path: dir.join("already-gone").to_string_lossy().into(),
        branch: Some("manual-branch".into()),
        task_id: None,
        reason: CleanupReason::Manual,
        requested_at: "now".into(),
    };
    SqliteCleanupRepository::new(&db).create(&request).unwrap();
    drop(db);
    let git_common: std::path::PathBuf = state_db(&dir)
        .query_row("SELECT git_common_dir FROM projects LIMIT 1", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap()
        .into();
    let xdg = XdgPaths::new();
    let lock = AdmissionLock::acquire(
        &xdg.admission_lock_dir(&git_common),
        "manual-reconcile",
        std::process::id(),
        "test",
        "now",
    )
    .unwrap();
    let mut db = state_db(&dir);
    let warnings = carryctx_cli::application::cleanup::reconcile_pending_cleanup(
        &mut db,
        &project_id,
        &dir,
        None,
        &lock,
    )
    .unwrap();
    assert!(warnings.is_empty());
    let state: String = db
        .query_row(
            "SELECT state FROM worktree_cleanup_requests WHERE id='manual-reconcile'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(state, "completed");
}

#[test]
fn exact_request_reconciliation_does_not_select_same_task_sibling() {
    let (dir, bin) = setup_test_project("task_cleanup_exact_request");
    init_and_agent(&dir, &bin);
    let task = create_started_task(&dir, &bin, "exact request cleanup");
    let path = create_bound_worktree(&dir, &bin, &task);
    let db = state_db(&dir);
    let project_id: String = db
        .query_row("SELECT id FROM projects LIMIT 1", [], |row| row.get(0))
        .unwrap();
    let task_id: String = db
        .query_row("SELECT id FROM tasks WHERE display_id=?1", [&task], |row| {
            row.get(0)
        })
        .unwrap();
    let first_id = "exact-first";
    let second_id = "exact-second";
    let repo = SqliteCleanupRepository::new(&db);
    repo.create(&NewCleanupRequest {
        id: first_id.into(),
        project_id: project_id.clone(),
        worktree_id: None,
        worktree_path: dir.join("missing-first").to_string_lossy().into(),
        branch: None,
        task_id: Some(task_id.clone()),
        reason: CleanupReason::Manual,
        requested_at: "one".into(),
    })
    .unwrap();
    repo.create(&NewCleanupRequest {
        id: second_id.into(),
        project_id: project_id.clone(),
        worktree_id: None,
        worktree_path: path.to_string_lossy().into(),
        branch: None,
        task_id: Some(task_id),
        reason: CleanupReason::Manual,
        requested_at: "two".into(),
    })
    .unwrap();
    drop(repo);
    drop(db);
    let git_common: std::path::PathBuf = state_db(&dir)
        .query_row("SELECT git_common_dir FROM projects LIMIT 1", [], |row| {
            row.get::<_, String>(0)
        })
        .unwrap()
        .into();
    let lock = AdmissionLock::acquire(
        &XdgPaths::new().admission_lock_dir(&git_common),
        "exact-request",
        std::process::id(),
        "test",
        "now",
    )
    .unwrap();
    let mut db = state_db(&dir);
    carryctx_cli::application::cleanup::try_cleanup_request(
        &mut db,
        &project_id,
        first_id,
        &dir,
        None,
        &lock,
    )
    .unwrap();
    let first_state: String = db
        .query_row(
            "SELECT state FROM worktree_cleanup_requests WHERE id=?1",
            [first_id],
            |row| row.get(0),
        )
        .unwrap();
    let second_state: String = db
        .query_row(
            "SELECT state FROM worktree_cleanup_requests WHERE id=?1",
            [second_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(first_state, "completed");
    assert_eq!(second_state, "pending");
}

#[test]
fn session_cleanup_prefers_exact_worktree_over_same_task_siblings() {
    let (dir, bin) = setup_test_project("session_cleanup_scope");
    init_and_agent(&dir, &bin);
    let task = create_started_task(&dir, &bin, "scoped session cleanup");
    let first = create_bound_worktree(&dir, &bin, &task);
    let sibling_task = create_started_task(&dir, &bin, "scoped session sibling");
    let second = dir.parent().unwrap().join("session-cleanup-sibling");
    let _ = std::fs::remove_dir_all(&second);
    assert!(
        run_cmd(
            &dir,
            &bin,
            &[
                "worktree",
                "create",
                &sibling_task,
                "--path",
                second.to_str().unwrap()
            ]
        )
        .status
        .success()
    );
    let db = state_db(&dir);
    let project_id: String = db
        .query_row("SELECT id FROM projects LIMIT 1", [], |r| r.get(0))
        .unwrap();
    let task_id: String = db
        .query_row("SELECT id FROM tasks WHERE display_id=?1", [&task], |r| {
            r.get(0)
        })
        .unwrap();
    let ids: Vec<(String, String)> = {
        let mut stmt = db
            .prepare("SELECT id, normalized_path FROM worktrees ORDER BY normalized_path")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let repo = SqliteCleanupRepository::new(&db);
    let first_id = ids[0].0.clone();
    repo.create(&NewCleanupRequest {
        id: "scoped-first".into(),
        project_id: project_id.clone(),
        worktree_id: Some(first_id.clone()),
        worktree_path: first.to_string_lossy().into(),
        branch: None,
        task_id: Some(task_id.clone()),
        reason: CleanupReason::Manual,
        requested_at: "one".into(),
    })
    .unwrap();
    let sibling_id: String = db
        .query_row(
            "SELECT id FROM worktrees WHERE normalized_path=?1",
            [second.to_string_lossy()],
            |r| r.get(0),
        )
        .unwrap();
    repo.create(&NewCleanupRequest {
        id: "scoped-sibling".into(),
        project_id: project_id.clone(),
        worktree_id: Some(sibling_id),
        worktree_path: second.to_string_lossy().into(),
        branch: None,
        task_id: Some(task_id.clone()),
        reason: CleanupReason::Manual,
        requested_at: "two".into(),
    })
    .unwrap();
    drop(repo);
    let session_worktree_id = first_id;
    drop(db);
    assert!(
        run_cmd(
            &dir,
            &bin,
            &[
                "--non-interactive",
                "checkpoint",
                "--task",
                &task,
                "--no-git"
            ]
        )
        .status
        .success()
    );
    assert!(
        run_cmd(
            &dir,
            &bin,
            &[
                "session",
                "start",
                "--task",
                &task,
                "--worktree",
                &session_worktree_id
            ]
        )
        .status
        .success()
    );
    let end = run_cmd(
        &dir,
        &bin,
        &["--json", "--non-interactive", "session", "end"],
    );
    assert!(
        end.status.success(),
        "{}",
        String::from_utf8_lossy(&end.stderr)
    );
    let db = state_db(&dir);
    let states: Vec<(String, String)> = db
        .prepare("SELECT id, state FROM worktree_cleanup_requests ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        states,
        vec![
            ("scoped-first".into(), "completed".into()),
            ("scoped-sibling".into(), "pending".into())
        ]
    );
    assert!(second.exists());
}

#[test]
fn completion_blocks_cleanup_for_active_session() {
    let (dir, bin) = setup_test_project("task_cleanup_session");
    init_and_agent(&dir, &bin);
    let task = create_started_task(&dir, &bin, "session cleanup");
    let path = create_bound_worktree(&dir, &bin, &task);
    let db = state_db(&dir);
    db.execute("INSERT INTO sessions (id, project_id, agent_id, task_id, worktree_id, state, provider, working_directory, metadata_json, started_at, last_activity_at, updated_at) SELECT 'cleanup-session', w.project_id, a.id, NULL, w.id, 'active', 'test', w.normalized_path, '{}', 'now', 'now', 'now' FROM worktrees w JOIN agents a ON a.project_id=w.project_id WHERE w.task_id=(SELECT id FROM tasks WHERE display_id=?1) LIMIT 1", [&task]).unwrap();
    drop(db);
    let complete = run_cmd(&dir, &bin, &["task", "complete", &task]);
    assert!(complete.status.success());
    assert!(path.exists());
    assert!(String::from_utf8_lossy(&complete.stderr).contains("active_session"));
    let (state, blocker, _) = cleanup_state(&dir, &task).unwrap();
    assert_eq!(state, "blocked");
    assert_eq!(blocker.as_deref(), Some("active_session:cleanup-session"));
}

#[test]
fn completion_blocks_cleanup_for_dirty_worktree() {
    let (dir, bin) = setup_test_project("task_cleanup_dirty");
    init_and_agent(&dir, &bin);
    let task = create_started_task(&dir, &bin, "dirty cleanup");
    let path = create_bound_worktree(&dir, &bin, &task);
    std::fs::write(path.join("dirty.txt"), "dirty\n").unwrap();
    let complete = run_cmd(&dir, &bin, &["--json", "task", "complete", &task]);
    assert!(complete.status.success());
    assert!(path.exists());
    let envelope: serde_json::Value = serde_json::from_slice(&complete.stdout).unwrap();
    assert_eq!(envelope["success"], true);
    assert!(
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning
                .as_str()
                .unwrap_or_default()
                .contains("dirty_worktree"))
    );
    assert_eq!(
        cleanup_state(&dir, &task).unwrap().1.as_deref(),
        Some("dirty_worktree")
    );
    assert!(
        run_cmd(&dir, &bin, &["task", "reopen", &task])
            .status
            .success()
    );
    assert!(
        run_cmd(&dir, &bin, &["task", "start", &task])
            .status
            .success()
    );
    assert!(
        run_cmd(&dir, &bin, &["task", "complete", &task])
            .status
            .success()
    );
    assert_eq!(cleanup_state(&dir, &task).unwrap().2, 2);
    let request_count: i64 = state_db(&dir)
        .query_row(
            "SELECT COUNT(*) FROM worktree_cleanup_requests",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(request_count, 1);
    fixture_git(
        &dir,
        &["worktree", "remove", "--force", path.to_str().unwrap()],
    );
}

#[test]
fn completion_refuses_git_removal_in_jj_colocated_repository() {
    let (dir, bin) = setup_test_project("task_cleanup_jj_colocation");
    init_and_agent(&dir, &bin);
    let task = create_started_task(&dir, &bin, "jj cleanup");
    let path = create_bound_worktree(&dir, &bin, &task);
    std::fs::create_dir(dir.join(".jj")).unwrap();

    let complete = run_cmd(&dir, &bin, &["--json", "task", "complete", &task]);
    assert!(complete.status.success());
    assert!(
        path.exists(),
        "jj-colocated cleanup must not remove the worktree"
    );
    let envelope: serde_json::Value = serde_json::from_slice(&complete.stdout).unwrap();
    assert!(
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| {
                warning
                    .as_str()
                    .unwrap_or_default()
                    .contains("jj_colocation")
            })
    );
    assert_eq!(cleanup_state(&dir, &task).unwrap().0, "blocked");
    assert_eq!(
        cleanup_state(&dir, &task).unwrap().1.as_deref(),
        Some("jj_colocation")
    );

    let retried = run_cmd(&dir, &bin, &["worktree", "cleanup", "run", &task]);
    assert!(retried.status.success());
    assert!(String::from_utf8_lossy(&retried.stderr).contains("jj_colocation"));
    assert!(path.exists());
}

#[test]
fn completion_without_worktree_creates_no_cleanup_request() {
    let (dir, bin) = setup_test_project("task_cleanup_none");
    init_and_agent(&dir, &bin);
    let task = create_started_task(&dir, &bin, "no cleanup");
    assert!(
        run_cmd(&dir, &bin, &["task", "complete", &task])
            .status
            .success()
    );
    assert_eq!(cleanup_state(&dir, &task), None);
}

#[test]
fn explicit_terminal_cleanup_run_is_rejected() {
    let (dir, bin) = setup_test_project("task_cleanup_terminal_run");
    init_and_agent(&dir, &bin);
    let task = create_started_task(&dir, &bin, "terminal cleanup run");
    let path = create_bound_worktree(&dir, &bin, &task);
    assert!(
        run_cmd(&dir, &bin, &["task", "complete", &task])
            .status
            .success()
    );
    assert!(!path.exists());

    let request_id: String = state_db(&dir)
        .query_row(
            "SELECT id FROM worktree_cleanup_requests WHERE task_id=(SELECT id FROM tasks WHERE display_id=?1)",
            [&task],
            |row| row.get(0),
        )
        .unwrap();
    let output = run_cmd(
        &dir,
        &bin,
        &["--json", "worktree", "cleanup", "run", &request_id],
    );
    assert!(!output.status.success());
    let envelope: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(envelope["success"], false);
    assert_eq!(envelope["error"]["code"], "STATE_CONFLICT");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("not retryable")
    );
}

#[test]
fn task_reference_cleanup_prefers_retryable_request_over_completed_sibling() {
    let (dir, bin) = setup_test_project("task_cleanup_retryable_sibling");
    init_and_agent(&dir, &bin);
    let task = create_started_task(&dir, &bin, "retryable sibling");
    let path = create_bound_worktree(&dir, &bin, &task);
    assert!(
        run_cmd(&dir, &bin, &["task", "complete", &task])
            .status
            .success()
    );
    assert!(!path.exists());

    let db = state_db(&dir);
    let project_id: String = db
        .query_row("SELECT id FROM projects LIMIT 1", [], |row| row.get(0))
        .unwrap();
    let task_id: String = db
        .query_row("SELECT id FROM tasks WHERE display_id=?1", [&task], |row| {
            row.get(0)
        })
        .unwrap();
    SqliteCleanupRepository::new(&db)
        .create(&NewCleanupRequest {
            id: "retryable-sibling".into(),
            project_id: project_id.clone(),
            worktree_id: None,
            worktree_path: path.to_string_lossy().into(),
            branch: None,
            task_id: Some(task_id),
            reason: CleanupReason::Manual,
            requested_at: "later".into(),
        })
        .unwrap();
    drop(db);

    let output = run_cmd(&dir, &bin, &["--json", "worktree", "cleanup", "run", &task]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let state: String = state_db(&dir)
        .query_row(
            "SELECT state FROM worktree_cleanup_requests WHERE id='retryable-sibling'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(state, "completed");
}
