mod common;

// CTX-0178 / issue #203: `hooks install` now writes thin dispatch shims, so
// this legacy-template guard is retired. The `display_id` extraction pipeline
// below tested the old retired `carryctx context | grep display_id` hook
// content. The migration path (legacy fat -> shim) is covered by
// `hooks_test::test_hooks_install_upgrades_legacy_fat_hook_to_shim` and
// `hook_task_context_test::migrated_shim_dispatches_branch_bound_task_not_global_active`;
// the unit tests in `commands::hooks::tests` still pin the legacy marker for
// migration detection.
// (Test body kept for archaeology; ignored so `cargo test` stays green.)
/// End-to-end guard for the hook templates: `hooks install` writes scripts
/// that grep `"display_id"` out of `carryctx context --format json`. This
/// test runs that exact extraction pipeline against real CLI output so a
/// future rename of the JSON key cannot silently break every installed
/// hook again (CTX-0069 / issue #96 hooks scope).
#[test]
#[ignore = "CTX-0178: retired with the legacy fat-hook pipeline; install now writes dispatch shims"]
fn hook_display_id_extraction_matches_context_json() {
    let (dir, bin) = common::setup_test_project("hooks_template_json_test");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "HKT"]);
    common::run_cmd(
        &dir,
        &bin,
        &[
            "agent",
            "register",
            "--name",
            "hooker",
            "--provider",
            "test",
        ],
    );
    common::run_cmd(
        &dir,
        &bin,
        &[
            "task",
            "create",
            "--title",
            "Hook template probe",
            "--agent",
            "hooker",
        ],
    );
    common::run_cmd(&dir, &bin, &["task", "claim", "HKT-0001"]);
    common::run_cmd(&dir, &bin, &["task", "start", "HKT-0001"]);

    let ctx = std::process::Command::new(&bin)
        .args(["context", "--format", "json", "--task", "HKT-0001"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        ctx.status.success(),
        "context --format json must succeed: {}",
        String::from_utf8_lossy(&ctx.stderr)
    );

    let stdout = String::from_utf8_lossy(&ctx.stdout);
    // The exact grep expression installed by POST_COMMIT_HOOK /
    // PREPARE_COMMIT_MSG_HOOK.
    let needle = "\"display_id\":\"";
    let extracted = stdout
        .split(needle)
        .nth(1)
        .map(|rest| rest.split('"').next().unwrap_or("").to_string());
    assert_eq!(
        extracted.as_deref(),
        Some("HKT-0001"),
        "the template's grep pipeline must recover the active task display id from context JSON"
    );
}
