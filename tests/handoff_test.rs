mod common;

#[test]
fn test_handoff_create_and_list() {
    let (dir, bin) = common::setup_test_project("handoff_test");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "HF"]);
    common::run_cmd(&dir, &bin, &["agent", "register", "--name", "tester", "--provider", "test"]);
    common::run_cmd(&dir, &bin, &["agent", "register", "--name", "target", "--provider", "test"]);
    common::run_cmd(&dir, &bin, &["task", "create", "--title", "Handoff test task"]);
    
    let list_a = common::run_cmd(&dir, &bin, &["agent", "list", "--json"]);
    let stdout = String::from_utf8_lossy(&list_a.stdout);
    assert!(stdout.contains("tester"), "agent list should contain tester");
}
