mod common;

#[test]
fn test_task_create_and_list() {
    let (dir, bin) = common::setup_test_project("task_test");

    std::process::Command::new(&bin)
        .args(["init", "--force"])
        .current_dir(&dir)
        .output()
        .unwrap();

    std::process::Command::new(&bin)
        .args([
            "agent",
            "register",
            "--name",
            "tester",
            "--provider",
            "test",
        ])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(&dir)
        .output()
        .unwrap();

    let create = std::process::Command::new(&bin)
        .args(["task", "create", "--title", "Integration test task"])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(create.status.success(), "task create should succeed");

    let list = std::process::Command::new(&bin)
        .args(["task", "list", "--json"])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(list.status.success(), "task list should succeed");
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(
        stdout.contains("Integration test task"),
        "task list should contain the created task"
    );
}
