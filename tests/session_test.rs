mod common;

#[test]
fn test_full_session_lifecycle() {
    let (dir, bin) = common::setup_test_project("session_test");

    common::init_and_agent(&dir, &bin);

    // Start session
    let start = common::run_cmd(&dir, &bin, &["session", "start"]);
    assert!(start.status.success(), "session start should succeed");

    // Current session
    let current = common::run_cmd(&dir, &bin, &["session", "current", "--json"]);
    assert!(current.status.success(), "session current should succeed");
    let stdout = String::from_utf8_lossy(&current.stdout);
    assert!(
        stdout.contains("active"),
        "current session should be active"
    );

    // Pause session
    let pause = common::run_cmd(&dir, &bin, &["session", "pause", "--json"]);
    assert!(pause.status.success(), "session pause should succeed");
    let stdout = String::from_utf8_lossy(&pause.stdout);
    assert!(
        stdout.contains("paused"),
        "paused session should show paused state"
    );

    // Resume session
    let resume = common::run_cmd(&dir, &bin, &["session", "resume", "--json"]);
    assert!(resume.status.success(), "session resume should succeed");

    // End session
    let end = common::run_cmd(&dir, &bin, &["session", "end", "--json"]);
    assert!(end.status.success(), "session end should succeed");
}

#[test]
fn test_session_list() {
    let (dir, bin) = common::setup_test_project("session_list_test");
    common::init_and_agent(&dir, &bin);
    common::run_cmd(&dir, &bin, &["session", "start"]);

    let list = common::run_cmd(&dir, &bin, &["session", "list", "--json"]);
    assert!(list.status.success(), "session list should succeed");
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(
        stdout.contains("active") || stdout.contains("ended"),
        "list should contain session state"
    );
}

#[test]
fn test_session_superseded_event_points_to_successor_session() {
    let (dir, bin) = common::setup_test_project("session_superseded_event");
    common::init_and_agent(&dir, &bin);

    let first = common::run_cmd(&dir, &bin, &["session", "start", "--json"]);
    assert!(first.status.success(), "first session start should succeed");
    let first_json: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    let first_id = first_json["data"]["id"].as_str().unwrap();

    let second = common::run_cmd(&dir, &bin, &["session", "start", "--json"]);
    assert!(
        second.status.success(),
        "second session start should succeed"
    );
    let second_json: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    let second_id = second_json["data"]["id"].as_str().unwrap();
    assert_ne!(first_id, second_id);

    let events = common::run_cmd(
        &dir,
        &bin,
        &[
            "event",
            "list",
            "--session",
            first_id,
            "--event-type",
            "session.ended",
            "--json",
        ],
    );
    assert!(events.status.success(), "event list should succeed");
    let events_json: serde_json::Value = serde_json::from_slice(&events.stdout).unwrap();
    let event_list = events_json["data"]["events"].as_array().unwrap();
    assert!(
        !event_list.is_empty(),
        "supersession event should be recorded"
    );
    let superseded_by = event_list[0]["payload"]["superseded_by"].as_str().unwrap();

    assert_eq!(superseded_by, second_id);
    let successor = common::run_cmd(&dir, &bin, &["session", "show", superseded_by, "--json"]);
    assert!(successor.status.success(), "successor session should exist");
}

/// CTX-0071 / issue #104: end/pause/resume accepted any agent_id without
/// verifying session ownership, so any agent could kill another agent's
/// active session. Only the owning agent (by name or ULID) may transition it.
#[test]
fn test_session_transitions_require_owner_agent() {
    let (dir, bin) = common::setup_test_project("session_ownership");
    common::run_cmd(&dir, &bin, &["init", "--force"]);
    for name in ["alice", "bob"] {
        let out = common::run_cmd(
            &dir,
            &bin,
            &["agent", "register", "--name", name, "--provider", "test"],
        );
        assert!(out.status.success(), "register {name} failed");
    }

    // Alice owns the session.
    let start = common::run_cmd_as(&dir, &bin, "alice", &["session", "start", "--json"]);
    assert!(
        start.status.success(),
        "alice start: {}",
        String::from_utf8_lossy(&start.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&start.stdout).unwrap();
    let sid = value["data"]["id"]
        .as_str()
        .expect("session id")
        .to_string();

    // Bob must not be able to pause, resume, or end alice's session.
    for args in [
        vec!["session", "pause", "--json"],
        vec!["session", "end", "--json"],
    ] {
        let out = common::run_cmd_as(&dir, &bin, "bob", &args);
        assert!(
            !out.status.success(),
            "bob must not be able to {:?} alice's session",
            args
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("PERMISSION_SCOPE"),
            "foreign-agent {} must report PERMISSION_SCOPE: {}",
            args[0],
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // The session must still be owned and active by alice.
    let show = common::run_cmd(&dir, &bin, &["session", "show", &sid, "--json"]);
    assert!(show.status.success());
    let value: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(
        value["data"]["state"], "active",
        "session state after bob's attempts: {value}"
    );

    // The owner can still pause/resume/end — including referencing herself by name.
    let pause = common::run_cmd_as(&dir, &bin, "alice", &["session", "pause", "--json"]);
    assert!(
        pause.status.success(),
        "owner pause: {}",
        String::from_utf8_lossy(&pause.stderr)
    );
    let resume = common::run_cmd_as(&dir, &bin, "alice", &["session", "resume", "--json"]);
    assert!(
        resume.status.success(),
        "owner resume: {}",
        String::from_utf8_lossy(&resume.stderr)
    );

    // Bob still cannot resume a paused session he does not own.
    let out = common::run_cmd_as(&dir, &bin, "bob", &["session", "resume", "--json"]);
    assert!(!out.status.success(), "bob must not resume alice's session");

    let end = common::run_cmd_as(&dir, &bin, "alice", &["session", "end", "--json"]);
    assert!(
        end.status.success(),
        "owner end: {}",
        String::from_utf8_lossy(&end.stderr)
    );
}

#[test]
fn test_session_start_reuse_returns_active_session() {
    // CTX-0074: `--reuse` was parsed but bound to `_` and never read. The
    // documented semantics: re-use the currently active session instead of
    // superseding it with a fresh one.
    let (dir, bin) = common::setup_test_project("session_reuse");
    common::init_and_agent(&dir, &bin);

    let first = common::run_cmd(&dir, &bin, &["session", "start", "--json"]);
    assert!(first.status.success(), "first start should succeed");
    let first_id =
        serde_json::from_slice::<serde_json::Value>(&first.stdout).unwrap()["data"]["id"]
            .as_str()
            .unwrap()
            .to_string();

    let reused = common::run_cmd(&dir, &bin, &["session", "start", "--reuse", "--json"]);
    assert!(reused.status.success(), "reuse start should succeed");
    let reused_json: serde_json::Value = serde_json::from_slice(&reused.stdout).unwrap();
    assert_eq!(
        reused_json["data"]["id"].as_str().unwrap(),
        first_id,
        "--reuse must return the existing active session"
    );

    // Still exactly one session for the agent — no supersede happened.
    let list = common::run_cmd(&dir, &bin, &["session", "list", "--json"]);
    let list_json: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    let sessions = list_json["data"].as_array().expect("session array");
    assert_eq!(sessions.len(), 1, "reuse must not create a new session");

    // Without --reuse the default superseding behavior is unchanged.
    let second = common::run_cmd(&dir, &bin, &["session", "start", "--json"]);
    assert!(second.status.success(), "plain restart should succeed");
    let second_id =
        serde_json::from_slice::<serde_json::Value>(&second.stdout).unwrap()["data"]["id"]
            .as_str()
            .unwrap()
            .to_string();
    assert_ne!(second_id, first_id, "plain start must create a new session");

    // And --reuse with no active session falls back to creating one.
    common::run_cmd(&dir, &bin, &["session", "end", "--json"]);
    let fallback = common::run_cmd(&dir, &bin, &["session", "start", "--reuse", "--json"]);
    assert!(
        fallback.status.success(),
        "--reuse without an active session must still start one"
    );
}

/// CTX-0076 / issue #105: `session abandon --reason` parsed the reason then
/// discarded it, and the implementation reused end_session — recording a clean
/// `ended` state contrary to the docs (abandon must stay distinct from a clean
/// end). The reason must be persisted in the audit event payload and in the
/// session record's summary, and the session must land in `abandoned`.
#[test]
fn test_session_abandon_persists_reason_and_distinct_state() {
    let (dir, bin) = common::setup_test_project("session_abandon_reason");
    common::init_and_agent(&dir, &bin);

    let start = common::run_cmd(&dir, &bin, &["session", "start"]);
    assert!(start.status.success(), "session start should succeed");

    let abandon = common::run_cmd(
        &dir,
        &bin,
        &[
            "--json",
            "session",
            "abandon",
            "--reason",
            "fatal build failure",
        ],
    );
    assert!(
        abandon.status.success(),
        "abandon should succeed: {} {}",
        String::from_utf8_lossy(&abandon.stdout),
        String::from_utf8_lossy(&abandon.stderr)
    );
    let stdout = String::from_utf8_lossy(&abandon.stdout);
    assert!(
        stdout.contains("abandoned"),
        "the record must show the abandoned state: {stdout}"
    );

    // Distinct from a clean end: session.abandoned event with the reason.
    let events = std::process::Command::new(&bin)
        .args(["event", "list", "--limit", "100", "--json"])
        .env_remove("CARRYCTX_AGENT")
        .current_dir(&dir)
        .output()
        .expect("event list should execute");
    assert!(events.status.success());
    let value: serde_json::Value = serde_json::from_slice(&events.stdout).unwrap();
    let abandoned: Vec<_> = value["data"]["events"]
        .as_array()
        .expect("events array")
        .iter()
        .filter(|e| e["event_type"] == "session.abandoned")
        .collect();
    assert_eq!(
        abandoned.len(),
        1,
        "exactly one session.abandoned event: {value}"
    );
    assert_eq!(
        abandoned[0]["payload"]["reason"], "fatal build failure",
        "the reason must survive verbatim in the event payload: {value}"
    );
    // A clean-end event for this session would contradict the semantics.
    let clean_end: Vec<_> = value["data"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["event_type"] == "session.ended")
        .collect();
    assert!(
        clean_end.is_empty(),
        "abandon must not record session.ended: {value}"
    );
}

/// CTX-0076 / issue #105: `session start` hardcoded a "default" agent fallback,
/// ignoring `[agent] default_name`. The configured name must be used when no
/// explicit agent is given.
#[test]
fn test_session_start_honors_configured_default_agent_name() {
    let (dir, bin) = common::setup_test_project("session_config_default");
    common::run_cmd(&dir, &bin, &["init", "--force"]);
    common::run_cmd(
        &dir,
        &bin,
        &[
            "agent",
            "register",
            "--name",
            "claude-core",
            "--provider",
            "test",
        ],
    );
    std::fs::write(
        dir.join(".carryctx/config.toml"),
        "[agent]\ndefault_name = \"claude-core\"\n",
    )
    .unwrap();

    // No --agent, no CARRYCTX_AGENT: the config default must resolve.
    let start = std::process::Command::new(&bin)
        .args(["session", "start", "--json"])
        .env_remove("CARRYCTX_AGENT")
        .current_dir(&dir)
        .output()
        .expect("session start should execute");
    assert!(
        start.status.success(),
        "session start must honor [agent] default_name: {} {}",
        String::from_utf8_lossy(&start.stdout),
        String::from_utf8_lossy(&start.stderr)
    );

    // Resolve claude-core's ULID and confirm the session bound to it.
    let agents = std::process::Command::new(&bin)
        .args(["agent", "list", "--json"])
        .env_remove("CARRYCTX_AGENT")
        .current_dir(&dir)
        .output()
        .expect("agent list should execute");
    let agents_value: serde_json::Value = serde_json::from_slice(&agents.stdout).unwrap();
    let expected_id = agents_value["data"]
        .as_array()
        .and_then(|list| {
            list.iter()
                .find(|a| a["name"] == "claude-core")
                .and_then(|a| a["id"].as_str())
        })
        .expect("claude-core must be listed");

    let start_value: serde_json::Value = serde_json::from_slice(&start.stdout).unwrap();
    assert_eq!(
        start_value["data"]["agent_id"], expected_id,
        "the session must bind to the configured default agent: {start_value}"
    );
}
