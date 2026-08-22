mod common;

use common::{init_and_agent, run_cmd, setup_test_project};
use serde_json::Value;

fn body(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "stdout was not JSON: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

#[test]
fn team_context_projects_empty_team_with_public_schema() {
    let (dir, bin) = setup_test_project("team_context_empty");
    init_and_agent(&dir, &bin);

    let created = run_cmd(&dir, &bin, &["team", "create", "--name", "alpha", "--json"]);
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let team_id = body(&created)["data"]["team"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let context = run_cmd(&dir, &bin, &["team", "context", &team_id, "--json"]);
    assert!(
        context.status.success(),
        "{}",
        String::from_utf8_lossy(&context.stderr)
    );
    let response = body(&context);
    assert_eq!(response["command"], "team.context");
    for key in [
        "team",
        "view",
        "members",
        "tasks",
        "dependencies",
        "scopes",
        "progress",
        "scope_conflicts",
        "blockers",
        "conflicts",
        "latest_checkpoints",
        "decisions",
        "handoffs",
        "recent_events",
        "rebuild",
    ] {
        assert!(
            response["data"].get(key).is_some(),
            "missing context key {key}"
        );
    }
    assert_eq!(response["data"]["view"], "commander");
    assert_eq!(response["data"]["rebuild"]["source"], "durable_records");
}

#[test]
fn team_context_rejects_unknown_session_without_writing() {
    let (dir, bin) = setup_test_project("team_context_read_only");
    init_and_agent(&dir, &bin);
    let created = run_cmd(&dir, &bin, &["team", "create", "--name", "alpha", "--json"]);
    let team_id = body(&created)["data"]["team"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let unknown = run_cmd(
        &dir,
        &bin,
        &[
            "--session",
            "missing",
            "team",
            "context",
            &team_id,
            "--json",
        ],
    );
    assert!(!unknown.status.success());
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("RESOURCE_NOT_FOUND"));
}
