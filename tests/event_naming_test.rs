mod common;

/// Task transition event types are a public contract (documented in
/// `carryctx-docs/requirements.md`). The name is derived from the transition
/// verb, so verbs already ending in `e` must not gain a doubled vowel:
/// `complete` -> `task.completed`, not `task.completeed`.
#[test]
fn test_task_transition_event_types_are_correctly_spelled() {
    let (dir, bin) = common::setup_test_project("event_naming");
    common::init_and_agent(&dir, &bin);

    let create = common::run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", "Event naming task"],
    );
    assert!(create.status.success(), "task create should succeed");

    // Drive the two transitions whose verbs end in `e`: release and complete.
    for args in [
        &["task", "claim", "CTX-0001"][..],
        &["task", "release", "CTX-0001"][..],
        &["task", "claim", "CTX-0001"][..],
        &["task", "start", "CTX-0001"][..],
        &["task", "complete", "CTX-0001"][..],
    ] {
        let out = common::run_cmd(&dir, &bin, args);
        assert!(
            out.status.success(),
            "{:?} should succeed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let events = common::run_cmd(&dir, &bin, &["event", "list", "--limit", "100", "--json"]);
    assert!(events.status.success(), "event list should succeed");
    let stdout = String::from_utf8_lossy(&events.stdout);

    for bad in ["task.completeed", "task.releaseed"] {
        assert!(
            !stdout.contains(bad),
            "event log must not contain misspelled type {bad}: {stdout}"
        );
    }

    for expected in ["task.completed", "task.released"] {
        assert!(
            stdout.contains(expected),
            "event log should contain {expected}: {stdout}"
        );
    }
}

/// Filtering by the documented event type must return the recorded events.
#[test]
fn test_event_filter_matches_documented_task_completed_type() {
    let (dir, bin) = common::setup_test_project("event_filter_naming");
    common::init_and_agent(&dir, &bin);

    common::run_cmd(&dir, &bin, &["task", "create", "--title", "Filter task"]);
    for args in [
        &["task", "claim", "CTX-0001"][..],
        &["task", "start", "CTX-0001"][..],
        &["task", "complete", "CTX-0001"][..],
    ] {
        let out = common::run_cmd(&dir, &bin, args);
        assert!(out.status.success(), "{args:?} should succeed");
    }

    let filtered = common::run_cmd(
        &dir,
        &bin,
        &["event", "list", "--event-type", "task.completed", "--json"],
    );
    assert!(filtered.status.success(), "event list --event-type ok");
    let stdout = String::from_utf8_lossy(&filtered.stdout);
    assert!(
        stdout.contains("task.completed"),
        "filtering by the documented type must return the completion event: {stdout}"
    );
}

#[test]
fn test_event_filter_matches_documented_task_cancelled_type() {
    let (dir, bin) = common::setup_test_project("event_filter_cancelled_naming");
    common::init_and_agent(&dir, &bin);

    common::run_cmd(&dir, &bin, &["task", "create", "--title", "Cancel task"]);
    let cancelled = common::run_cmd(
        &dir,
        &bin,
        &[
            "task",
            "cancel",
            "CTX-0001",
            "--reason",
            "test cancellation",
        ],
    );
    assert!(cancelled.status.success(), "task cancel should succeed");

    let filtered = common::run_cmd(
        &dir,
        &bin,
        &["event", "list", "--event-type", "task.cancelled", "--json"],
    );
    assert!(filtered.status.success(), "event list --event-type ok");
    let stdout = String::from_utf8_lossy(&filtered.stdout);
    assert!(stdout.contains("task.cancelled"));
}
