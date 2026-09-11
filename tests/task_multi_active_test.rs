//! CTX-0043: `single_active_task_per_agent` is a deprecated, non-enforcing
//! compatibility key. Design `2026-08-21-agent-team-orchestration.md` §11
//! (invariants I19/I21) and DEC-0004: multiple in-progress tasks per agent are
//! supported, and no configuration value may make `task claim`, `task start`,
//! or `task assign` fail because of a task count.
//!
//! These tests pin that behavior. There is no capacity cap, warning, or
//! signal to assert against: the ratified v0.6 resolution is compatibility-only
//! and inert, so the honest contract is "the key has no effect".

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

fn agent_id(dir: &std::path::Path, bin: &std::path::Path, name: &str) -> String {
    let out = run_cmd(dir, bin, &["--json", "agent", "show", name]);
    assert!(
        out.status.success(),
        "agent show {name} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    json_stdout(&out)["data"]["id"].as_str().unwrap().to_owned()
}

fn create_task(dir: &std::path::Path, bin: &std::path::Path, title: &str) -> String {
    let out = run_cmd(dir, bin, &["--json", "task", "create", "--title", title]);
    assert!(
        out.status.success(),
        "task create failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    json_stdout(&out)["data"]["id"].as_str().unwrap().to_owned()
}

/// Claim and start `task_id` as `agent`, returning the `task.start` envelope.
fn claim_and_start(
    dir: &std::path::Path,
    bin: &std::path::Path,
    agent: &str,
    task_id: &str,
) -> Value {
    let claim = run_cmd_as(dir, bin, agent, &["--json", "task", "claim", task_id]);
    assert!(
        claim.status.success(),
        "claim of {task_id} by {agent} must succeed: {}",
        String::from_utf8_lossy(&claim.stderr)
    );
    let start = run_cmd_as(dir, bin, agent, &["--json", "task", "start", task_id]);
    assert!(
        start.status.success(),
        "start of {task_id} by {agent} must succeed: {}",
        String::from_utf8_lossy(&start.stderr)
    );
    json_stdout(&start)
}

#[test]
fn default_config_permits_multiple_in_progress_tasks_per_agent() {
    let (dir, bin) = setup_test_project("multi_active_default");
    init_and_agent(&dir, &bin);
    // A subagent is the strictest identity for ownership checks; the legacy
    // key defaults to `true` in the shipped config.
    register(&dir, &bin, "worker", "subagent");

    let first = create_task(&dir, &bin, "First");
    let second = create_task(&dir, &bin, "Second");

    claim_and_start(&dir, &bin, "worker", &first);
    let start_envelope = claim_and_start(&dir, &bin, "worker", &second);

    // No capacity warning is emitted: the ratified design ships none, so an
    // agent is never nudged to fan out by a count-based signal.
    if let Some(warnings) = start_envelope["data"]["warnings"].as_array() {
        assert!(
            warnings.is_empty(),
            "no capacity warning is expected: {start_envelope}"
        );
    }

    let worker = agent_id(&dir, &bin, "worker");
    for task in [&first, &second] {
        let show = run_cmd(&dir, &bin, &["--json", "task", "show", task]);
        assert!(show.status.success());
        let data = json_stdout(&show)["data"].clone();
        assert_eq!(data["status"], "in_progress", "task {task} stays active");
        assert_eq!(
            data["owner_agent_id"].as_str(),
            Some(worker.as_str()),
            "task {task} is owned by the same worker"
        );
    }
}

#[test]
fn legacy_key_false_does_not_gate_a_second_active_task() {
    let (dir, bin) = setup_test_project("multi_active_false");
    init_and_agent(&dir, &bin);
    register(&dir, &bin, "worker", "subagent");

    // Flip the deprecated key to the other value; it must remain inert. This
    // is the case a reader of the old documentation would expect to cap the
    // agent at one active task.
    let cfg = dir.join(".carryctx/config.toml");
    let content = std::fs::read_to_string(&cfg).unwrap();
    std::fs::write(
        &cfg,
        content.replace(
            "single_active_task_per_agent = true",
            "single_active_task_per_agent = false",
        ),
    )
    .unwrap();
    let get = run_cmd(
        &dir,
        &bin,
        &[
            "--json",
            "config",
            "get",
            "task.single_active_task_per_agent",
        ],
    );
    assert_eq!(
        json_stdout(&get)["data"]["value"],
        Value::Bool(false),
        "the fixture must actually set the key to false"
    );

    let first = create_task(&dir, &bin, "First");
    let second = create_task(&dir, &bin, "Second");
    claim_and_start(&dir, &bin, "worker", &first);
    claim_and_start(&dir, &bin, "worker", &second);

    let listed = run_cmd(
        &dir,
        &bin,
        &["--json", "task", "list", "--status", "in_progress"],
    );
    assert!(listed.status.success());
    let active = json_stdout(&listed)["data"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        active.len() >= 2,
        "both tasks stay in progress with the legacy key false: {active:?}"
    );
}
