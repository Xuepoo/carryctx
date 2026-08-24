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
