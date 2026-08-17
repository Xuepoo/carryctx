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

#[test]
fn test_task_priority_aliases_and_validation() {
    let (dir, bin) = common::setup_test_project("task_priority_test");

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

    // 1. Critical alias -> urgent
    let p_crit = std::process::Command::new(&bin)
        .args([
            "task",
            "create",
            "--title",
            "Crit Task",
            "--priority",
            "critical",
            "--json",
        ])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        p_crit.status.success(),
        "priority critical alias should succeed"
    );
    let stdout_crit = String::from_utf8_lossy(&p_crit.stdout);
    assert!(
        stdout_crit.contains(r#""priority":"urgent""#),
        "critical must map to urgent priority"
    );

    // 2. Medium alias -> normal
    let p_med = std::process::Command::new(&bin)
        .args([
            "task",
            "create",
            "--title",
            "Med Task",
            "--priority",
            "medium",
            "--json",
        ])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        p_med.status.success(),
        "priority medium alias should succeed"
    );
    let stdout_med = String::from_utf8_lossy(&p_med.stdout);
    assert!(
        stdout_med.contains(r#""priority":"normal""#),
        "medium must map to normal priority"
    );

    // 3. Backlog alias -> low
    let p_backlog = std::process::Command::new(&bin)
        .args([
            "task",
            "create",
            "--title",
            "Backlog Task",
            "--priority",
            "backlog",
            "--json",
        ])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        p_backlog.status.success(),
        "priority backlog alias should succeed"
    );
    let stdout_backlog = String::from_utf8_lossy(&p_backlog.stdout);
    assert!(
        stdout_backlog.contains(r#""priority":"low""#),
        "backlog must map to low priority"
    );

    // 4. Invalid priority rejected during CLI parse
    let p_inv = std::process::Command::new(&bin)
        .args([
            "task",
            "create",
            "--title",
            "Bad Task",
            "--priority",
            "bogus",
        ])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(!p_inv.status.success(), "invalid priority must be rejected");

    // 5. Invalid priority with --dry-run must still be rejected by argument parsing
    let p_dry_inv = std::process::Command::new(&bin)
        .args([
            "task",
            "create",
            "--title",
            "Bad Task",
            "--priority",
            "bogus",
            "--dry-run",
        ])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        !p_dry_inv.status.success(),
        "--dry-run must not bypass invalid priority validation"
    );

    // 6. Task edit with alias & invalid validation
    let edit_crit = std::process::Command::new(&bin)
        .args([
            "task",
            "edit",
            "CTX-0001",
            "--priority",
            "critical",
            "--json",
        ])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        edit_crit.status.success(),
        "task edit with critical alias should succeed"
    );

    let edit_inv_dry = std::process::Command::new(&bin)
        .args([
            "task",
            "edit",
            "CTX-0001",
            "--priority",
            "bogus",
            "--dry-run",
        ])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        !edit_inv_dry.status.success(),
        "task edit --dry-run must reject invalid priority"
    );
}

#[test]
fn test_task_help_shows_priority_possible_values() {
    let (_, bin) = common::setup_test_project("task_help_test");

    let help_create = std::process::Command::new(&bin)
        .args(["task", "create", "--help"])
        .output()
        .unwrap();
    assert!(help_create.status.success());
    let help_create_txt = String::from_utf8_lossy(&help_create.stdout);
    assert!(
        help_create_txt.contains("possible values:")
            && help_create_txt.contains("low")
            && help_create_txt.contains("normal")
            && help_create_txt.contains("high")
            && help_create_txt.contains("urgent"),
        "task create --help must list possible values for priority: {help_create_txt}"
    );

    let help_edit = std::process::Command::new(&bin)
        .args(["task", "edit", "--help"])
        .output()
        .unwrap();
    assert!(help_edit.status.success());
    let help_edit_txt = String::from_utf8_lossy(&help_edit.stdout);
    assert!(
        help_edit_txt.contains("possible values:")
            && help_edit_txt.contains("low")
            && help_edit_txt.contains("normal")
            && help_edit_txt.contains("high")
            && help_edit_txt.contains("urgent"),
        "task edit --help must list possible values for priority: {help_edit_txt}"
    );
}
