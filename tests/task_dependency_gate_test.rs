mod common;

/// CTX-0071 / issue #98: the Complete transition ignored
/// `strong_dependencies_complete`, so a task could be completed while strong
/// blockers were still open — claim and start are gated, complete must be too.
#[test]
fn test_complete_denied_while_strong_dependencies_open() {
    let (dir, bin) = common::setup_test_project("complete_dep_gate");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "DEP"]);
    common::init_and_agent(&dir, &bin);

    // blocker -> main dependency chain
    for title in ["blocker", "main"] {
        let out = common::run_cmd(&dir, &bin, &["task", "create", "--title", title, "--json"]);
        assert!(out.status.success(), "create {title} failed");
    }

    let dep_id = task_display_id_by_title(&dir, &bin, "blocker");
    let main_id = task_display_id_by_title(&dir, &bin, "main");

    let out = common::run_cmd(
        &dir,
        &bin,
        &["task", "depend", &main_id, "--on", &dep_id, "--json"],
    );
    assert!(out.status.success(), "depend failed: {out:?}");

    // Completing the blocker promotes main to ready.
    let claim = common::run_cmd(&dir, &bin, &["task", "claim", &dep_id, "--json"]);
    assert!(
        claim.status.success(),
        "claim blocker: {}",
        String::from_utf8_lossy(&claim.stderr)
    );
    let complete_blocker = common::run_cmd(&dir, &bin, &["task", "complete", &dep_id, "--json"]);
    assert!(
        complete_blocker.status.success(),
        "complete blocker: {}",
        String::from_utf8_lossy(&complete_blocker.stderr)
    );

    let claim_main = common::run_cmd(&dir, &bin, &["task", "claim", &main_id, "--json"]);
    assert!(claim_main.status.success(), "claim main");

    // Add a NEW open strong dependency after the claim: completing main must
    // now be denied until that blocker completes.
    let late = common::run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", "late blocker", "--json"],
    );
    assert!(late.status.success());
    let late_id = task_display_id_by_title(&dir, &bin, "late blocker");

    let depend = common::run_cmd(
        &dir,
        &bin,
        &["task", "depend", &main_id, "--on", &late_id, "--json"],
    );
    assert!(depend.status.success(), "late depend failed");

    let complete_main = common::run_cmd(&dir, &bin, &["task", "complete", &main_id, "--json"]);
    assert!(
        !complete_main.status.success(),
        "complete with open strong dependencies must fail"
    );
    assert!(
        String::from_utf8_lossy(&complete_main.stderr).contains("DEPENDENCY_INCOMPLETE"),
        "must report DEPENDENCY_INCOMPLETE: {}",
        String::from_utf8_lossy(&complete_main.stderr)
    );

    // Finish the late blocker; now completion succeeds.
    let claim_late = common::run_cmd(&dir, &bin, &["task", "claim", &late_id, "--json"]);
    assert!(claim_late.status.success(), "claim late blocker");
    let complete_late = common::run_cmd(&dir, &bin, &["task", "complete", &late_id, "--json"]);
    assert!(
        complete_late.status.success(),
        "complete late blocker: {}",
        String::from_utf8_lossy(&complete_late.stderr)
    );

    let complete_main = common::run_cmd(&dir, &bin, &["task", "complete", &main_id, "--json"]);
    assert!(
        complete_main.status.success(),
        "complete after blockers done: {}",
        String::from_utf8_lossy(&complete_main.stderr)
    );
}

fn task_display_id_by_title(dir: &std::path::Path, bin: &std::path::Path, title: &str) -> String {
    let list = common::run_cmd(dir, bin, &["task", "list", "--json"]);
    assert!(list.status.success(), "task list failed");
    let value: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&list.stdout)).expect("valid json envelope");
    let tasks = value["data"].as_array().expect("task list array");
    tasks
        .iter()
        .find(|t| t["title"] == serde_json::Value::String(title.to_string()))
        .unwrap_or_else(|| panic!("task with title '{title}' not found in {value}"))["display_id"]
        .as_str()
        .expect("display id")
        .to_string()
}

/// CTX-0071 / issue #98: `create --status` accepted in_progress/review/blocked/
/// completed/cancelled and bypassed dependency gating entirely — including
/// minting an already-completed task. Only planned/ready may be minted.
#[test]
fn test_create_rejects_non_initial_statuses() {
    let (dir, bin) = common::setup_test_project("create_status_whitelist");
    common::init_and_agent(&dir, &bin);

    for status in ["in_progress", "review", "blocked", "completed", "cancelled"] {
        let out = common::run_cmd(
            &dir,
            &bin,
            &[
                "task",
                "create",
                "--title",
                &format!("bad status {status}"),
                "--status",
                status,
                "--json",
            ],
        );
        assert!(
            !out.status.success(),
            "create --status {status} must be rejected"
        );
    }

    // planned and ready remain creatable.
    let planned = common::run_cmd(
        &dir,
        &bin,
        &[
            "task",
            "create",
            "--title",
            "planned ok",
            "--status",
            "planned",
            "--json",
        ],
    );
    assert!(
        planned.status.success(),
        "planned creation must stay allowed: {}",
        String::from_utf8_lossy(&planned.stderr)
    );
    let ready = common::run_cmd(
        &dir,
        &bin,
        &[
            "task", "create", "--title", "ready ok", "--status", "ready", "--json",
        ],
    );
    assert!(
        ready.status.success(),
        "ready creation (no deps) must stay allowed: {}",
        String::from_utf8_lossy(&ready.stderr)
    );
}
