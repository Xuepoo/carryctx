//! Admission-lock stress regression (CI flake hotfix).
//!
//! Spawns parallel CLI children doing real mutations against ONE project and
//! asserts the whole fleet lands inside the documented exit-code envelope
//! (0..=12 — a Rust panic exits 101) with every mutation persisted.
//!
//! CI runners are slow and 2-core: child count, retry budget, and timeouts
//! below are sized to finish well under a minute even there.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU16, Ordering};
use std::thread;
use std::time::{Duration, Instant};

static STRESS_COUNTER: AtomicU16 = AtomicU16::new(0);

const CHILDREN: usize = 8;
/// Upper bound for one child invocation. Generous for starved runners,
/// but bounded so a hung child fails the test instead of freezing CI.
const CHILD_TIMEOUT: Duration = Duration::from_secs(30);

struct ChildOutcome {
    name: String,
    graph_code: Option<i32>,
    task_code: Option<i32>,
}

fn run_child(dir: &Path, bin: &Path, args: &[String]) -> Option<i32> {
    let deadline = Instant::now() + CHILD_TIMEOUT;
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .env("CARRYCTX_AGENT", "stresser")
        .current_dir(dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(_) => return None,
    };
    loop {
        match child.try_wait().expect("try_wait should not fail") {
            Some(status) => return status.code(),
            None => {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

#[test]
fn parallel_mutating_children_stay_inside_the_exit_envelope_and_persist_everything() {
    let run = STRESS_COUNTER.fetch_add(1, Ordering::SeqCst) as u32;
    let tag = format!(
        "admission_stress_{}_{}",
        std::process::id(),
        STRESS_COUNTER.fetch_add(0, Ordering::SeqCst)
    );
    let dir: PathBuf = std::env::temp_dir().join(format!("{tag}_{run}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let status = Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(&dir)
        .status()
        .expect("git init");
    assert!(status.success());
    Command::new("git")
        .args(["config", "user.email", "stress@carryctx.dev"])
        .current_dir(&dir)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "Stress"])
        .current_dir(&dir)
        .status()
        .unwrap();
    Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(&dir)
        .status()
        .unwrap();

    let bin = common::test_binary();
    let init = run_child(&dir, &bin, &["init".into(), "--force".into()]);
    assert_eq!(init, Some(0), "project init must succeed");

    // The audit event appended by every mutation references the acting
    // agent, so it must exist before children start mutating.
    let register = run_child(
        &dir,
        &bin,
        &[
            "agent".into(),
            "register".into(),
            "--name".into(),
            "stresser".into(),
            "--provider".into(),
            "stress".into(),
        ],
    );
    assert_eq!(register, Some(0), "agent registration must succeed");

    // Launch CHILDREN concurrent processes, each performing two real
    // mutations against the single shared project admission lock.
    let mut handles = Vec::new();
    for i in 0..CHILDREN {
        let dir = dir.clone();
        let bin = bin.clone();
        handles.push(thread::spawn(move || {
            let node = format!("stress-node-{run}-{i}");
            let task = format!("stress-task-{run}-{i}");
            let graph_code = run_child(
                &dir,
                &bin,
                &[
                    "graph".into(),
                    "add-node".into(),
                    "--node-type".into(),
                    "file".into(),
                    "--name".into(),
                    node,
                ],
            );
            let task_code = run_child(
                &dir,
                &bin,
                &["task".into(), "create".into(), "--title".into(), task],
            );
            ChildOutcome {
                name: format!("child-{i}"),
                graph_code,
                task_code,
            }
        }));
    }

    let mut outcomes = Vec::new();
    for handle in handles {
        outcomes.push(handle.join().expect("child thread must not panic"));
    }

    // Every child must exit inside the documented envelope: exit codes are
    // capped at 12; anything above (notably 101/134) is a crash. A `None`
    // means the child had to be killed after CHILD_TIMEOUT.
    let mut crashed = Vec::new();
    for outcome in &outcomes {
        for (kind, code) in [
            ("graph.add-node", outcome.graph_code),
            ("task.create", outcome.task_code),
        ] {
            match code {
                Some(c) if c <= 12 => {}
                other => crashed.push(format!(
                    "{} {}: exit {:?} exceeds envelope",
                    outcome.name, kind, other
                )),
            }
        }
    }
    assert!(
        crashed.is_empty(),
        "children crashed instead of using the error envelope:\n{}",
        crashed.join("\n")
    );

    // Every successful mutation must actually be persisted: a success exit
    // code is only printed AFTER the UnitOfWork commit, so a missing row
    // means the write was lost despite reported success.
    let export_out = Command::new(&bin)
        .args(["graph", "export", "--type", "mermaid"])
        .current_dir(&dir)
        .output()
        .expect("export output");
    assert_eq!(
        export_out.status.code(),
        Some(0),
        "graph export must succeed"
    );
    let export_text = String::from_utf8_lossy(&export_out.stdout).into_owned();

    let list_out = Command::new(&bin)
        .args(["--json", "task", "list"])
        .current_dir(&dir)
        .output()
        .expect("list output");
    assert_eq!(list_out.status.code(), Some(0), "task list must succeed");
    let list_text = String::from_utf8_lossy(&list_out.stdout).into_owned();

    let mut lost = Vec::new();
    for (i, outcome) in outcomes.iter().enumerate() {
        let node = format!("stress-node-{run}-{i}");
        let task = format!("stress-task-{run}-{i}");
        if outcome.graph_code == Some(0) && !export_text.contains(&node) {
            lost.push(format!("node '{node}' reported success but is absent"));
        }
        if outcome.task_code == Some(0) && !list_text.contains(&task) {
            lost.push(format!("task '{task}' reported success but is absent"));
        }
    }
    assert!(
        lost.is_empty(),
        "mutations were lost despite success envelopes:\n{}",
        lost.join("\n")
    );

    // At least one full success path must have exercised the lock; if every
    // child failed something systemic broke (e.g. all hit STATE_CONFLICT).
    let successes = outcomes
        .iter()
        .filter(|o| o.graph_code == Some(0) && o.task_code == Some(0))
        .count();
    assert!(
        successes > 0,
        "every child failed; admission lock is rejecting all contenders"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
