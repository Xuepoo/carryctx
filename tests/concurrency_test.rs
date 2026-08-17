mod common;

use std::sync::Arc;
use std::thread;

#[test]
fn test_concurrent_task_creation() {
    let (dir, bin) = common::setup_test_project("concurrent_writes_test");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "CONC"]);
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

    let dir_arc = Arc::new(dir);
    let bin_arc = Arc::new(bin);
    let num_tasks = 8;
    let mut handles = vec![];

    for i in 0..num_tasks {
        let d = Arc::clone(&dir_arc);
        let b = Arc::clone(&bin_arc);
        handles.push(thread::spawn(move || {
            let title = format!("Concurrent task {i}");
            let out = std::process::Command::new(&*b)
                .args(["task", "create", "--title", &title, "--agent", "tester"])
                .current_dir(&*d)
                .output()
                .unwrap();
            (i, out)
        }));
    }

    let mut failed = vec![];
    for handle in handles {
        let (i, out) = handle.join().unwrap();
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            failed.push(format!("Task {i} failed: {stderr}"));
        }
    }

    assert!(
        failed.is_empty(),
        "Concurrent writes should not fail with database locked: {:?}",
        failed
    );

    let list = common::run_cmd(&dir_arc, &bin_arc, &["task", "list", "--json"]);
    assert!(list.status.success());
    let list_val: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    let tasks = list_val["data"].as_array().unwrap();
    assert_eq!(
        tasks.len(),
        num_tasks,
        "All concurrent tasks should be present in database"
    );
}

#[test]
fn test_owner_alias_for_agent_flag() {
    let (dir, bin) = common::setup_test_project("owner_alias_test");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "OWN"]);
    common::run_cmd(
        &dir,
        &bin,
        &["agent", "register", "--name", "alice", "--provider", "test"],
    );

    common::run_cmd(
        &dir,
        &bin,
        &[
            "task",
            "create",
            "--title",
            "Owner test task",
            "--agent",
            "alice",
        ],
    );

    // Use --owner instead of --agent for writing a decision
    let dec = std::process::Command::new(&bin)
        .args([
            "decision",
            "add",
            "--owner",
            "alice",
            "--title",
            "Owner test dec",
            "--task",
            "OWN-0001",
            "--json",
        ])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        dec.status.success(),
        "decision add with --owner alias should succeed: {}",
        String::from_utf8_lossy(&dec.stderr)
    );
    let dec_val: serde_json::Value = serde_json::from_slice(&dec.stdout).unwrap();
    assert!(dec_val["data"]["created_by_agent"].as_str().is_some());
}
