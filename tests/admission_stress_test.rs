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
/// Characters of stderr/stdout kept per child for failure diagnostics.
const TAIL_CHARS: usize = 2000;

fn tail(content: &str) -> String {
    let total = content.chars().count();
    if total <= TAIL_CHARS {
        content.to_string()
    } else {
        content.chars().skip(total - TAIL_CHARS).collect()
    }
}

/// Full result of one child invocation: exit code plus captured streams so
/// an unexpected exit can be diagnosed from the assertion message alone
/// (CI previously reported only `Some(101)` with the panic text discarded).
struct ChildRun {
    /// Process exit code; `None` means spawn failure or CHILD_TIMEOUT kill.
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl ChildRun {
    /// Human-readable diagnostic block: exit code, stderr tail, stdout tail.
    fn report(&self) -> String {
        format!(
            "exit={:?}\n--- stderr tail ---\n{}\n--- stdout tail ---\n{}",
            self.code,
            tail(self.stderr.trim_end()),
            tail(self.stdout.trim_end())
        )
    }
}

struct ChildOutcome {
    name: String,
    graph: ChildRun,
    task: ChildRun,
}

fn run_child(dir: &Path, bin: &Path, args: &[&str]) -> ChildRun {
    let deadline = Instant::now() + CHILD_TIMEOUT;
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .env("CARRYCTX_AGENT", "stresser")
        // Panic text alone names the site; a symbolized backtrace makes the
        // interleaving obvious when a crash only reproduces on CI runners.
        .env("RUST_BACKTRACE", "1")
        .current_dir(dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) => {
            return ChildRun {
                code: None,
                stdout: String::new(),
                stderr: format!("<spawn failed: {err}>"),
            };
        }
    };
    // Drain both pipes on dedicated threads so a chatty child can never fill
    // its pipe buffer and deadlock before the timeout logic kicks in.
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let stdout_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(pipe) = stdout_pipe.as_mut() {
            let _ = std::io::Read::read_to_end(pipe, &mut buf);
        }
        String::from_utf8_lossy(&buf).into_owned()
    });
    let stderr_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(pipe) = stderr_pipe.as_mut() {
            let _ = std::io::Read::read_to_end(pipe, &mut buf);
        }
        String::from_utf8_lossy(&buf).into_owned()
    });
    let (code, timed_out) = loop {
        match child.try_wait().expect("try_wait should not fail") {
            Some(status) => break (status.code(), false),
            None => {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break (None, true);
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
    };
    let stdout = stdout_reader.join().unwrap_or_default();
    let mut stderr = stderr_reader.join().unwrap_or_default();
    if timed_out {
        stderr.push_str(&format!("\n<child killed after {:?}>", CHILD_TIMEOUT));
    }
    ChildRun {
        code,
        stdout,
        stderr,
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

    // GIT_* scrubbing matters here too: under hook runners these fixture
    // commands must never resolve into the repository under test (CTX-0082).
    common::fixture_git(&dir, &["init", "-b", "main"]);
    common::fixture_git(&dir, &["config", "user.email", "stress@carryctx.dev"]);
    common::fixture_git(&dir, &["config", "user.name", "Stress"]);
    common::fixture_git(&dir, &["commit", "--allow-empty", "-m", "init"]);

    let bin = common::test_binary();
    let init = run_child(&dir, &bin, &["init", "--force"]);
    assert_eq!(
        init.code,
        Some(0),
        "project init must succeed:\n{}",
        init.report()
    );

    // The audit event appended by every mutation references the acting
    // agent, so it must exist before children start mutating.
    let register = run_child(
        &dir,
        &bin,
        &[
            "agent",
            "register",
            "--name",
            "stresser",
            "--provider",
            "stress",
        ],
    );
    assert_eq!(
        register.code,
        Some(0),
        "agent registration must succeed:\n{}",
        register.report()
    );

    // Launch CHILDREN concurrent processes, each performing two real
    // mutations against the single shared project admission lock.
    let mut handles = Vec::new();
    for i in 0..CHILDREN {
        let dir = dir.clone();
        let bin = bin.clone();
        handles.push(thread::spawn(move || {
            let node = format!("stress-node-{run}-{i}");
            let task = format!("stress-task-{run}-{i}");
            let graph = run_child(
                &dir,
                &bin,
                &["graph", "add-node", "--node-type", "file", "--name", &node],
            );
            let task_run = run_child(&dir, &bin, &["task", "create", "--title", &task]);
            ChildOutcome {
                name: format!("child-{i}"),
                graph,
                task: task_run,
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
        for (kind, run) in [
            ("graph.add-node", &outcome.graph),
            ("task.create", &outcome.task),
        ] {
            match run.code {
                Some(c) if c <= 12 => {}
                other => crashed.push(format!(
                    "{} {}: exit {:?} exceeds envelope\n{}",
                    outcome.name,
                    kind,
                    other,
                    run.report()
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
    let export_out = run_child(&dir, &bin, &["graph", "export", "--type", "mermaid"]);
    assert_eq!(
        export_out.code,
        Some(0),
        "graph export must succeed:\n{}",
        export_out.report()
    );
    let export_text = export_out.stdout;

    let list_out = run_child(&dir, &bin, &["--json", "task", "list"]);
    assert_eq!(
        list_out.code,
        Some(0),
        "task list must succeed:\n{}",
        list_out.report()
    );
    let list_text = list_out.stdout;

    let mut lost = Vec::new();
    for (i, outcome) in outcomes.iter().enumerate() {
        let node = format!("stress-node-{run}-{i}");
        let task = format!("stress-task-{run}-{i}");
        if outcome.graph.code == Some(0) && !export_text.contains(&node) {
            lost.push(format!("node '{node}' reported success but is absent"));
        }
        if outcome.task.code == Some(0) && !list_text.contains(&task) {
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
        .filter(|o| o.graph.code == Some(0) && o.task.code == Some(0))
        .count();
    assert!(
        successes > 0,
        "every child failed; admission lock is rejecting all contenders"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
