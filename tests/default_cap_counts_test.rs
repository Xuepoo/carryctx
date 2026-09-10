//! CTX-0080: `status` and `doctor` read task totals through the capped
//! `task list` (default 200 rows), so projects with more tasks under-report
//! counts and skip diagnostics for rows outside the cap window. Totals must
//! come from COUNT(*)-style queries instead of listing.

mod common;

use carryctx_cli::adapter::sqlite::ProjectDatabase;
use carryctx_cli::adapter::unit_of_work::UnitOfWork;
use carryctx_cli::domain::task::{TaskPriority, TaskStatus};
use carryctx_cli::repository::task::{NewTask, TaskRepository};
use serde_json::Value;
use std::process::Command;

fn run(dir: &std::path::Path, bin: &std::path::Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .output()
        .expect("command should execute");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), combined)
}

/// Seed 200 fresh unowned tasks plus 3 old `in_progress` tasks owned by a
/// live agent. The old rows sort last under `created_at DESC` and fall
/// outside the default cap window — exactly where the under-count hid.
fn seed_tasks_beyond_cap(dir: &std::path::Path) {
    let db_path = dir.join(".git/carryctx/state.sqlite");
    let mut db = ProjectDatabase::open(&db_path).unwrap();
    let project_id: String = db
        .connection()
        .query_row("SELECT id FROM projects LIMIT 1", [], |row| row.get(0))
        .unwrap();
    let owner_id: String = db
        .connection()
        .query_row("SELECT id FROM agents WHERE name = 'tester'", [], |row| {
            row.get(0)
        })
        .unwrap();

    let conn = db.connection_mut();
    let uow = UnitOfWork::begin(conn).unwrap();
    let repo = carryctx_cli::adapter::sqlite_repos::SqliteTaskRepository::new(uow.connection());

    let new_task = |i: usize| NewTask {
        id: format!("task-{i:04}"),
        project_id: project_id.clone(),
        display_id: format!("{:04}", 1000 + i),
        title: format!("seed {i}"),
        description: None,
        status: TaskStatus::Planned,
        priority: TaskPriority::Normal,
        owner_agent_id: None,
        parent_task_id: None,
        required_role: None,
        team_id: None,
    };

    // Oldest rows first: they fall outside the newest-200 cap window once
    // the filler rows exist.
    for i in 0..3 {
        repo.create(
            &NewTask {
                owner_agent_id: Some(owner_id.clone()),
                ..new_task(i)
            },
            "2026-08-20T09:00:00+00:00",
        )
        .unwrap();
        repo.update_status(
            &format!("task-{i:04}"),
            &project_id,
            TaskStatus::InProgress,
            Some(owner_id.clone()),
            "2026-08-20T09:05:00+00:00",
        )
        .unwrap();
    }
    for i in 3..203 {
        repo.create(&new_task(i), "2026-08-24T10:00:00+00:00")
            .unwrap();
    }
    uow.commit().unwrap();
}

#[test]
fn status_and_doctor_count_beyond_the_default_cap() {
    let (dir, bin) = common::setup_test_project("cap_counts");
    common::init_and_agent(&dir, &bin);
    seed_tasks_beyond_cap(&dir);

    // Status markdown must report the true total (203), not the cap (200).
    let (ok, out) = run(&dir, &bin, &["--format", "markdown", "status"]);
    assert!(ok, "status should succeed: {out}");
    let total_line = out.lines().find(|l| l.contains("Total Tasks"));
    assert_eq!(
        total_line,
        Some("- **Total Tasks**: 203"),
        "Total Tasks must count beyond the cap"
    );

    // Status JSON carries the same total alongside the capped page array.
    let (ok, out) = run(&dir, &bin, &["--format", "json", "status"]);
    assert!(ok, "status --format json should succeed: {out}");
    let value: Value = serde_json::from_str(out.trim_start()).expect("valid json envelope");
    assert_eq!(value["data"]["totalTasks"], 203);
    assert_eq!(
        value["data"]["tasks"].as_array().map(|a| a.len()),
        Some(200),
        "the tasks array remains the capped page"
    );

    // Doctor must see the 3 in-progress tasks that the capped listing used
    // to miss entirely, and report no orphans when every owner exists.
    let (ok, out) = run(&dir, &bin, &["--format", "json", "doctor"]);
    assert!(ok, "doctor should succeed: {out}");
    let value: Value = serde_json::from_str(out.trim_start()).expect("valid json envelope");
    let checks = value["data"]["checks"].as_array().expect("checks array");
    let find = |name: &str| {
        checks
            .iter()
            .find(|c| c["check"] == name)
            .unwrap_or_else(|| panic!("missing check {name}: {checks:?}"))
            .clone()
    };
    let orphaned = find("tasks.orphaned");
    assert_eq!(orphaned["status"], "ok");
    let in_progress = find("tasks.in_progress");
    assert!(
        in_progress["message"]
            .as_str()
            .unwrap_or("")
            .contains("3 task(s)"),
        "in-progress count must reflect rows beyond the cap: {in_progress}"
    );
    assert_eq!(
        in_progress["tasks"].as_array().map(|a| a.len()),
        Some(3),
        "in-progress display ids must be complete"
    );
}
