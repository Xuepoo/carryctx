mod common;

#[test]
fn test_checkpoint_create_and_list() {
    let (dir, bin) = common::setup_test_project("checkpoint_test");
    let init_res = common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "CK"]);
    if !init_res.status.success() {
        panic!("init failed: {}", String::from_utf8_lossy(&init_res.stderr));
    }
    let agent_reg = common::run_cmd(
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
    if !agent_reg.status.success() {
        panic!(
            "agent register failed: {}",
            String::from_utf8_lossy(&agent_reg.stderr)
        );
    }
    let task_create = common::run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", "Checkpoint test task"],
    );
    if !task_create.status.success() {
        panic!(
            "task create failed: {}",
            String::from_utf8_lossy(&task_create.stderr)
        );
    }

    // Create checkpoint
    let cp = common::run_cmd(
        &dir,
        &bin,
        &[
            "checkpoint",
            "--task",
            "CK-0001",
            "--done",
            "First item",
            "--remaining",
            "Second item",
            "--json",
        ],
    );
    if !cp.status.success() {
        panic!(
            "checkpoint create failed: {}",
            String::from_utf8_lossy(&cp.stderr)
        );
    }
    let stdout = String::from_utf8_lossy(&cp.stdout);
    assert!(
        stdout.contains("First item"),
        "checkpoint should contain done items"
    );

    // List checkpoints
    let list = common::run_cmd(&dir, &bin, &["checkpoint", "list", "--json"]);
    assert!(list.status.success(), "checkpoint list should succeed");
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(
        stdout.contains("First item"),
        "list should contain checkpoint"
    );
}

/// Requires the `jj` binary on PATH. Not run by default in `cargo test`
/// (no CI guarantee jj is installed); run explicitly with
/// `cargo test --test checkpoint_test -- --ignored`.
///
/// Verifies Phase 2 of carryctx-docs/plans/2026-07-25-jujutsu-compatibility.md:
/// under a jj-colocated repo, `checkpoint` reports `vcs_backend: "jj"`, the
/// staged/modified/untracked three-way split is empty (jj's auto-snapshotting
/// makes it meaningless), and `changed_files`/`dirty` stay accurate.
#[test]
#[ignore]
fn test_checkpoint_jj_colocation_reports_backend_and_changed_files() {
    let (dir, bin) = common::setup_test_project("checkpoint_jj_test");

    let jj_init = std::process::Command::new("jj")
        .args(["git", "init", "--colocate"])
        .current_dir(&dir)
        .output()
        .expect("jj binary must be on PATH to run this test");
    assert!(
        jj_init.status.success(),
        "jj git init --colocate failed: {}",
        String::from_utf8_lossy(&jj_init.stderr)
    );

    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "JJ"]);
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
    common::run_cmd(&dir, &bin, &["task", "create", "--title", "jj test task"]);

    std::fs::write(dir.join("tracked.txt"), "hello\n").unwrap();

    let cp = common::run_cmd(
        &dir,
        &bin,
        &[
            "checkpoint",
            "--task",
            "JJ-0001",
            "--done",
            "wrote a file",
            "--json",
        ],
    );
    assert!(cp.status.success(), "checkpoint create should succeed");
    let stdout = String::from_utf8_lossy(&cp.stdout);
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let data = &value["data"];

    assert_eq!(data["vcs_backend"], "jj", "vcs_backend should report jj");
    assert_eq!(
        data["dirty"], true,
        "repo has an untracked file, dirty must be true"
    );
    assert_eq!(
        data["staged_files"].as_array().unwrap().len(),
        0,
        "staged_files must be empty under jj colocation"
    );
    assert_eq!(
        data["modified_files"].as_array().unwrap().len(),
        0,
        "modified_files must be empty under jj colocation"
    );
    assert_eq!(
        data["untracked_files"].as_array().unwrap().len(),
        0,
        "untracked_files must be empty under jj colocation"
    );
    let changed = data["changed_files"].as_array().unwrap();
    assert!(
        changed.iter().any(|f| f == "tracked.txt"),
        "changed_files must still list the new file: {changed:?}"
    );
}
