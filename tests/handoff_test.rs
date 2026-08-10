mod common;

/// Regression test for https://github.com/Xuepoo/carryctx/issues/60.
///
/// `handoff create --target` accepts "the target agent ULID or role name", but
/// the value was inserted verbatim into `to_agent_id`, failing the agents(id)
/// foreign key for anything but a raw ULID. Targets must resolve by name,
/// ULID, or role before insert.
fn setup(dir: &std::path::Path, bin: &std::path::Path) {
    common::run_cmd(dir, bin, &["init", "--force", "--task-prefix", "HF"]);
    common::run_cmd(
        dir,
        bin,
        &[
            "agent",
            "register",
            "--name",
            "tester",
            "--provider",
            "test",
        ],
    );
    common::run_cmd(
        dir,
        bin,
        &[
            "agent",
            "register",
            "--name",
            "target",
            "--provider",
            "test",
            "--role",
            "codegen",
        ],
    );
    let created = common::run_cmd(
        dir,
        bin,
        &["task", "create", "--title", "Handoff task", "--json"],
    );
    assert!(
        created.status.success(),
        "task create failed: {}",
        String::from_utf8_lossy(&created.stdout)
    );
}

fn task_display_id(stdout: &str) -> String {
    // Envelope: {"schema_version":1,"command":"task.create","success":true,"data":{"display_id":"HF-0001",...
    let start = stdout
        .find("\"display_id\":\"")
        .expect("display_id present")
        + 14;
    let end = stdout[start..].find('"').expect("closing quote") + start;
    stdout[start..end].to_string()
}

#[test]
fn test_handoff_create_with_name_target() {
    let (dir, bin) = common::setup_test_project("handoff_name");
    setup(&dir, &bin);
    let list = common::run_cmd(&dir, &bin, &["task", "list", "--json"]);
    let tid = task_display_id(&String::from_utf8_lossy(&list.stdout));

    let create = common::run_cmd(
        &dir,
        &bin,
        &[
            "handoff",
            "create",
            "--target",
            "target",
            "--task",
            &tid,
            "--summary",
            "by name",
            "--json",
        ],
    );
    let stdout = String::from_utf8_lossy(&create.stdout);
    assert!(
        create.status.success(),
        "handoff create by name failed: {stdout} stderr: {}",
        String::from_utf8_lossy(&create.stderr)
    );
    assert!(
        stdout.contains("\"success\":true"),
        "expected success envelope: {stdout}"
    );
}

#[test]
fn test_handoff_create_with_role_target() {
    let (dir, bin) = common::setup_test_project("handoff_role");
    setup(&dir, &bin);
    let list = common::run_cmd(&dir, &bin, &["task", "list", "--json"]);
    let tid = task_display_id(&String::from_utf8_lossy(&list.stdout));

    let create = common::run_cmd(
        &dir,
        &bin,
        &[
            "handoff",
            "create",
            "--target",
            "codegen",
            "--task",
            &tid,
            "--summary",
            "by role",
            "--json",
        ],
    );
    let stdout = String::from_utf8_lossy(&create.stdout);
    assert!(
        create.status.success(),
        "handoff create by role failed: {stdout} stderr: {}",
        String::from_utf8_lossy(&create.stderr)
    );
    assert!(
        stdout.contains("\"success\":true"),
        "expected success envelope: {stdout}"
    );
}

#[test]
fn test_handoff_create_with_unknown_target_errors_clearly() {
    let (dir, bin) = common::setup_test_project("handoff_unknown");
    setup(&dir, &bin);
    let list = common::run_cmd(&dir, &bin, &["task", "list", "--json"]);
    let tid = task_display_id(&String::from_utf8_lossy(&list.stdout));

    let create = common::run_cmd(
        &dir,
        &bin,
        &[
            "handoff",
            "create",
            "--target",
            "nobody",
            "--task",
            &tid,
            "--summary",
            "boom",
        ],
    );
    let stderr = String::from_utf8_lossy(&create.stderr);
    assert!(
        !create.status.success(),
        "unknown target must fail: {stderr}"
    );
    assert!(
        stderr.contains("not found"),
        "failure must name the missing agent: {stderr}"
    );
}
