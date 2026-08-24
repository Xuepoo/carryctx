//! CTX-0074: `status --since` and `--worktrees` were parsed but silently
//! dropped. `--since` had no honest behavior (the status report contains no
//! event stream), so it is removed; `--worktrees` now adds a detailed
//! worktree table to the Markdown report.

mod common;

#[test]
fn test_status_since_flag_is_removed() {
    let (dir, bin) = common::setup_test_project("status_since_removed");
    common::init_and_agent(&dir, &bin);

    let out = common::run_cmd(&dir, &bin, &["status", "--since", "24h"]);
    assert!(
        !out.status.success(),
        "--since must no longer be accepted as a silent no-op"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--since"),
        "clap should report the unknown flag: {stderr}"
    );
}

#[test]
fn test_status_worktrees_flag_adds_markdown_table() {
    let (dir, bin) = common::setup_test_project("status_worktrees_md");
    common::init_and_agent(&dir, &bin);

    let plain = common::run_cmd(&dir, &bin, &["--format", "markdown", "status"]);
    assert!(plain.status.success(), "plain status should succeed");
    let plain_md = String::from_utf8_lossy(&plain.stdout);
    assert!(
        !plain_md.contains("## Worktrees"),
        "default markdown report must not include the table"
    );

    let with = common::run_cmd(
        &dir,
        &bin,
        &["--format", "markdown", "status", "--worktrees"],
    );
    assert!(with.status.success(), "--worktrees status should succeed");
    let md = String::from_utf8_lossy(&with.stdout);
    assert!(
        md.contains("## Worktrees"),
        "--worktrees must add the worktree section: {md}"
    );
}
