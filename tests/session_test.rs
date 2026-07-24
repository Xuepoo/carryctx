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
