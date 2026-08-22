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

#[test]
fn team_write_commands_have_nested_help_contract() {
    let (dir, bin) = setup_test_project("team_help");
    let help = run_cmd(&dir, &bin, &["team", "--help"]);
    assert!(help.status.success());
    let text = String::from_utf8_lossy(&help.stdout);
    assert!(text.contains("create"));
    assert!(text.contains("member"));
    assert!(text.contains("commander"));

    let task_help = run_cmd(&dir, &bin, &["task", "team", "--help"]);
    assert!(task_help.status.success());
    let text = String::from_utf8_lossy(&task_help.stdout);
    assert!(text.contains("set"));
    assert!(text.contains("unset"));
}

#[test]
fn team_status_projects_members_tasks_counts_and_is_read_only() {
    let (dir, bin) = setup_test_project("team_status");
    init_and_agent(&dir, &bin);
    let worker = run_cmd(
        &dir,
        &bin,
        &[
            "agent",
            "register",
            "--name",
            "worker",
            "--provider",
            "test",
            "--kind",
            "subagent",
        ],
    );
    assert!(worker.status.success());
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
    let team_id = json_stdout(&created)["data"]["team"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        run_cmd(
            &dir,
            &bin,
            &["team", "member", "add", &team_id, "--agent", "worker"]
        )
        .status
        .success()
    );
    let task = run_cmd(
        &dir,
        &bin,
        &[
            "task",
            "create",
            "--title",
            "active",
            "--team",
            &team_id,
            "--assignee",
            "worker",
            "--json",
        ],
    );
    let task_id = json_stdout(&task)["data"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        run_cmd(&dir, &bin, &["task", "start", &task_id])
            .status
            .success()
    );
    let before = json_stdout(&run_cmd(&dir, &bin, &["event", "list", "--json"]));
    let status = run_cmd(&dir, &bin, &["team", "status", "alpha", "--json"]);
    assert!(status.status.success());
    let body = json_stdout(&status);
    assert_eq!(body["command"], "team.status");
    assert_eq!(body["data"]["members"].as_array().unwrap().len(), 2);
    assert_eq!(body["data"]["counts"]["commanders"], 1);
    assert_eq!(body["data"]["counts"]["subagents"], 1);
    assert_eq!(body["data"]["members"][1]["active_task_count"], 1);
    assert_eq!(
        json_stdout(&run_cmd(&dir, &bin, &["event", "list", "--json"]))["data"]["events"],
        before["data"]["events"]
    );
}

#[test]
fn team_status_without_ref_is_project_scoped() {
    let (dir, bin) = setup_test_project("team_status_all");
    init_and_agent(&dir, &bin);
    assert!(
        run_cmd(&dir, &bin, &["team", "create", "--name", "alpha"])
            .status
            .success()
    );
    assert!(
        run_cmd(&dir, &bin, &["team", "create", "--name", "beta"])
            .status
            .success()
    );
    let all = run_cmd(&dir, &bin, &["team", "status", "--json"]);
    assert!(all.status.success());
    assert_eq!(
        json_stdout(&all)["data"]["teams"].as_array().unwrap().len(),
        2
    );
    let unknown = run_cmd(&dir, &bin, &["team", "status", "missing", "--json"]);
    assert!(!unknown.status.success());
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("TEAM_NOT_FOUND"));
}

#[test]
fn team_create_and_membership_writes_emit_snake_case_json_and_audit_actor() {
    let (dir, bin) = setup_test_project("team_create");
    init_and_agent(&dir, &bin);
    let registered = run_cmd(
        &dir,
        &bin,
        &[
            "agent",
            "register",
            "--name",
            "worker",
            "--provider",
            "test",
            "--json",
        ],
    );
    assert!(registered.status.success());

    let created = run_cmd_as(
        &dir,
        &bin,
        "tester",
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
    let body = json_stdout(&created);
    assert_eq!(body["command"], "team.create");
    assert!(body["data"]["team"]["id"].is_string());
    let team_id = body["data"]["team"]["id"].as_str().unwrap();
    let commander_id = body["data"]["team"]["commander_agent_id"].as_str().unwrap();

    let member_events = run_cmd(
        &dir,
        &bin,
        &[
            "event",
            "list",
            "--event-type",
            "team.member_added",
            "--json",
        ],
    );
    assert!(member_events.status.success());
    let member_events_body = json_stdout(&member_events);
    assert_eq!(
        member_events_body["data"]["events"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        member_events_body["data"]["events"][0]["payload"]["agent_id"],
        commander_id
    );

    let by_name = run_cmd(
        &dir,
        &bin,
        &["team", "commander", "set", "alpha", "--clear", "--json"],
    );
    assert!(
        by_name.status.success(),
        "{}",
        String::from_utf8_lossy(&by_name.stderr)
    );

    let added = run_cmd_as(
        &dir,
        &bin,
        "tester",
        &[
            "team",
            "member",
            "add",
            team_id,
            "--agent",
            "worker",
            "--role",
            "implementation",
            "--json",
        ],
    );
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    let body = json_stdout(&added);
    assert_eq!(body["command"], "team.member_add");
    assert_eq!(body["data"]["member"]["role"], "implementation");
}

#[test]
fn team_create_rejects_duplicate_name_with_state_conflict() {
    let (dir, bin) = setup_test_project("team_duplicate_name");
    init_and_agent(&dir, &bin);
    let created = run_cmd(&dir, &bin, &["team", "create", "--name", "alpha", "--json"]);
    assert!(created.status.success());

    let duplicate = run_cmd(&dir, &bin, &["team", "create", "--name", "alpha", "--json"]);
    assert!(!duplicate.status.success());
    assert!(
        duplicate.stdout.is_empty(),
        "failed command must not write to stdout"
    );
    let stderr = String::from_utf8_lossy(&duplicate.stderr);
    let envelope: serde_json::Value =
        serde_json::from_str(&stderr).expect("error envelope must be valid JSON on stderr");
    assert_eq!(envelope["success"], serde_json::json!(false));
    assert_eq!(
        envelope["error"]["code"],
        serde_json::json!("STATE_CONFLICT"),
        "duplicate team name must be a state conflict, not a raw database error"
    );
    assert!(
        !stderr.contains("UNIQUE constraint"),
        "error message must not leak SQL details: {stderr}"
    );
    assert_eq!(duplicate.status.code(), Some(3), "STATE_CONFLICT exits 3");
}

#[test]
fn team_rejects_duplicate_membership_and_commander_removal_until_clear() {
    let (dir, bin) = setup_test_project("team_policy");
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
    let team = json_stdout(&created)["data"]["team"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let duplicate = run_cmd(
        &dir,
        &bin,
        &[
            "team", "member", "add", &team, "--agent", "tester", "--json",
        ],
    );
    assert!(!duplicate.status.success());
    assert!(String::from_utf8_lossy(&duplicate.stderr).contains("STATE_CONFLICT"));

    let removal = run_cmd(
        &dir,
        &bin,
        &[
            "team", "member", "remove", &team, "--agent", "tester", "--json",
        ],
    );
    assert!(!removal.status.success());
    assert!(String::from_utf8_lossy(&removal.stderr).contains("STATE_CONFLICT"));

    let clear = run_cmd(
        &dir,
        &bin,
        &["team", "commander", "set", &team, "--clear", "--json"],
    );
    assert!(
        clear.status.success(),
        "{}",
        String::from_utf8_lossy(&clear.stderr)
    );
    let removal = run_cmd(
        &dir,
        &bin,
        &[
            "team", "member", "remove", &team, "--agent", "tester", "--json",
        ],
    );
    assert!(
        removal.status.success(),
        "{}",
        String::from_utf8_lossy(&removal.stderr)
    );
}

#[test]
fn team_refs_are_project_scoped_and_fail_without_mutation() {
    let (dir, bin) = setup_test_project("team_refs");
    init_and_agent(&dir, &bin);
    let unknown = run_cmd(
        &dir,
        &bin,
        &[
            "team", "member", "add", "missing", "--agent", "tester", "--json",
        ],
    );
    assert!(!unknown.status.success());
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("RESOURCE_NOT_FOUND"));
    let created = run_cmd(&dir, &bin, &["team", "create", "--name", "alpha", "--json"]);
    let created_body = json_stdout(&created);
    let team = created_body["data"]["team"]["id"].as_str().unwrap();
    let bad_task = run_cmd(
        &dir,
        &bin,
        &["task", "team", "set", "missing", "--team", team, "--json"],
    );
    assert!(!bad_task.status.success());
    assert!(String::from_utf8_lossy(&bad_task.stderr).contains("RESOURCE_NOT_FOUND"));
}

#[test]
fn failed_create_rolls_back_team_and_audit_event() {
    let (dir, bin) = setup_test_project("team_rollback");
    init_and_agent(&dir, &bin);
    let failed = run_cmd(
        &dir,
        &bin,
        &[
            "team",
            "create",
            "--name",
            "alpha",
            "--commander",
            "missing",
            "--json",
        ],
    );
    assert!(!failed.status.success());
    let created = run_cmd(&dir, &bin, &["team", "create", "--name", "alpha", "--json"]);
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let events = run_cmd(
        &dir,
        &bin,
        &["event", "list", "--event-type", "team.created", "--json"],
    );
    assert!(
        events.status.success(),
        "{}",
        String::from_utf8_lossy(&events.stderr)
    );
    let body = json_stdout(&events);
    assert_eq!(body["data"]["events"][0]["event_type"], "team.created");
}

#[test]
fn team_mutations_attribute_audit_to_invocation_agent() {
    let (dir, bin) = setup_test_project("team_audit");
    init_and_agent(&dir, &bin);
    let worker = run_cmd(
        &dir,
        &bin,
        &[
            "agent",
            "register",
            "--name",
            "worker",
            "--provider",
            "test",
        ],
    );
    assert!(worker.status.success());
    let created = run_cmd_as(
        &dir,
        &bin,
        "tester",
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
    assert!(created.status.success());
    let team = json_stdout(&created)["data"]["team"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let added = run_cmd_as(
        &dir,
        &bin,
        "tester",
        &[
            "team", "member", "add", &team, "--agent", "worker", "--json",
        ],
    );
    assert!(added.status.success());
    eprintln!("added: {}", String::from_utf8_lossy(&added.stdout));
    let events = run_cmd(
        &dir,
        &bin,
        &["event", "list", "--event-type", "team.created", "--json"],
    );
    let body = json_stdout(&events);
    assert!(
        body["data"]["events"][0]["actor_agent_id"]
            .as_str()
            .is_some()
    );
}

#[test]
fn task_team_set_unset_and_dry_run_do_not_mutate() {
    let (dir, bin) = setup_test_project("task_team");
    init_and_agent(&dir, &bin);
    let team = json_stdout(&run_cmd(
        &dir,
        &bin,
        &["team", "create", "--name", "alpha", "--json"],
    ))["data"]["team"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let task = json_stdout(&run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", "work", "--json"],
    ))["data"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let dry = run_cmd(
        &dir,
        &bin,
        &[
            "--dry-run",
            "task",
            "team",
            "set",
            &task,
            "--team",
            &team,
            "--json",
        ],
    );
    assert!(dry.status.success());
    let dry_body = json_stdout(&dry);
    assert_eq!(dry_body["command"], "task.team_set");
    assert_eq!(dry_body["data"]["operation"]["applied"], false);
    assert_eq!(dry_body["data"]["task"]["id"], task);
    assert_eq!(dry_body["data"]["task"]["team_id"], team);
    assert!(dry_body["data"]["previous_team_id"].is_null());
    let set = run_cmd(
        &dir,
        &bin,
        &["task", "team", "set", &task, "--team", &team, "--json"],
    );
    assert!(
        set.status.success(),
        "{}",
        String::from_utf8_lossy(&set.stderr)
    );
    let set_body = json_stdout(&set);
    assert_eq!(set_body["command"], "task.team_set");
    assert_eq!(set_body["data"]["task"]["team_id"], team);
    assert!(set_body["data"]["previous_team_id"].is_null());
    assert_eq!(set_body["data"]["operation"]["applied"], true);

    let dry_via_none = run_cmd(
        &dir,
        &bin,
        &[
            "task",
            "team",
            "set",
            &task,
            "--team",
            "none",
            "--dry-run",
            "--json",
        ],
    );
    assert!(dry_via_none.status.success());
    let dry_via_none_body = json_stdout(&dry_via_none);
    assert_eq!(dry_via_none_body["command"], "task.team_unset");
    assert_eq!(dry_via_none_body["data"]["task"]["id"], task);
    assert_eq!(
        dry_via_none_body["data"]["task"]["team_id"],
        serde_json::Value::Null
    );
    assert_eq!(dry_via_none_body["data"]["previous_team_id"], team);
    assert_eq!(dry_via_none_body["data"]["operation"]["applied"], false);

    let dry_unset = run_cmd(
        &dir,
        &bin,
        &["--dry-run", "task", "team", "unset", &task, "--json"],
    );
    assert!(dry_unset.status.success());
    let dry_unset_body = json_stdout(&dry_unset);
    assert_eq!(
        dry_unset_body["data"]["task"]["team_id"],
        serde_json::Value::Null
    );
    assert_eq!(dry_unset_body["data"]["previous_team_id"], team);
    assert_eq!(dry_unset_body["data"]["operation"]["applied"], false);

    let via_none = run_cmd(
        &dir,
        &bin,
        &["task", "team", "set", &task, "--team", "none", "--json"],
    );
    assert!(via_none.status.success());
    let via_none_body = json_stdout(&via_none);
    assert_eq!(via_none_body["command"], "task.team_unset");
    assert_eq!(via_none_body["data"]["task"]["id"], task);
    assert_eq!(
        via_none_body["data"]["task"]["team_id"],
        serde_json::Value::Null
    );
    assert_eq!(via_none_body["data"]["previous_team_id"], team);
    assert_eq!(via_none_body["data"]["operation"]["applied"], true);
    let unset = run_cmd(&dir, &bin, &["task", "team", "unset", &task, "--json"]);
    assert!(
        unset.status.success(),
        "{}",
        String::from_utf8_lossy(&unset.stderr)
    );
    let unset_body = json_stdout(&unset);
    assert_eq!(unset_body["command"], "task.team_unset");
    assert_eq!(
        unset_body["data"]["task"]["team_id"],
        serde_json::Value::Null
    );
    assert!(unset_body["data"]["previous_team_id"].is_null());
    assert_eq!(unset_body["data"]["operation"]["applied"], true);
}

#[test]
fn task_create_resolves_team_and_rejects_unknown_team_without_database_error() {
    let (dir, bin) = setup_test_project("task_team_create");
    init_and_agent(&dir, &bin);
    let created = run_cmd(&dir, &bin, &["team", "create", "--name", "alpha", "--json"]);
    assert!(created.status.success());
    let task = run_cmd(
        &dir,
        &bin,
        &[
            "task", "create", "--title", "assigned", "--team", "alpha", "--json",
        ],
    );
    assert!(
        task.status.success(),
        "{}",
        String::from_utf8_lossy(&task.stderr)
    );
    assert_eq!(
        json_stdout(&task)["data"]["team_id"]
            .as_str()
            .unwrap()
            .len(),
        26
    );

    let unknown = run_cmd(
        &dir,
        &bin,
        &[
            "task", "create", "--title", "bad", "--team", "missing", "--json",
        ],
    );
    assert!(!unknown.status.success());
    let body: Value = serde_json::from_slice(&unknown.stderr).expect("error envelope on stderr");
    assert_eq!(body["error"]["code"], "RESOURCE_NOT_FOUND");
}

#[test]
fn team_mutation_text_dry_run_stays_concise() {
    let (dir, bin) = setup_test_project("team_text_dry");
    init_and_agent(&dir, &bin);
    let out = run_cmd(
        &dir,
        &bin,
        &["--dry-run", "team", "create", "--name", "alpha"],
    );
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("[dry-run]"));
    assert!(out.stdout.is_empty());
}

#[test]
fn agent_register_kind_is_persisted_and_constrained() {
    let (dir, bin) = setup_test_project("agent_kind");
    init_and_agent(&dir, &bin);
    let registered = run_cmd(
        &dir,
        &bin,
        &[
            "agent",
            "register",
            "--name",
            "commander",
            "--kind",
            "commander",
            "--json",
        ],
    );
    assert!(
        registered.status.success(),
        "{}",
        String::from_utf8_lossy(&registered.stderr)
    );
    assert_eq!(json_stdout(&registered)["data"]["kind"], "commander");

    let invalid = run_cmd(
        &dir,
        &bin,
        &[
            "agent", "register", "--name", "bad-kind", "--kind", "operator", "--json",
        ],
    );
    assert!(!invalid.status.success());
}

#[test]
fn task_edit_required_role_is_persisted() {
    let (dir, bin) = setup_test_project("task_required_role");
    init_and_agent(&dir, &bin);
    let created = run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", "role task", "--json"],
    );
    assert!(created.status.success());
    let edited = run_cmd(
        &dir,
        &bin,
        &[
            "task",
            "edit",
            "CTX-0001",
            "--required-role",
            "reviewer",
            "--json",
        ],
    );
    assert!(
        edited.status.success(),
        "{}",
        String::from_utf8_lossy(&edited.stderr)
    );
    assert_eq!(json_stdout(&edited)["data"]["required_role"], "reviewer");
}
