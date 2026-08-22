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
