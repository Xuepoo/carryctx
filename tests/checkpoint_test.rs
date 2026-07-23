mod common;

#[test]
fn test_checkpoint_create_and_list() {
    let (dir, bin) = common::setup_test_project("checkpoint_test");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "CK"]);
    common::run_cmd(&dir, &bin, &["agent", "register", "--name", "tester", "--provider", "test"]);
    common::run_cmd(&dir, &bin, &["task", "create", "--title", "Checkpoint test task"]);
    
    // Create checkpoint
    let cp = common::run_cmd(&dir, &bin, &["checkpoint", "--task", "CK-0001",
        "--done", "First item", "--remaining", "Second item", "--json"]);
    assert!(cp.status.success(), "checkpoint create should succeed");
    let stdout = String::from_utf8_lossy(&cp.stdout);
    assert!(stdout.contains("First item"), "checkpoint should contain done items");
    
    // List checkpoints
    let list = common::run_cmd(&dir, &bin, &["checkpoint", "list", "--json"]);
    assert!(list.status.success(), "checkpoint list should succeed");
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(stdout.contains("First item"), "list should contain checkpoint");
}
