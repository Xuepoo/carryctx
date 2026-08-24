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

/// CTX-0071 / issue #98: task claim was check-then-act with an unconditional
/// UPDATE, so two agents claiming the same ready task could both win
/// (last writer wins) and both log `task.claimed`. The guarded UPDATE must
/// yield exactly one winner.
#[test]
fn test_concurrent_task_claim_has_exactly_one_winner() {
    let (dir, bin) = common::setup_test_project("claim_race");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "RACE"]);

    // The default actor used by common::run_cmd.
    let out = common::run_cmd(
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
    assert!(out.status.success(), "register tester failed");

    const RACERS: usize = 6;
    for i in 0..RACERS {
        let out = common::run_cmd(
            &dir,
            &bin,
            &[
                "agent",
                "register",
                "--name",
                &format!("racer{i}"),
                "--provider",
                "test",
            ],
        );
        assert!(out.status.success(), "register racer{i} failed");
    }

    let created = common::run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", "Race task", "--json"],
    );
    assert!(created.status.success(), "task create failed");
    let stdout = String::from_utf8_lossy(&created.stdout);
    let start = stdout
        .find("\"display_id\":\"")
        .expect("display_id present")
        + 14;
    let end = stdout[start..].find('"').expect("closing quote") + start;
    let display_id = stdout[start..end].to_string();

    let dir_arc = Arc::new(dir);
    let bin_arc = Arc::new(bin);

    let handles: Vec<_> = (0..RACERS)
        .map(|i| {
            let d = Arc::clone(&dir_arc);
            let b = Arc::clone(&bin_arc);
            let tid = display_id.clone();
            thread::spawn(move || {
                std::process::Command::new(&*b)
                    .args(["task", "claim", &tid, "--json"])
                    .env("CARRYCTX_AGENT", format!("racer{i}"))
                    .current_dir(&*d)
                    .output()
                    .unwrap()
            })
        })
        .collect();

    let mut winners = 0;
    let mut losers = 0;
    for handle in handles {
        let out = handle.join().unwrap();
        if out.status.success() {
            winners += 1;
        } else {
            losers += 1;
        }
    }
    assert_eq!(
        winners, 1,
        "exactly one concurrent claim must win, got {winners}"
    );
    assert_eq!(losers, RACERS - 1, "all other claims must lose");

    // Exactly one claimed event and exactly one owner.
    let show = common::run_cmd(&dir_arc, &bin_arc, &["task", "show", &display_id, "--json"]);
    assert!(show.status.success());
    let value: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(value["data"]["status"], "in_progress");
    assert!(
        value["data"]["owner_agent_id"].is_string(),
        "winner must own the task: {value}"
    );

    // Read all events without an implicit actor filter (CARRYCTX_AGENT
    // narrows `event list` to that agent's own events).
    let events = std::process::Command::new(&*bin_arc)
        .args(["event", "list", "--limit", "200", "--json"])
        .env_remove("CARRYCTX_AGENT")
        .current_dir(&*dir_arc)
        .output()
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&events.stdout).unwrap();
    let claimed: Vec<_> = value["data"]["events"]
        .as_array()
        .expect("events array")
        .iter()
        .filter(|e| e["type"] == "task.claimed" || e["event_type"] == "task.claimed")
        .collect();
    assert_eq!(
        claimed.len(),
        1,
        "exactly one task.claimed event must be recorded: {value}"
    );
}

/// CTX-0071 / issue #98: handoff accept had no compare-and-set guard, so two
/// agents accepting the same pending handoff could both succeed. Exactly one
/// accept must win; the loser must get a state conflict.
#[test]
fn test_concurrent_handoff_accept_has_exactly_one_winner() {
    let (dir, bin) = common::setup_test_project("handoff_accept_race");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "HR"]);

    // The default actor used by common::run_cmd.
    let out = common::run_cmd(
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
    assert!(out.status.success(), "register tester failed");

    const ACCEPTORS: usize = 5;
    for i in 0..ACCEPTORS {
        let out = common::run_cmd(
            &dir,
            &bin,
            &[
                "agent",
                "register",
                "--name",
                &format!("acceptor{i}"),
                "--provider",
                "test",
            ],
        );
        assert!(out.status.success(), "register acceptor{i} failed");
    }

    let created = common::run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", "Handoff race", "--json"],
    );
    assert!(created.status.success());
    let stdout = String::from_utf8_lossy(&created.stdout);
    let start = stdout
        .find("\"display_id\":\"")
        .expect("display_id present")
        + 14;
    let end = stdout[start..].find('"').expect("closing quote") + start;
    let task_id = stdout[start..end].to_string();

    let created = common::run_cmd(
        &dir,
        &bin,
        &[
            "handoff",
            "create",
            "--target",
            "acceptor0",
            "--task",
            &task_id,
            "--summary",
            "race me",
            "--json",
        ],
    );
    assert!(created.status.success(), "handoff create failed");
    let stdout = String::from_utf8_lossy(&created.stdout);
    let key_at = stdout
        .find("\"display_id\":\"HO-")
        .expect("handoff display id")
        + 14;
    let end = stdout[key_at..].find('"').expect("closing quote") + key_at;
    let hid = stdout[key_at..end].to_string();

    let dir_arc = Arc::new(dir);
    let bin_arc = Arc::new(bin);

    let handles: Vec<_> = (0..ACCEPTORS)
        .map(|i| {
            let d = Arc::clone(&dir_arc);
            let b = Arc::clone(&bin_arc);
            let h = hid.clone();
            thread::spawn(move || {
                std::process::Command::new(&*b)
                    .args(["handoff", "accept", &h, "--json"])
                    .env("CARRYCTX_AGENT", format!("acceptor{i}"))
                    .current_dir(&*d)
                    .output()
                    .unwrap()
            })
        })
        .collect();

    let mut winners = 0;
    for handle in handles {
        let out = handle.join().unwrap();
        if out.status.success() {
            winners += 1;
        } else {
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(
                stderr.contains("STATE_CONFLICT"),
                "losing accept must report STATE_CONFLICT, got: {stderr}"
            );
        }
    }
    assert_eq!(
        winners, 1,
        "exactly one concurrent accept must win, got {winners}"
    );

    // Read all events without an implicit actor filter (CARRYCTX_AGENT
    // narrows `event list` to that agent's own events).
    let events = std::process::Command::new(&*bin_arc)
        .args(["event", "list", "--limit", "200", "--json"])
        .env_remove("CARRYCTX_AGENT")
        .current_dir(&*dir_arc)
        .output()
        .unwrap();
    assert!(events.status.success());
    let value: serde_json::Value = serde_json::from_slice(&events.stdout).unwrap();
    let accepted: Vec<_> = value["data"]["events"]
        .as_array()
        .expect("events array")
        .iter()
        .filter(|e| e["type"] == "handoff.accepted" || e["event_type"] == "handoff.accepted")
        .collect();
    assert_eq!(
        accepted.len(),
        1,
        "exactly one handoff.accepted event must be recorded: {value}"
    );
}
