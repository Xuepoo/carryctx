mod common;

/// Requires the `jj` binary on PATH. Not run by default in `cargo test`
/// (no CI guarantee jj is installed); run explicitly with
/// `cargo test --test hooks_test -- --ignored`.
///
/// Verifies Phase 4 of carryctx-docs/plans/2026-07-25-jujutsu-compatibility.md:
/// `carryctx hooks install` refuses under jj colocation with a clear error
/// instead of silently installing git hooks that `jj commit`/`jj describe`
/// never trigger (jj writes commits via `jj git export`, bypassing Git's
/// hook mechanism entirely).
#[test]
#[ignore]
fn test_hooks_install_refuses_under_jj_colocation() {
    let (dir, bin) = common::setup_test_project("hooks_jj_test");

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

    common::run_cmd(&dir, &bin, &["init", "--force"]);

    let result = common::run_cmd(&dir, &bin, &["hooks", "install"]);
    assert!(
        !result.status.success(),
        "hooks install must fail under jj colocation, not silently install dead hooks"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("jj"),
        "error message should explain the jj-specific reason: {stderr}"
    );

    assert!(
        !dir.join(".git/hooks/post-commit").exists(),
        "post-commit hook must not be written under jj colocation"
    );
    assert!(
        !dir.join(".git/hooks/prepare-commit-msg").exists(),
        "prepare-commit-msg hook must not be written under jj colocation"
    );
}

/// Companion regression check: plain (non-jj) repos must be completely
/// unaffected by the jj-colocation guard added for the test above.
#[test]
fn test_hooks_install_unaffected_by_jj_guard_on_plain_git() {
    let (dir, bin) = common::setup_test_project("hooks_plain_git_test");
    common::run_cmd(&dir, &bin, &["init", "--force"]);

    let result = common::run_cmd(&dir, &bin, &["hooks", "install"]);
    assert!(
        result.status.success(),
        "hooks install should succeed on plain git: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        dir.join(".git/hooks/post-commit").exists(),
        "post-commit hook should be written on plain git"
    );
}
