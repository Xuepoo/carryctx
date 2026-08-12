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

fn handoff_display_id(stdout: &str) -> String {
    // The JSON envelope contains both the task_id and the handoff's own display_id.
    // Handoff display IDs always start with "HO-", so search for that prefix to
    // skip over any earlier "display_id" field that belongs to a different entity.
    const KEY: &str = "\"display_id\":\"";
    // Anchor on the HO- prefix so an earlier display_id belonging to another
    // entity in the same envelope cannot be picked up, but slice from just after
    // the opening quote so the returned value keeps its prefix.
    let key_at = stdout
        .find(&format!("{KEY}HO-"))
        .expect("handoff display_id present");
    let start = key_at + KEY.len();
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

/// `handoff list` is the session-start check for "what was routed to me", so a
/// list dominated by already-resolved handoffs makes it useless: measured on a
/// real project it returned 7 records of which only 1 was actionable. Pending is
/// therefore the default, with --all and --status to widen deliberately.
#[test]
fn test_handoff_list_defaults_to_pending_only() {
    let (dir, bin) = common::setup_test_project("handoff_list_default");
    setup(&dir, &bin);
    let list = common::run_cmd(&dir, &bin, &["task", "list", "--json"]);
    let tid = task_display_id(&String::from_utf8_lossy(&list.stdout));

    // Two handoffs on the same task: one left pending, one closed.
    let keep = common::run_cmd(
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
            "still open",
            "--json",
        ],
    );
    let keep_id = handoff_display_id(&String::from_utf8_lossy(&keep.stdout));
    let gone = common::run_cmd(
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
            "resolved",
            "--json",
        ],
    );
    let gone_id = handoff_display_id(&String::from_utf8_lossy(&gone.stdout));
    let closed = common::run_cmd(&dir, &bin, &["handoff", "close", &gone_id, "--json"]);
    assert!(
        closed.status.success(),
        "close failed: {}",
        String::from_utf8_lossy(&closed.stdout)
    );

    let default = common::run_cmd(&dir, &bin, &["handoff", "list", "--json"]);
    let out = String::from_utf8_lossy(&default.stdout);
    assert!(
        out.contains(&keep_id),
        "pending handoff {keep_id} must be listed by default: {out}"
    );
    assert!(
        !out.contains(&gone_id),
        "closed handoff {gone_id} must be hidden by default: {out}"
    );

    // --all restores the unfiltered view.
    let all = common::run_cmd(&dir, &bin, &["handoff", "list", "--all", "--json"]);
    let all_out = String::from_utf8_lossy(&all.stdout);
    assert!(
        all_out.contains(&keep_id) && all_out.contains(&gone_id),
        "--all must list both: {all_out}"
    );

    // An explicit --status wins over the pending default.
    let only_closed = common::run_cmd(
        &dir,
        &bin,
        &["handoff", "list", "--status", "closed", "--json"],
    );
    let closed_out = String::from_utf8_lossy(&only_closed.stdout);
    assert!(
        closed_out.contains(&gone_id) && !closed_out.contains(&keep_id),
        "--status closed must list only the closed one: {closed_out}"
    );
}

/// `Open` persists as `"pending"` and `Rejected` as `"declined"`, so a user can
/// legitimately pass either the JSON spelling they read back or the domain name.
#[test]
fn test_handoff_list_status_accepts_both_spellings_and_rejects_junk() {
    let (dir, bin) = common::setup_test_project("handoff_list_status");
    setup(&dir, &bin);
    let list = common::run_cmd(&dir, &bin, &["task", "list", "--json"]);
    let tid = task_display_id(&String::from_utf8_lossy(&list.stdout));
    let created = common::run_cmd(
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
            "p",
            "--json",
        ],
    );
    let id = handoff_display_id(&String::from_utf8_lossy(&created.stdout));

    for spelling in ["pending", "open"] {
        let out = common::run_cmd(
            &dir,
            &bin,
            &["handoff", "list", "--status", spelling, "--json"],
        );
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && text.contains(&id),
            "--status {spelling} must find the pending handoff: {text}"
        );
    }

    let bad = common::run_cmd(&dir, &bin, &["handoff", "list", "--status", "bogus"]);
    let stderr = String::from_utf8_lossy(&bad.stderr);
    assert!(!bad.status.success(), "junk status must fail: {stderr}");
    assert!(
        stderr.contains("bogus") && stderr.contains("pending"),
        "error must name the bad value and the valid set: {stderr}"
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

/// Regression test for https://github.com/Xuepoo/carryctx/issues/75.
///
/// `handoff accept --claim-task` was parsed but destructured away: the task was
/// never claimed. Accepting with --claim-task must claim the handoff's task for
/// the accepting agent and move it to in_progress, mirroring `task claim`.
#[test]
fn test_handoff_accept_claim_task_claims_the_task() {
    let (dir, bin) = common::setup_test_project("handoff_claim");
    setup(&dir, &bin);
    common::run_cmd(
        &dir,
        &bin,
        &[
            "agent",
            "register",
            "--name",
            "acceptor",
            "--provider",
            "test",
        ],
    );
    let list = common::run_cmd(&dir, &bin, &["task", "list", "--json"]);
    let tid = task_display_id(&String::from_utf8_lossy(&list.stdout));

    let created = common::run_cmd(
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
            "claim me",
            "--json",
        ],
    );
    let hid = handoff_display_id(&String::from_utf8_lossy(&created.stdout));

    // The handoff was routed to "target", but any agent may accept; accept as
    // "acceptor" so the claim target is distinct from the test default.
    let accept = common::run_cmd_as(
        &dir,
        &bin,
        "acceptor",
        &["handoff", "accept", &hid, "--claim-task", "--json"],
    );
    assert!(
        accept.status.success(),
        "accept --claim-task failed: {} stderr: {}",
        String::from_utf8_lossy(&accept.stdout),
        String::from_utf8_lossy(&accept.stderr)
    );

    let show = common::run_cmd(&dir, &bin, &["task", "show", &tid, "--json"]);
    assert!(
        show.status.success(),
        "task show failed: {}",
        String::from_utf8_lossy(&show.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(
        value["data"]["status"], "in_progress",
        "task must be in_progress after accept --claim-task: {value}"
    );
    let owner = value["data"]["owner_agent_id"]
        .as_str()
        .expect("task must be claimed by an agent");
    let agents = common::run_cmd(&dir, &bin, &["agent", "list", "--json"]);
    let agents_value: serde_json::Value = serde_json::from_slice(&agents.stdout).unwrap();
    let acceptor_id = agents_value["data"]
        .as_array()
        .expect("agent list array")
        .iter()
        .find(|a| a["name"] == "acceptor")
        .expect("acceptor registered")
        .get("id")
        .and_then(|v| v.as_str())
        .expect("acceptor id present");
    assert_eq!(
        owner, acceptor_id,
        "task must be owned by the accepting agent: {value}"
    );
}

/// Regression test for https://github.com/Xuepoo/carryctx/issues/76.
///
/// `handoff show`/`accept` on a missing ref short-circuited with a bare
/// `ExitCode`, skipping the standard error envelope. Machine consumers must get
/// the standard `success:false` envelope on stderr with exit code 7, like
/// `task show` does.
#[test]
fn test_handoff_show_missing_returns_standard_error_envelope() {
    let (dir, bin) = common::setup_test_project("handoff_show_missing");
    setup(&dir, &bin);

    let show = common::run_cmd(&dir, &bin, &["handoff", "show", "HO-9999", "--json"]);
    assert!(!show.status.success(), "missing handoff must fail");
    assert_eq!(show.status.code(), Some(7), "exit code must be 7");
    let stderr: serde_json::Value = serde_json::from_slice(&show.stderr).unwrap_or_else(|e| {
        panic!(
            "stderr must be a JSON envelope: {e}: {}",
            String::from_utf8_lossy(&show.stderr)
        )
    });
    assert_eq!(stderr["success"], false);
    assert_eq!(stderr["command"], "handoff.show");
    assert_eq!(stderr["error"]["code"], "RESOURCE_NOT_FOUND");
}

#[test]
fn test_handoff_accept_missing_returns_standard_error_envelope() {
    let (dir, bin) = common::setup_test_project("handoff_accept_missing");
    setup(&dir, &bin);

    let accept = common::run_cmd(&dir, &bin, &["handoff", "accept", "HO-9999", "--json"]);
    assert!(!accept.status.success(), "missing handoff must fail");
    assert_eq!(accept.status.code(), Some(7), "exit code must be 7");
    let stderr: serde_json::Value = serde_json::from_slice(&accept.stderr).unwrap_or_else(|e| {
        panic!(
            "stderr must be a JSON envelope: {e}: {}",
            String::from_utf8_lossy(&accept.stderr)
        )
    });
    assert_eq!(stderr["success"], false);
    assert_eq!(stderr["command"], "handoff.accept");
    assert_eq!(stderr["error"]["code"], "RESOURCE_NOT_FOUND");
}
