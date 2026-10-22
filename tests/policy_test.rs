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

fn register_agent(dir: &std::path::Path, bin: &std::path::Path, name: &str, role: Option<&str>) {
    let mut args = vec!["agent", "register", "--name", name, "--provider", "test"];
    if let Some(r) = role {
        args.push("--role");
        args.push(r);
    }
    let out = run_cmd(dir, bin, &args);
    assert!(
        out.status.success(),
        "registering agent {name} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn create_task(
    dir: &std::path::Path,
    bin: &std::path::Path,
    title: &str,
    role: Option<&str>,
    assignee: Option<&str>,
) -> String {
    let mut args = vec!["--json", "task", "create", "--title", title];
    if let Some(r) = role {
        args.push("--required-role");
        args.push(r);
    }
    if let Some(a) = assignee {
        args.push("--assignee");
        args.push(a);
    }
    let out = run_cmd(dir, bin, &args);
    assert!(
        out.status.success(),
        "task create failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    json_stdout(&out)["data"]["id"].as_str().unwrap().to_owned()
}

fn set_policy(dir: &std::path::Path, enforce_role: bool, single_active: bool) {
    let cfg = dir.join(".carryctx/config.toml");
    let content = std::fs::read_to_string(&cfg).unwrap();
    let updated = content
        .replace(
            "enforce_role_matching = false",
            &format!("enforce_role_matching = {enforce_role}"),
        )
        .replace(
            "single_active_task = false",
            &format!("single_active_task = {single_active}"),
        );
    std::fs::write(&cfg, updated).unwrap();
}

#[test]
fn default_policy_allows_role_mismatch_and_multiple_active_tasks() {
    let (dir, bin) = setup_test_project("policy_default");
    init_and_agent(&dir, &bin);

    register_agent(&dir, &bin, "frontend_dev", Some("frontend"));
    let task_id = create_task(&dir, &bin, "Backend Task", Some("backend"), None);

    // Role mismatch is allowed by default
    let claim = run_cmd_as(
        &dir,
        &bin,
        "frontend_dev",
        &["--json", "task", "claim", &task_id],
    );
    assert!(
        claim.status.success(),
        "default claim should succeed despite role mismatch: {}",
        String::from_utf8_lossy(&claim.stderr)
    );

    let start = run_cmd_as(
        &dir,
        &bin,
        "frontend_dev",
        &["--json", "task", "start", &task_id],
    );
    assert!(
        start.status.success(),
        "start should succeed: {}",
        String::from_utf8_lossy(&start.stderr)
    );

    // Multiple active tasks allowed by default
    let task_id_2 = create_task(&dir, &bin, "Second Task", None, None);
    let claim_2 = run_cmd_as(
        &dir,
        &bin,
        "frontend_dev",
        &["--json", "task", "claim", &task_id_2],
    );
    assert!(
        claim_2.status.success(),
        "second claim should succeed by default"
    );

    let start_2 = run_cmd_as(
        &dir,
        &bin,
        "frontend_dev",
        &["--json", "task", "start", &task_id_2],
    );
    assert!(
        start_2.status.success(),
        "second start should succeed by default"
    );
}

#[test]
fn enforce_role_matching_blocks_mismatched_claim_and_handoff() {
    let (dir, bin) = setup_test_project("policy_role_matching");
    init_and_agent(&dir, &bin);

    set_policy(&dir, true, false);

    register_agent(&dir, &bin, "frontend_dev", Some("frontend"));
    register_agent(&dir, &bin, "backend_dev", Some("backend"));

    let task_id = create_task(&dir, &bin, "Backend API Task", Some("backend"), None);

    // 1. Claim by mismatched agent must fail
    let claim_bad = run_cmd_as(
        &dir,
        &bin,
        "frontend_dev",
        &["--json", "task", "claim", &task_id],
    );
    assert_eq!(
        claim_bad.status.code(),
        Some(9),
        "should exit with code 9 (PermissionScope)"
    );
    let err_json = json_stderr(&claim_bad);
    assert_eq!(err_json["error"]["code"], "ROLE_MISMATCH");
    assert!(
        err_json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("does not match required role 'backend'"),
        "error message should explain mismatch: {}",
        err_json["error"]["message"]
    );

    // 2. Claim by matching agent must succeed
    let claim_good = run_cmd_as(
        &dir,
        &bin,
        "backend_dev",
        &["--json", "task", "claim", &task_id],
    );
    assert!(
        claim_good.status.success(),
        "matching role claim should succeed: {}",
        String::from_utf8_lossy(&claim_good.stderr)
    );

    // 3. Test handoff accept
    // Create a handoff from backend_dev to frontend_dev
    let handoff_out = run_cmd_as(
        &dir,
        &bin,
        "backend_dev",
        &[
            "--json",
            "handoff",
            "create",
            "--task",
            &task_id,
            "--target",
            "frontend_dev",
            "--summary",
            "Transferring to frontend",
        ],
    );
    assert!(
        handoff_out.status.success(),
        "handoff create failed: {}",
        String::from_utf8_lossy(&handoff_out.stderr)
    );
    let handoff_id = json_stdout(&handoff_out)["data"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Frontend dev tries to accept handoff, but task requires 'backend'
    let accept_bad = run_cmd_as(
        &dir,
        &bin,
        "frontend_dev",
        &["--json", "handoff", "accept", &handoff_id],
    );
    assert_eq!(
        accept_bad.status.code(),
        Some(9),
        "handoff accept with role mismatch should fail with exit code 9"
    );
    let handoff_err = json_stderr(&accept_bad);
    assert_eq!(handoff_err["error"]["code"], "ROLE_MISMATCH");

    // Register second backend dev who matches the role
    register_agent(&dir, &bin, "backend_dev_2", Some("backend"));
    let accept_good = run_cmd_as(
        &dir,
        &bin,
        "backend_dev_2",
        &["--json", "handoff", "accept", &handoff_id],
    );
    assert!(
        accept_good.status.success(),
        "matching role handoff accept should succeed: {}",
        String::from_utf8_lossy(&accept_good.stderr)
    );
}

#[test]
fn single_active_task_blocks_multiple_in_progress() {
    let (dir, bin) = setup_test_project("policy_single_active");
    init_and_agent(&dir, &bin);

    set_policy(&dir, false, true);

    register_agent(&dir, &bin, "worker", None);

    let t1 = create_task(&dir, &bin, "Task 1", None, None);
    let t2 = create_task(&dir, &bin, "Task 2", None, None);

    // Claim and start task 1 (t1 transitions from ready to in_progress upon claim)
    let claim_1 = run_cmd_as(&dir, &bin, "worker", &["--json", "task", "claim", &t1]);
    assert!(claim_1.status.success());

    // Trying to claim task 2 while task 1 is in_progress must fail
    let claim_2 = run_cmd_as(&dir, &bin, "worker", &["--json", "task", "claim", &t2]);
    assert_eq!(
        claim_2.status.code(),
        Some(3),
        "claim while holding an active task must fail with exit code 3 (StateConflict)"
    );
    let claim_err = json_stderr(&claim_2);
    assert_eq!(claim_err["error"]["code"], "SINGLE_ACTIVE_TASK_VIOLATION");
    assert!(
        claim_err["error"]["message"]
            .as_str()
            .unwrap()
            .contains("already has an active task in progress"),
        "error message should identify violation: {}",
        claim_err["error"]["message"]
    );

    // Complete task 1
    let complete_1 = run_cmd_as(&dir, &bin, "worker", &["--json", "task", "complete", &t1]);
    assert!(
        complete_1.status.success(),
        "complete_1 failed: {}",
        String::from_utf8_lossy(&complete_1.stderr)
    );

    // Now claiming task 2 should succeed
    let claim_2_retry = run_cmd_as(&dir, &bin, "worker", &["--json", "task", "claim", &t2]);
    assert!(
        claim_2_retry.status.success(),
        "claim after previous task completed should succeed: {}",
        String::from_utf8_lossy(&claim_2_retry.stderr)
    );
}

#[test]
fn single_active_task_blocks_start_when_already_assigned() {
    let (dir, bin) = setup_test_project("policy_start_gate");
    init_and_agent(&dir, &bin);

    set_policy(&dir, false, true);

    register_agent(&dir, &bin, "worker", None);
    let t1 = create_task(&dir, &bin, "Task 1", None, Some("worker"));
    let t2 = create_task(&dir, &bin, "Task 2", None, Some("worker"));

    // Start t1 -> in_progress
    let start_1 = run_cmd_as(&dir, &bin, "worker", &["--json", "task", "start", &t1]);
    assert!(
        start_1.status.success(),
        "start_1 failed: {}",
        String::from_utf8_lossy(&start_1.stderr)
    );

    // Now start t2 while t1 is in_progress -> must fail
    let start_2 = run_cmd_as(&dir, &bin, "worker", &["--json", "task", "start", &t2]);
    assert_eq!(
        start_2.status.code(),
        Some(3),
        "start while holding an active task must fail with exit code 3"
    );
    let start_err = json_stderr(&start_2);
    assert_eq!(start_err["error"]["code"], "SINGLE_ACTIVE_TASK_VIOLATION");

    // Complete t1
    let complete_1 = run_cmd_as(&dir, &bin, "worker", &["--json", "task", "complete", &t1]);
    assert!(complete_1.status.success());

    // Now start t2 should succeed
    let start_2_retry = run_cmd_as(&dir, &bin, "worker", &["--json", "task", "start", &t2]);
    assert!(
        start_2_retry.status.success(),
        "start_2 after complete must succeed: {}",
        String::from_utf8_lossy(&start_2_retry.stderr)
    );
}

#[test]
fn policy_alias_task_enforcement_works() {
    let (dir, bin) = setup_test_project("policy_alias");
    init_and_agent(&dir, &bin);

    // Replace [policy] table with [task.enforcement]
    let cfg = dir.join(".carryctx/config.toml");
    let content = std::fs::read_to_string(&cfg).unwrap();
    let updated = content.replace(
        "[policy]\nenforce_role_matching = false\nsingle_active_task = false",
        "[task.enforcement]\nenforce_role_matching = true\nsingle_active_task = true",
    );
    std::fs::write(&cfg, updated).unwrap();

    register_agent(&dir, &bin, "frontend_dev", Some("frontend"));
    let task_id = create_task(&dir, &bin, "Backend Task", Some("backend"), None);

    let claim_bad = run_cmd_as(
        &dir,
        &bin,
        "frontend_dev",
        &["--json", "task", "claim", &task_id],
    );
    assert_eq!(claim_bad.status.code(), Some(9));
    let err_json = json_stderr(&claim_bad);
    assert_eq!(err_json["error"]["code"], "ROLE_MISMATCH");
}
