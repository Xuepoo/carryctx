//! CTX-0082 / issue #108: graph mutations with an unregistered acting agent
//! leaked raw SQLite internals (`EVENTS_APPEND_ERROR: SqliteFailure(...
//! ConstraintViolation...787)`) as DATABASE_ERROR rc=5, while task paths
//! returned a clean RESOURCE_NOT_FOUND for the same input class. Every path
//! must answer an unknown actor with the identical clean envelope.

mod common;

use serde_json::Value;

fn run_as(
    dir: &std::path::Path,
    bin: &std::path::Path,
    agent: &str,
    args: &[&str],
) -> std::process::Output {
    std::process::Command::new(bin)
        .args(args)
        .env("CARRYCTX_AGENT", agent)
        .current_dir(dir)
        .output()
        .expect("command should execute")
}

fn envelope(output: &std::process::Output) -> Value {
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    serde_json::from_str(stdout.trim())
        .or_else(|_| serde_json::from_str(stderr.trim()))
        .unwrap_or_else(|e| panic!("expected a JSON error envelope ({e}): {stdout} | {stderr}"))
}

fn combined_text(output: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// The shared expectation: every ghost-actor mutation answers exactly like
/// `task create` does — clean RESOURCE_NOT_FOUND, exit 7, naming the agent,
/// and zero SQL internals anywhere in the output.
fn assert_clean_ghost_envelope(output: &std::process::Output, command_label: &str) {
    assert_eq!(
        output.status.code(),
        Some(7),
        "{command_label} must exit 7; out={}",
        combined_text(output)
    );
    let value = envelope(output);
    assert_eq!(value["success"], false, "{command_label}");
    assert_eq!(
        value["error"]["code"],
        "RESOURCE_NOT_FOUND",
        "{command_label}: {}",
        combined_text(output)
    );
    assert_eq!(
        value["error"]["message"], "Agent 'ghost' not found.",
        "{command_label}"
    );
    let text = combined_text(output);
    for leaked in [
        "SqliteFailure",
        "ConstraintViolation",
        "EVENTS_APPEND_ERROR",
        "FOREIGN KEY",
    ] {
        assert!(
            !text.contains(leaked),
            "{command_label} leaked SQL internals ({leaked}): {text}"
        );
    }
}

#[test]
fn ghost_actor_graph_mutations_match_task_create_baseline() {
    let (dir, bin) = common::setup_test_project("graph_ghost_actor");
    common::init_and_agent(&dir, &bin);

    // Baseline: task paths already answer cleanly.
    let baseline = run_as(
        &dir,
        &bin,
        "ghost",
        &["task", "create", "--title", "x", "--json"],
    );
    assert_eq!(baseline.status.code(), Some(7));
    let baseline_value = envelope(&baseline);
    assert_eq!(baseline_value["error"]["code"], "RESOURCE_NOT_FOUND");
    assert_eq!(
        baseline_value["error"]["message"],
        "Agent 'ghost' not found."
    );

    let add_node = run_as(
        &dir,
        &bin,
        "ghost",
        &[
            "graph",
            "add-node",
            "--node-type",
            "service",
            "--name",
            "alpha",
            "--json",
        ],
    );
    assert_clean_ghost_envelope(&add_node, "graph add-node");

    // Compare envelopes field-by-field against the baseline.
    let base = envelope(&baseline);
    let node = envelope(&add_node);
    assert_eq!(
        base["error"], node["error"],
        "error objects must be identical"
    );

    let link = run_as(
        &dir,
        &bin,
        "ghost",
        &["graph", "link", "a", "b", "calls", "--json"],
    );
    assert_clean_ghost_envelope(&link, "graph link");

    let scan = run_as(&dir, &bin, "ghost", &["graph", "scan", "--json"]);
    assert_clean_ghost_envelope(&scan, "graph scan");

    // Nothing may have been written by the rejected mutations.
    let nodes = run_as(&dir, &bin, "tester", &["graph", "nodes", "--json"]);
    if nodes.status.success() {
        let text = String::from_utf8_lossy(&nodes.stdout).into_owned();
        assert!(
            !text.contains("\"alpha\""),
            "rejected node leaked in: {text}"
        );
    }
}

#[test]
fn registered_agent_graph_mutation_still_succeeds() {
    let (dir, bin) = common::setup_test_project("graph_registered_actor");
    common::init_and_agent(&dir, &bin);

    let out = run_as(
        &dir,
        &bin,
        "tester",
        &[
            "graph",
            "add-node",
            "--node-type",
            "service",
            "--name",
            "alpha",
            "--json",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", combined_text(&out));
}
