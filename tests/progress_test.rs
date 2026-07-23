mod common;

#[test]
fn test_progress_todo_and_list() {
    let (dir, bin) = common::setup_test_project("progress_test");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "TP"]);
    common::run_cmd(&dir, &bin, &["agent", "register", "--name", "tester", "--provider", "test"]);
    
    // Create task
    let create = common::run_cmd(&dir, &bin, &["task", "create", "--title", "Progress test task", "--json"]);
    let stdout = String::from_utf8_lossy(&create.stdout);
    assert!(stdout.contains("display_id"), "task create should return display_id");
    
    // Add progress items
    let todo = common::run_cmd(&dir, &bin, &["progress", "todo", "--task", "TP-0001", "Test progress", "--json"]);
    assert!(todo.status.success(), "progress todo should succeed");
    assert!(String::from_utf8_lossy(&todo.stdout).contains("Test progress"), "todo should contain content");
    
    // List progress
    let list = common::run_cmd(&dir, &bin, &["progress", "list", "--task", "TP-0001", "--json"]);
    assert!(list.status.success(), "progress list should succeed");
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(stdout.contains("Test progress"), "list should contain the progress item");
}
