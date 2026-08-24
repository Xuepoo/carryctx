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

#[test]
fn test_admission_lock_exactly_one_winner_across_threads() {
    use carryctx::adapter::filesystem::AdmissionLock;
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const CONTENDERS: usize = 8;
    let root = tempfile::tempdir().unwrap();
    let lock = root.path().join("command.lock");
    let barrier = Barrier::new(CONTENDERS);
    let winners = AtomicUsize::new(0);
    let finished_losers = AtomicUsize::new(0);

    std::thread::scope(|scope| {
        for i in 0..CONTENDERS {
            let barrier = &barrier;
            let winners = &winners;
            let finished_losers = &finished_losers;
            let lock_path = lock.clone();
            scope.spawn(move || {
                barrier.wait();
                match AdmissionLock::acquire(
                    &lock_path,
                    &format!("racer-{i}"),
                    std::process::id(),
                    "test",
                    "now",
                ) {
                    Ok(guard) => {
                        winners.fetch_add(1, Ordering::SeqCst);
                        // Keep holding until every contender has made its
                        // single attempt so a late scheduler cannot acquire
                        // a second time after release.
                        let deadline =
                            std::time::Instant::now() + std::time::Duration::from_secs(10);
                        while finished_losers.load(Ordering::SeqCst) < CONTENDERS - 1 {
                            assert!(
                                std::time::Instant::now() < deadline,
                                "contenders failed to finish their attempts"
                            );
                            std::thread::sleep(std::time::Duration::from_millis(1));
                        }
                        drop(guard);
                    }
                    Err(e) if e.code == "STATE_CONFLICT" => {
                        finished_losers.fetch_add(1, Ordering::SeqCst);
                    }
                    Err(e) => panic!("unexpected error kind {}: {e}", e.code),
                }
            });
        }
    });

    assert_eq!(
        winners.load(Ordering::SeqCst),
        1,
        "exactly one contender must win while the holder keeps the lock"
    );
}
