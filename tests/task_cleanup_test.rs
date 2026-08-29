mod common;

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
            "SELECT COUNT(*) FROM events WHERE type IN ('worktree.cleanup_requested', 'worktree.cleanup_completed')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(events, 2);
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
    assert!(
        run_cmd(&dir, &bin, &["task", "complete", &task])
            .status
            .success()
    );
    assert!(path.exists());
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
    assert!(
        run_cmd(&dir, &bin, &["task", "complete", &task])
            .status
            .success()
    );
    assert!(path.exists());
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
