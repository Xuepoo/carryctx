mod common;

/// Requires the `jj` binary on PATH (jj >= 0.43.0, `jj git init --colocate`).
/// Runs by default whenever `jj` is present and skips cleanly when it is not.
///
/// Verifies Phase 4 of carryctx-docs/plans/2026-07-25-jujutsu-compatibility.md:
/// `carryctx hooks install` refuses under jj colocation with a clear error
/// instead of silently installing git hooks that `jj commit`/`jj describe`
/// never trigger (jj writes commits via `jj git export`, bypassing Git's
/// hook mechanism entirely).
#[test]
fn test_hooks_install_refuses_under_jj_colocation() {
    if !common::jj_available() {
        eprintln!(
            "skipping test_hooks_install_refuses_under_jj_colocation: `jj` is not on PATH (requires jj >= 0.43.0)"
        );
        return;
    }

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

/// CTX-0178 / issue #203: `hooks install` must migrate a legacy fat hook
/// (the old `carryctx context --format json | grep -o display_id` shell
/// pipeline) to the thin dispatch shim without requiring `--force`, because
/// the legacy pipeline resolves the global active task instead of the
/// worktree-bound task. The legacy bytes are preserved once in `<hook>.bak`.
#[test]
fn test_hooks_install_upgrades_legacy_fat_hook_to_shim() {
    let (dir, bin) = common::setup_test_project("hooks_legacy_migrate");
    common::run_cmd(&dir, &bin, &["init", "--force"]);

    let legacy_post_commit = "#!/bin/sh\n# CarryCtx post-commit hook\nTASK_ID=$(carryctx context --format json 2>/dev/null | grep -o '\"display_id\":\"[^\"]*\"' | head -1 | cut -d'\"' -f4)\ncarryctx checkpoint --task \"$TASK_ID\" --quiet 2>/dev/null || true\n";
    let legacy_prepare = "#!/bin/sh\n# CarryCtx prepare-commit-msg hook\nTASK_ID=$(carryctx context --format json 2>/dev/null | grep -o '\"display_id\":\"[^\"]*\"' | head -1 | cut -d'\"' -f4)\n";
    std::fs::write(dir.join(".git/hooks/post-commit"), legacy_post_commit).unwrap();
    std::fs::write(dir.join(".git/hooks/prepare-commit-msg"), legacy_prepare).unwrap();

    let out = common::run_cmd(&dir, &bin, &["--json", "hooks", "install"]);
    assert!(
        out.status.success(),
        "install must migrate legacy fat hooks without --force: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("install must emit a JSON envelope");
    let warnings = value["warnings"]
        .as_array()
        .expect("migration must surface warnings");
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap_or_default().contains("dispatch shim")),
        "migration must mention the dispatch shim: {value}"
    );

    for name in ["post-commit", "prepare-commit-msg"] {
        let content = std::fs::read_to_string(dir.join(".git/hooks").join(name)).unwrap();
        assert!(
            content.contains("hooks dispatch"),
            "{name} must be a dispatch shim after migration: {content}"
        );
        assert!(
            !content.contains("carryctx context"),
            "{name} must not keep the legacy context pipeline: {content}"
        );
        let bak = std::fs::read_to_string(dir.join(".git/hooks").join(format!("{name}.bak")))
            .expect("legacy content must be backed up to .bak");
        assert!(
            bak.contains("carryctx context"),
            "{name}.bak must preserve the legacy fat content: {bak}"
        );
    }
}

/// CTX-0178 / issue #203: a second install must not clobber an existing
/// `.bak` from a previous migration, and the shim stays installed.
#[test]
fn test_hooks_install_migration_does_not_overwrite_existing_bak() {
    let (dir, bin) = common::setup_test_project("hooks_legacy_bak_once");
    common::run_cmd(&dir, &bin, &["init", "--force"]);

    let sentinel = "sentinel-from-first-migration";
    std::fs::write(
        dir.join(".git/hooks/post-commit.bak"),
        format!("# CarryCtx legacy\n{sentinel}\ncarryctx context\n"),
    )
    .unwrap();
    let legacy = "#!/bin/sh\n# CarryCtx post-commit hook\nTASK_ID=$(carryctx context --format json | head -1)\n";
    std::fs::write(dir.join(".git/hooks/post-commit"), legacy).unwrap();

    let out = common::run_cmd(&dir, &bin, &["hooks", "install"]);
    assert!(
        out.status.success(),
        "install must succeed without --force: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bak = std::fs::read_to_string(dir.join(".git/hooks/post-commit.bak")).unwrap();
    assert!(
        bak.contains(sentinel),
        "existing .bak must not be overwritten: {bak}"
    );
    let content = std::fs::read_to_string(dir.join(".git/hooks/post-commit")).unwrap();
    assert!(
        content.contains("hooks dispatch"),
        "hook must be a shim after migration: {content}"
    );
}

/// CTX-0178 / issue #203: `hooks status` must flag legacy fat installs with
/// an actionable migration hint (text stdout and JSON stdout).
#[test]
fn test_hooks_status_flags_legacy_with_migration_hint() {
    let (dir, bin) = common::setup_test_project("hooks_legacy_status");
    common::run_cmd(&dir, &bin, &["init", "--force"]);

    let legacy = "#!/bin/sh\n# CarryCtx post-commit hook\nTASK_ID=$(carryctx context --format json 2>/dev/null | grep -o '\"display_id\":\"[^\"]*\"' | head -1)\n";
    std::fs::write(dir.join(".git/hooks/post-commit"), legacy).unwrap();

    let out = common::run_cmd(&dir, &bin, &["hooks", "status", "--json"]);
    assert!(out.status.success(), "hooks status should succeed");
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let hooks = json["data"]["hooks"].as_array().expect("hooks array");
    let post = hooks
        .iter()
        .find(|h| h["hook"] == "post-commit")
        .expect("post-commit entry");
    assert_eq!(
        post["legacy"],
        serde_json::Value::Bool(true),
        "legacy fat hook must be flagged: {json}"
    );

    let warnings = json["warnings"].as_array();
    assert!(
        warnings.is_some_and(|w| w
            .iter()
            .any(|x| x.as_str().unwrap_or_default().contains("hooks install"))),
        "JSON status must hint at the migration command: {json}"
    );

    let out = common::run_cmd(&dir, &bin, &["hooks", "status"]);
    assert!(out.status.success(), "hooks status should succeed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stdout.contains("hooks install") || stderr.contains("hooks install"),
        "text status must hint at the migration command: stdout={stdout} stderr={stderr}"
    );
}

#[test]
fn test_hooks_status_envelope_uses_dotted_command_label() {
    // CTX-0074: the envelope label was the space-separated "hooks status",
    // straggling behind the dotted-label cleanup ("hooks.status") applied
    // across the rest of the surface.
    let (dir, bin) = common::setup_test_project("hooks_status_label");
    common::run_cmd(&dir, &bin, &["init", "--force"]);

    let out = common::run_cmd(&dir, &bin, &["hooks", "status", "--json"]);
    assert!(out.status.success(), "hooks status should succeed");
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        json["command"].as_str().unwrap(),
        "hooks.status",
        "envelope command must use the dotted convention"
    );
}

/// CTX-0076 / PX-0125: hooks install/uninstall ignored the global --json flag,
/// so scripts parsing stdout got nothing (or human-facing ✓ lines). Both must
/// emit standard success envelopes under --json; text mode stays unchanged.
#[test]
fn test_hooks_install_and_uninstall_honor_global_json() {
    let (dir, bin) = common::setup_test_project("hooks_json_envelopes");

    // Install under --json: success envelope on stdout.
    let out = common::run_cmd(&dir, &bin, &["--json", "hooks", "install"]);
    assert!(
        out.status.success(),
        "install must succeed: {} {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("install must emit a JSON envelope");
    assert_eq!(value["command"], "hooks.install");
    assert_eq!(value["success"], serde_json::Value::Bool(true));
    let installed = value["data"]["installed"]
        .as_array()
        .expect("installed list");
    assert_eq!(installed.len(), 2, "both hooks installed by default");

    // Uninstall under --json: success envelope on stdout.
    let out = common::run_cmd(&dir, &bin, &["--json", "hooks", "uninstall"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("uninstall must emit a JSON envelope");
    assert_eq!(value["command"], "hooks.uninstall");
    assert_eq!(value["success"], serde_json::Value::Bool(true));

    // Text mode stays unchanged: human-readable lines, no JSON.
    let out = common::run_cmd(&dir, &bin, &["hooks", "install"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("✓ Installed hook: post-commit"), "{stdout}");
}

/// A conflicting install (existing hook, no --force) must render the standard
/// error envelope under --json instead of a bare stderr note + silent code.
#[test]
fn test_hooks_install_conflict_renders_error_envelope_under_json() {
    let (dir, bin) = common::setup_test_project("hooks_json_conflict");
    std::fs::write(
        dir.join(".git").join("hooks").join("post-commit"),
        "#!/bin/sh\n",
    )
    .unwrap();

    let out = common::run_cmd(&dir, &bin, &["--json", "hooks", "install"]);
    assert!(!out.status.success(), "conflicting install must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let value: serde_json::Value = serde_json::from_str(stderr.trim())
        .unwrap_or_else(|e| panic!("stderr must be an error envelope: {e}; stderr={stderr}"));
    assert_eq!(value["success"], serde_json::Value::Bool(false));
    assert_eq!(value["command"], "hooks.install");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("already exists"),
        "the message must explain the conflict: {value}"
    );
}
