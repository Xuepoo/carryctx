mod common;

#[test]
fn test_full_workflow_e2e() {
    let (dir, bin) = common::setup_test_project("e2e_full");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "E2E"]);
    common::run_cmd(&dir, &bin, &["agent", "register", "--name", "tester", "--provider", "test"]);
    
    // Session start
    let s = common::run_cmd(&dir, &bin, &["session", "start", "--json"]);
    assert!(s.status.success(), "session start");
    
    // Task create
    let t = common::run_cmd(&dir, &bin, &["task", "create", "--title", "E2E task", "--json"]);
    assert!(t.status.success(), "task create");
    
    // Task claim
    let c = common::run_cmd(&dir, &bin, &["task", "claim", "E2E-0001", "--json"]);
    assert!(c.status.success(), "task claim");
    
    // Progress todo
    let p = common::run_cmd(&dir, &bin, &["progress", "todo", "--task", "E2E-0001", "Step one", "--json"]);
    assert!(p.status.success(), "progress todo");
    
    // Checkpoint
    let cp = common::run_cmd(&dir, &bin, &["checkpoint", "--task", "E2E-0001", "--done", "Step one done", "--json"]);
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
