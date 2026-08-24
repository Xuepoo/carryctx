mod common;

/// CTX-0071 / issue #104: agent identity integrity.
///
/// - Duplicate names must be rejected with a helpful error (the schema has a
///   UNIQUE(project_id, name) constraint; the use case must pre-check and map
///   violations to an actionable message instead of a raw database error).
/// - Renaming into a taken name must be rejected.
/// - Deactivated agents must not resolve: they cannot act.
#[test]
fn test_duplicate_agent_name_rejected_with_helpful_error() {
    let (dir, bin) = common::setup_test_project("agent_dup_name");
    common::run_cmd(&dir, &bin, &["init", "--force"]);
    let first = common::run_cmd(
        &dir,
        &bin,
        &["agent", "register", "--name", "alice", "--provider", "test"],
    );
    assert!(first.status.success(), "first register should succeed");

    let second = common::run_cmd(
        &dir,
        &bin,
        &["agent", "register", "--name", "alice", "--provider", "test"],
    );
    assert!(!second.status.success(), "duplicate register must fail");
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("STATE_CONFLICT"),
        "duplicate register must report STATE_CONFLICT: {stderr}"
    );
    assert!(
        stderr.to_lowercase().contains("rename") || stderr.to_lowercase().contains("already"),
        "duplicate register error must be actionable: {stderr}"
    );

    // Renaming bob onto alice's taken name must fail too.
    let bob = common::run_cmd(
        &dir,
        &bin,
        &["agent", "register", "--name", "bob", "--provider", "test"],
    );
    assert!(bob.status.success(), "bob register should succeed");
    let rename = common::run_cmd(
        &dir,
        &bin,
        &["agent", "rename", "bob", "--name", "alice", "--json"],
    );
    assert!(
        !rename.status.success(),
        "rename onto a taken name must fail"
    );
    assert!(
        String::from_utf8_lossy(&rename.stderr).contains("STATE_CONFLICT"),
        "rename conflict must report STATE_CONFLICT: {}",
        String::from_utf8_lossy(&rename.stderr)
    );
}

#[test]
fn test_agent_deactivate_persists() {
    // CTX-0074: `agent deactivate` used to print success without committing
    // its transaction, so the agent silently stayed active. The command must
    // persist the deactivation.
    let (dir, bin) = common::setup_test_project("agent_deactivate_persists");
    common::run_cmd(&dir, &bin, &["init", "--force"]);
    let reg = common::run_cmd(
        &dir,
        &bin,
        &["agent", "register", "--name", "ghost", "--provider", "test"],
    );
    assert!(reg.status.success(), "ghost register should succeed");

    let deact = common::run_cmd(&dir, &bin, &["agent", "deactivate", "ghost"]);
    assert!(
        deact.status.success(),
        "deactivate should succeed: {}",
        String::from_utf8_lossy(&deact.stderr)
    );

    // Prove persistence: a fresh connection must observe the deactivated
    // status (the old defect rolled the UPDATE back before process exit).
    let db_path = dir.join(".git/carryctx/state.sqlite");
    let conn = rusqlite::Connection::open(&db_path).expect("open state db");
    let status: String = conn
        .query_row(
            "SELECT status FROM agents WHERE name = 'ghost'",
            [],
            |row| row.get(0),
        )
        .expect("ghost row must exist");
    drop(conn);
    assert_eq!(
        status, "deactivated",
        "deactivate must persist the status change"
    );
}

#[test]
fn test_deactivated_agent_cannot_resolve() {
    let (dir, bin) = common::setup_test_project("agent_deactivated");
    common::run_cmd(&dir, &bin, &["init", "--force"]);
    let reg = common::run_cmd(
        &dir,
        &bin,
        &["agent", "register", "--name", "ghost", "--provider", "test"],
    );
    assert!(reg.status.success(), "ghost register should succeed");

    let deact = common::run_cmd(&dir, &bin, &["agent", "deactivate", "ghost"]);
    assert!(deact.status.success(), "ghost deactivate should succeed");

    // A deactivated agent must not act: resolver rejects it instead of
    // silently resolving the deactivated row.
    let out = common::run_cmd_as(
        &dir,
        &bin,
        "ghost",
        &["task", "create", "--title", "ghost work", "--json"],
    );
    assert!(
        !out.status.success(),
        "deactivated agent must not be able to act"
    );
}

#[test]
fn test_deactivated_agent_rejected_by_command_layer_resolver() {
    // CTX-0074: the early main.rs resolver (resolve_agent_id) resolved
    // deactivated rows happily, so actors slipped through at the command
    // layer even though the runtime resolver rejects them.
    let (dir, bin) = common::setup_test_project("agent_deact_cmd_resolver");
    common::run_cmd(&dir, &bin, &["init", "--force"]);
    let reg = common::run_cmd(
        &dir,
        &bin,
        &["agent", "register", "--name", "ghost", "--provider", "test"],
    );
    assert!(reg.status.success(), "ghost register should succeed");
    let deact = common::run_cmd(&dir, &bin, &["agent", "deactivate", "ghost"]);
    assert!(deact.status.success(), "ghost deactivate should succeed");

    // An explicit actor reference to a deactivated agent must be rejected
    // with a message that says so — not silently resolved and acted on.
    let out = common::run_cmd(
        &dir,
        &bin,
        &["--agent", "ghost", "session", "start", "--json"],
    );
    assert!(
        !out.status.success(),
        "deactivated actor must not start a session"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("deactivated"),
        "rejection must explain the deactivation: {combined}"
    );
}

/// CTX-0080: `event list --agent <ref>` swallowed resolver rejections with
/// `.ok()`, so a deactivated (or unknown) agent silently widened the filter
/// to ALL events. `search --assignee` errors loudly through the standard
/// envelope; both commands must behave identically: loud error, no fallback.
#[test]
fn test_event_list_agent_filter_matches_search_assignee_loudness() {
    let (dir, bin) = common::setup_test_project("event_agent_loud");
    common::run_cmd(&dir, &bin, &["init", "--force"]);
    common::run_cmd(
        &dir,
        &bin,
        &["agent", "register", "--name", "alice", "--provider", "test"],
    );
    common::run_cmd(
        &dir,
        &bin,
        &["agent", "register", "--name", "bob", "--provider", "test"],
    );
    let create = common::run_cmd_as(
        &dir,
        &bin,
        "alice",
        &["task", "create", "--title", "alice work", "--json"],
    );
    assert!(create.status.success(), "task create should succeed");

    let deact = common::run_cmd(&dir, &bin, &["agent", "deactivate", "bob"]);
    assert!(deact.status.success(), "bob deactivate should succeed");

    let event_error_code = |args: &[&str]| {
        let out = std::process::Command::new(&bin)
            .args(args)
            .env_remove("CARRYCTX_AGENT")
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            !out.status.success(),
            "expected failure for {args:?}, got success"
        );
        // Error envelopes print on stderr by contract, even in JSON mode.
        let value: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&out.stderr))
            .expect("error envelope on stderr");
        (
            value["error"]["code"].as_str().unwrap_or("").to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };

    // Deactivated reference: both commands reject with PERMISSION_SCOPE.
    let (event_code, event_msg) =
        event_error_code(&["--format", "json", "event", "list", "--agent", "bob"]);
    assert_eq!(event_code, "PERMISSION_SCOPE");
    assert!(event_msg.contains("deactivated"));
    let (search_code, _) =
        event_error_code(&["--format", "json", "search", "work", "--assignee", "bob"]);
    assert_eq!(search_code, "PERMISSION_SCOPE");

    // Unknown reference: both commands reject with RESOURCE_NOT_FOUND.
    let (event_unknown, _) =
        event_error_code(&["--format", "json", "event", "list", "--agent", "nobody"]);
    assert_eq!(event_unknown, "RESOURCE_NOT_FOUND");
    let (search_unknown, _) =
        event_error_code(&["--format", "json", "search", "work", "--assignee", "nobody"]);
    assert_eq!(search_unknown, "RESOURCE_NOT_FOUND");
}
