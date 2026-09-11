//! CTX-0044: `task release` must enforce ownership. Design 2026-08-21
//! §4.4 (invariant I2) makes the acting agent's `kind` the gate: an owner may
//! always release; a `commander` may override and is audited as `forced`; a
//! peer `subagent` is rejected with `TASK_NOT_OWNED` (exit 9); an unclassified
//! (`kind IS NULL`) agent keeps the pre-team behavior for compatibility.

mod common;

use common::{init_and_agent, run_cmd, run_cmd_as, setup_test_project};
use serde_json::Value;

fn json_stdout(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "stdout was not JSON: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn json_stderr(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stderr).unwrap_or_else(|_| {
        panic!(
            "stderr was not JSON: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn register(dir: &std::path::Path, bin: &std::path::Path, name: &str, kind: &str) {
    let out = run_cmd(
        dir,
        bin,
        &[
            "agent",
            "register",
            "--name",
            name,
            "--provider",
            "test",
            "--kind",
            kind,
        ],
    );
    assert!(
        out.status.success(),
        "registering {name} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn released_payloads(dir: &std::path::Path, bin: &std::path::Path) -> Vec<Value> {
    let out = run_cmd(
        dir,
        bin,
        &["event", "list", "--event-type", "task.released", "--json"],
    );
    assert!(out.status.success(), "event list should succeed");
    json_stdout(&out)["data"]["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[test]
fn release_rejects_peer_subagent_and_audits_commander_override() {
    let (dir, bin) = setup_test_project("release_ownership");
    // `tester` is registered without a kind: the legacy/unclassified actor.
    init_and_agent(&dir, &bin);
    register(&dir, &bin, "owner", "subagent");
    register(&dir, &bin, "peer", "subagent");
    register(&dir, &bin, "cmdr", "commander");

    let created = run_cmd(
        &dir,
        &bin,
        &["--json", "task", "create", "--title", "Owned"],
    );
    assert!(created.status.success(), "task create should succeed");
    let task_id = json_stdout(&created)["data"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // The owner claims and starts the task.
    assert!(
        run_cmd_as(&dir, &bin, "owner", &["task", "claim", &task_id])
            .status
            .success(),
        "owner claim should succeed"
    );
    assert!(
        run_cmd_as(&dir, &bin, "owner", &["task", "start", &task_id])
            .status
            .success(),
        "owner start should succeed"
    );

    // A peer subagent cannot release another agent's task. Fail closed with a
    // typed error and exit 9, and write no audit event.
    let peer = run_cmd_as(&dir, &bin, "peer", &["--json", "task", "release", &task_id]);
    assert!(!peer.status.success(), "peer release must fail");
    assert_eq!(peer.status.code(), Some(9), "ownership denial is exit 9");
    let envelope = json_stderr(&peer);
    assert_eq!(envelope["success"], Value::Bool(false));
    assert_eq!(envelope["error"]["code"], "TASK_NOT_OWNED", "{envelope}");
    assert!(
        released_payloads(&dir, &bin).is_empty(),
        "a denied release must not append a task.released event"
    );

    // A commander may override ownership; the audit records it as forced.
    let commander = run_cmd_as(&dir, &bin, "cmdr", &["--json", "task", "release", &task_id]);
    assert!(
        commander.status.success(),
        "commander release should succeed: {}",
        String::from_utf8_lossy(&commander.stderr)
    );
    let events = released_payloads(&dir, &bin);
    assert_eq!(events.len(), 1, "one release event after the override");
    assert_eq!(
        events[0]["payload"]["forced"],
        Value::Bool(true),
        "a non-owner commander release is audited as forced: {}",
        events[0]
    );

    // The owning subagent releasing its own task is not forced.
    assert!(
        run_cmd_as(&dir, &bin, "owner", &["task", "claim", &task_id])
            .status
            .success()
    );
    assert!(
        run_cmd_as(&dir, &bin, "owner", &["task", "start", &task_id])
            .status
            .success()
    );
    assert!(
        run_cmd_as(&dir, &bin, "owner", &["task", "release", &task_id])
            .status
            .success(),
        "the owner may release its own task"
    );
    let events = released_payloads(&dir, &bin);
    assert_eq!(events.len(), 2);
    let forced_flags: Vec<bool> = events
        .iter()
        .map(|event| event["payload"]["forced"].as_bool().unwrap_or(false))
        .collect();
    assert_eq!(
        forced_flags.iter().filter(|forced| **forced).count(),
        1,
        "exactly the commander override is forced: {events:?}"
    );
    assert_eq!(
        forced_flags.iter().filter(|forced| !**forced).count(),
        1,
        "exactly the owner release is unforced: {events:?}"
    );
}

#[test]
fn release_keeps_legacy_unclassified_actor_behavior() {
    let (dir, bin) = setup_test_project("release_legacy");
    init_and_agent(&dir, &bin); // `tester` has no kind
    register(&dir, &bin, "owner", "subagent");

    let created = run_cmd(
        &dir,
        &bin,
        &["--json", "task", "create", "--title", "Legacy"],
    );
    let task_id = json_stdout(&created)["data"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        run_cmd_as(&dir, &bin, "owner", &["task", "claim", &task_id])
            .status
            .success()
    );
    assert!(
        run_cmd_as(&dir, &bin, "owner", &["task", "start", &task_id])
            .status
            .success()
    );

    // `tester` is unclassified, so the pre-team behavior is preserved: it may
    // release a task it does not own.
    let legacy = run_cmd_as(
        &dir,
        &bin,
        "tester",
        &["--json", "task", "release", &task_id],
    );
    assert!(
        legacy.status.success(),
        "unclassified legacy actor must keep unchecked behavior: {}",
        String::from_utf8_lossy(&legacy.stderr)
    );
    let events = released_payloads(&dir, &bin);
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0]["payload"]["forced"],
        Value::Bool(true),
        "a legacy third-party release is still audited as forced"
    );
}
