mod common;

#[test]
fn test_full_workflow_e2e() {
    let (dir, bin) = common::setup_test_project("e2e_full");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "E2E"]);
    common::run_cmd(
        &dir,
        &bin,
        &[
            "agent",
            "register",
            "--name",
            "tester",
            "--provider",
            "test",
        ],
    );

    // Session start
    let s = common::run_cmd(&dir, &bin, &["session", "start", "--json"]);
    assert!(s.status.success(), "session start");

    // Task create
    let t = common::run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", "E2E task", "--json"],
    );
    assert!(t.status.success(), "task create");

    // Task claim
    let c = common::run_cmd(&dir, &bin, &["task", "claim", "E2E-0001", "--json"]);
    assert!(c.status.success(), "task claim");

    // Progress todo
    let p = common::run_cmd(
        &dir,
        &bin,
        &[
            "progress", "todo", "--task", "E2E-0001", "Step one", "--json",
        ],
    );
    assert!(p.status.success(), "progress todo");

    // Checkpoint
    let cp = common::run_cmd(
        &dir,
        &bin,
        &[
            "checkpoint",
            "--task",
            "E2E-0001",
            "--done",
            "Step one done",
            "--json",
        ],
    );
    assert!(cp.status.success(), "checkpoint create");

    // Stats
    let st = common::run_cmd(&dir, &bin, &["stats", "--json"]);
    assert!(st.status.success(), "stats");

    // Task complete
    let done = common::run_cmd(&dir, &bin, &["task", "complete", "E2E-0001", "--json"]);
    assert!(done.status.success(), "task complete");

    // Session end
    let se = common::run_cmd(&dir, &bin, &["session", "end", "--json"]);
    assert!(se.status.success(), "session end");
}

#[test]
fn compact_context_projects_progress_without_changing_json_envelope() {
    let (dir, bin) = common::setup_test_project("context_compact");
    common::init_and_agent(&dir, &bin);
    let task = common::run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", "context task", "--json"],
    );
    assert!(task.status.success(), "task create: {:?}", task.stderr);

    for index in 0..40 {
        let result = common::run_cmd(
            &dir,
            &bin,
            &[
                "progress",
                "todo",
                "--task",
                "CTX-0001",
                &format!("resume item {index}"),
                "--json",
            ],
        );
        assert!(
            result.status.success(),
            "progress create: {:?}",
            result.stderr
        );
    }

    let full = common::run_cmd(&dir, &bin, &["context", "--task", "CTX-0001", "--json"]);
    let compact = common::run_cmd(
        &dir,
        &bin,
        &["context", "--task", "CTX-0001", "--compact", "--json"],
    );
    assert!(full.status.success(), "full context: {:?}", full.stderr);
    assert!(
        compact.status.success(),
        "compact context: {:?}",
        compact.stderr
    );

    let full_json: serde_json::Value = serde_json::from_slice(&full.stdout).unwrap();
    let compact_json: serde_json::Value = serde_json::from_slice(&compact.stdout).unwrap();
    assert_eq!(compact_json["schema_version"], 1);
    assert_eq!(compact_json["command"], "context");
    assert_eq!(compact_json["success"], true);
    assert_eq!(full_json["data"]["progress"].as_array().unwrap().len(), 40);
    assert_eq!(
        compact_json["data"]["progress"].as_array().unwrap().len(),
        40
    );
    assert!(compact.stdout.len() < full.stdout.len() / 2);
    assert_eq!(
        compact_json["data"]["progress"][0]["content"],
        "resume item 0"
    );
    assert!(
        compact_json["data"]["progress"][0]
            .get("updated_at")
            .is_none()
    );
}
