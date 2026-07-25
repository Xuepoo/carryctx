mod common;

/// Requires the `jj` binary on PATH. Not run by default in `cargo test`
/// (no CI guarantee jj is installed); run explicitly with
/// `cargo test --test worktree_test -- --ignored`.
///
/// Verifies Phase 3 of carryctx-docs/plans/2026-07-25-jujutsu-compatibility.md:
/// `carryctx worktree create` refuses with a clear, non-panicking error under
/// jj colocation instead of silently creating a directory neither `jj` nor
/// carryctx's own state commands can use from inside (jj secondary
/// workspaces from `jj workspace add` have no `.git/`, and `git worktree add`
/// produces a directory `jj workspace list` never discovers).
#[test]
#[ignore]
fn test_worktree_create_refuses_under_jj_colocation() {
    let (dir, bin) = common::setup_test_project("worktree_jj_test");

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

    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "WJ"]);
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
    common::run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", "jj worktree task"],
    );

    let result = common::run_cmd(&dir, &bin, &["worktree", "create", "WJ-0001", "--json"]);
    assert!(
        !result.status.success(),
        "worktree create must fail under jj colocation, not silently create a broken worktree"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    let value: serde_json::Value =
        serde_json::from_str(&stderr).expect("valid JSON error envelope on stderr");
    assert_eq!(value["success"], false);
    assert_eq!(value["error"]["code"], "VALIDATION_FAILED");
    let message = value["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("jj"),
        "error message should explain the jj-specific reason: {message}"
    );

    // The directory carryctx would have created must not exist.
    assert!(
        !dir.join(".worktrees").exists(),
        "no worktree directory should have been created on refusal"
    );
}

/// Companion regression check: plain (non-jj) repos must be completely
/// unaffected by the jj-colocation guard added for the test above.
#[test]
fn test_worktree_create_unaffected_by_jj_guard_on_plain_git() {
    let (dir, bin) = common::setup_test_project("worktree_plain_git_test");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "WP"]);
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
    common::run_cmd(&dir, &bin, &["task", "create", "--title", "plain git task"]);

    let result = common::run_cmd(&dir, &bin, &["worktree", "create", "WP-0001", "--json"]);
    assert!(
        result.status.success(),
        "worktree create should succeed on plain git: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}
