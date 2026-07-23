mod common;

#[test]
fn test_decision_add_and_list() {
    let (dir, bin) = common::setup_test_project("decision_test");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "DC"]);
    common::run_cmd(&dir, &bin, &["agent", "register", "--name", "tester", "--provider", "test"]);
    
    // Create task
    common::run_cmd(&dir, &bin, &["task", "create", "--title", "Decision test task"]);
    
    // Add decision
    let add = common::run_cmd(&dir, &bin, &["decision", "add", "--title", "Test decision",
        "--context", "Testing", "--decision", "Use markdown", "--consequences", "None",
        "--task", "DC-0001", "--json"]);
    assert!(add.status.success(), "decision add should succeed");
    let stdout = String::from_utf8_lossy(&add.stdout);
    assert!(stdout.contains("Test decision"), "decision should contain title");
    
    // List decisions
    let list = common::run_cmd(&dir, &bin, &["decision", "list", "--json"]);
    assert!(list.status.success(), "decision list should succeed");
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(stdout.contains("Test decision"), "list should contain the decision");
}

#[test]
fn test_decision_search() {
    let (dir, bin) = common::setup_test_project("decision_search_test");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "DS"]);
    common::run_cmd(&dir, &bin, &["agent", "register", "--name", "tester", "--provider", "test"]);
    common::run_cmd(&dir, &bin, &["task", "create", "--title", "Search test task"]);
    common::run_cmd(&dir, &bin, &["decision", "add", "--title", "UniqueSearchDecision",
        "--task", "DS-0001"]);
    
    let search = common::run_cmd(&dir, &bin, &["decision", "search", "UniqueSearch", "--json"]);
    let stdout = String::from_utf8_lossy(&search.stdout);
    assert!(stdout.contains("UniqueSearchDecision"), "search should find the decision");
}
