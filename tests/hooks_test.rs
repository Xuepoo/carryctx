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
