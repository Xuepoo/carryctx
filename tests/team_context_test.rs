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
fn context_and_resume_legacy_camelcase_keys_are_frozen() {
    let (dir, bin) = setup_test_project("legacy_camelcase_frozen");
    init_and_agent(&dir, &bin);

    let context = run_cmd(&dir, &bin, &["context", "--json"]);
    assert!(
        context.status.success(),
        "{}",
        String::from_utf8_lossy(&context.stderr)
    );
    let context_body = body(&context);
    let data = &context_body["data"];
    for key in ["projectId", "projectName", "currentTask", "contextGraph"] {
        assert!(
            data.get(key).is_some(),
            "context must keep legacy camelCase key {key}"
        );
    }
    assert!(
        data["contextGraph"].get("nodeCount").is_some(),
        "contextGraph must keep legacy camelCase key nodeCount"
    );
    assert!(
        data["contextGraph"].get("edgeCount").is_some(),
        "contextGraph must keep legacy camelCase key edgeCount"
    );

    let resume = run_cmd(&dir, &bin, &["resume", "--json"]);
    assert!(
        resume.status.success(),
        "{}",
        String::from_utf8_lossy(&resume.stderr)
    );
    let resume_body = body(&resume);
    let data = &resume_body["data"];
    for key in [
        "projectId",
        "currentSession",
        "currentTask",
        "latestCheckpoint",
        "recentEvents",
    ] {
        assert!(
            data.get(key).is_some(),
            "resume must keep legacy camelCase key {key}"
        );
    }
}

#[test]
fn team_context_text_and_markdown_are_human_readable() {
    let (dir, bin) = setup_test_project("team_context_render");
    init_and_agent(&dir, &bin);
    let created = run_cmd(
        &dir,
        &bin,
        &[
            "team",
            "create",
            "--name",
            "alpha",
            "--commander",
            "tester",
            "--json",
        ],
    );
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let team_id = body(&created)["data"]["team"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let text = run_cmd(&dir, &bin, &["team", "context", &team_id]);
    assert!(
        text.status.success(),
        "{}",
        String::from_utf8_lossy(&text.stderr)
    );
    let text_out = String::from_utf8_lossy(&text.stdout);
    assert!(
        !text_out.trim_start().starts_with('{'),
        "context text output must not be JSON: {text_out}"
    );
    assert!(
        text_out.contains("alpha"),
        "context summary must name the team: {text_out}"
    );

    let markdown = run_cmd(
        &dir,
        &bin,
        &["team", "context", &team_id, "--format", "markdown"],
    );
    assert!(
        markdown.status.success(),
        "{}",
        String::from_utf8_lossy(&markdown.stderr)
    );
    let markdown_out = String::from_utf8_lossy(&markdown.stdout);
    assert!(
        !markdown_out.trim_start().starts_with('{'),
        "context markdown must not be JSON: {markdown_out}"
    );
    assert!(
        markdown_out.contains("# "),
        "context markdown needs a heading: {markdown_out}"
    );
    assert!(
        markdown_out.contains('|'),
        "context markdown needs a table: {markdown_out}"
    );
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
