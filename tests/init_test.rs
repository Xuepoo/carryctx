mod common;

fn config_path(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join(".carryctx").join("config.toml")
}

fn read_config(dir: &std::path::Path) -> String {
    std::fs::read_to_string(config_path(dir)).expect("config.toml must exist")
}

fn write_config(dir: &std::path::Path, content: &str) {
    write_config_bytes(dir, content.as_bytes());
}

fn write_config_bytes(dir: &std::path::Path, content: &[u8]) {
    let path = config_path(dir);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, content).unwrap();
}

/// CTX-0174 / issue #197: `init` in a checkout whose committed config has
/// non-default values must update only the identity fields instead of
/// rewriting the Git-tracked file from defaults.
const COMMITTED_CONFIG: &str = r#"# committed project config
[project]
id = "proj_committed_identity"
name = "CommittedName"
task_prefix = "CMT"

[verification]
commands = ["just check"]

[worktree.cleanup]
delete_branch = "when_removed"

[context]
max_events = 42
"#;

/// Identity that matches what `init` resolves in a fresh fixture repo (id,
/// name, and prefix reused from the file; branch = main), so a no-op init
/// must not rewrite a single byte.
const NOOP_CONFIG: &str = r#"# committed project config
[project]
id = "proj_noop"
name = "NoopProject"
task_prefix = "NOP"

[git]
main_branch = "main"

[verification]
commands = ["just check"]
"#;

/// Every non-identity byte of a pre-existing config must survive `init`; the
/// identity fields are reused from that config when no flag overrides them.
#[test]
fn test_init_preserves_existing_config_and_updates_only_identity() {
    let (dir, bin) = common::setup_test_project("init_preserve_config");
    write_config(&dir, COMMITTED_CONFIG);

    let out = common::run_cmd(&dir, &bin, &["init", "--json"]);
    assert!(
        out.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let after = read_config(&dir);
    for expected in [
        "# committed project config",
        "id = \"proj_committed_identity\"",
        "name = \"CommittedName\"",
        "task_prefix = \"CMT\"",
        "commands = [\"just check\"]",
        "delete_branch = \"when_removed\"",
        "max_events = 42",
    ] {
        assert!(
            after.contains(expected),
            "missing {expected:?} after init:\n{after}"
        );
    }

    // Explicit flags rewrite exactly the identity key they name.
    let out = common::run_cmd(
        &dir,
        &bin,
        &[
            "init",
            "--force",
            "--name",
            "Renamed",
            "--task-prefix",
            "RNM",
            "--json",
        ],
    );
    assert!(
        out.status.success(),
        "forced init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let after = read_config(&dir);
    for expected in [
        "# committed project config",
        "id = \"proj_committed_identity\"",
        "name = \"Renamed\"",
        "task_prefix = \"RNM\"",
        "commands = [\"just check\"]",
        "delete_branch = \"when_removed\"",
        "max_events = 42",
    ] {
        assert!(
            after.contains(expected),
            "missing {expected:?} after forced init:\n{after}"
        );
    }
    assert!(
        !after.contains("CommittedName"),
        "old identity must be replaced:\n{after}"
    );
}

/// An identity update that changes nothing must not touch the file at all.
#[test]
fn test_init_with_matching_identity_leaves_config_byte_identical() {
    let (dir, bin) = common::setup_test_project("init_config_noop");
    write_config(&dir, NOOP_CONFIG);
    let before = std::fs::read(config_path(&dir)).unwrap();

    let out = common::run_cmd(&dir, &bin, &["init", "--json"]);
    assert!(
        out.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let after = std::fs::read(config_path(&dir)).unwrap();
    assert_eq!(
        before,
        after,
        "a no-op identity update must leave config.toml byte-identical:\n{}",
        String::from_utf8_lossy(&after)
    );
}

/// Issue #197 fix-forward: `toml_edit` re-serialization normalizes CRLF to
/// LF, so a matching identity must skip the write entirely rather than emit
/// whole-file Git drift.
#[test]
fn test_init_noop_with_crlf_config_leaves_bytes_identical() {
    let (dir, bin) = common::setup_test_project("init_config_noop_crlf");
    write_config(&dir, &NOOP_CONFIG.replace('\n', "\r\n"));
    let before = std::fs::read(config_path(&dir)).unwrap();

    let out = common::run_cmd(&dir, &bin, &["init", "--json"]);
    assert!(
        out.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let after = std::fs::read(config_path(&dir)).unwrap();
    assert_eq!(
        before,
        after,
        "a no-op identity update must preserve CRLF bytes:\n{}",
        String::from_utf8_lossy(&after)
    );
}

/// Re-serialization appends a final newline the committed file may not have;
/// a matching identity must leave the bytes alone.
#[test]
fn test_init_noop_without_final_newline_leaves_bytes_identical() {
    let (dir, bin) = common::setup_test_project("init_config_noop_no_newline");
    write_config(&dir, NOOP_CONFIG.trim_end_matches('\n'));
    let before = std::fs::read(config_path(&dir)).unwrap();

    let out = common::run_cmd(&dir, &bin, &["init", "--json"]);
    assert!(
        out.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let after = std::fs::read(config_path(&dir)).unwrap();
    assert_eq!(
        before,
        after,
        "a no-op identity update must not append a final newline:\n{}",
        String::from_utf8_lossy(&after)
    );
}

/// Re-serialization drops a UTF-8 BOM; a matching identity must leave the
/// byte sequence (BOM included) untouched.
#[test]
fn test_init_noop_with_bom_leaves_bytes_identical() {
    let (dir, bin) = common::setup_test_project("init_config_noop_bom");
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(NOOP_CONFIG.as_bytes());
    write_config_bytes(&dir, &bytes);
    let before = std::fs::read(config_path(&dir)).unwrap();

    let out = common::run_cmd(&dir, &bin, &["init", "--json"]);
    assert!(
        out.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let after = std::fs::read(config_path(&dir)).unwrap();
    assert_eq!(
        before, after,
        "a no-op identity update must preserve a UTF-8 BOM:\n{:?}",
        after
    );
}

/// The absent-config path keeps writing the full default template.
#[test]
fn test_init_without_config_writes_default_template() {
    let (dir, bin) = common::setup_test_project("init_default_template");
    let out = common::run_cmd(&dir, &bin, &["init", "--json"]);
    assert!(
        out.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let config = read_config(&dir);
    for expected in [
        "delete_branch = \"never\"",
        "commands = []",
        "main_branch = \"main\"",
    ] {
        assert!(
            config.contains(expected),
            "default template missing {expected:?}:\n{config}"
        );
    }
}

/// Inline-table identities are schema-valid to the loader, so `init` must
/// update them in place instead of failing CONFIGURATION_ERROR (base
/// succeeded destructively by rewriting the file).
#[test]
fn test_init_handles_inline_project_table() {
    let (dir, bin) = common::setup_test_project("init_inline_project");
    write_config(
        &dir,
        "# inline identity\n\
         project = { id = \"proj_inline\", name = \"InlineName\", task_prefix = \"INL\" }\n\
         \n\
         [git]\n\
         main_branch = \"main\"\n\
         \n\
         [verification]\n\
         commands = [\"just check\"]\n",
    );

    let out = common::run_cmd(
        &dir,
        &bin,
        &[
            "init",
            "--name",
            "InlineRenamed",
            "--task-prefix",
            "INR",
            "--json",
        ],
    );
    assert!(
        out.status.success(),
        "init on an inline project table must succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let after = read_config(&dir);
    for expected in [
        "# inline identity",
        "project = {",
        "id = \"proj_inline\"",
        "name = \"InlineRenamed\"",
        "task_prefix = \"INR\"",
        "commands = [\"just check\"]",
    ] {
        assert!(
            after.contains(expected),
            "missing {expected:?} after init:\n{after}"
        );
    }
}

/// Root-level `git = { ... }` is equally schema-valid; `init` must update
/// `main_branch` in place and keep the remaining inline keys.
#[test]
fn test_init_handles_inline_git_table() {
    let (dir, bin) = common::setup_test_project("init_inline_git");
    write_config(
        &dir,
        "git = { main_branch = \"develop\", branch_template = \"carryctx/{task_id}\" }\n\
         \n\
         [project]\n\
         id = \"proj_git_inline\"\n\
         name = \"GitInline\"\n\
         task_prefix = \"GIL\"\n\
         \n\
         [verification]\n\
         commands = [\"just check\"]\n",
    );

    let out = common::run_cmd(&dir, &bin, &["init", "--json"]);
    assert!(
        out.status.success(),
        "init on an inline git table must succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let after = read_config(&dir);
    for expected in [
        "id = \"proj_git_inline\"",
        "name = \"GitInline\"",
        "task_prefix = \"GIL\"",
        "git = {",
        "main_branch = \"main\"",
        "branch_template = \"carryctx/{task_id}\"",
        "commands = [\"just check\"]",
    ] {
        assert!(
            after.contains(expected),
            "missing {expected:?} after init:\n{after}"
        );
    }
}

#[test]
fn test_init_success() {
    let (dir, bin) = common::setup_test_project("init_success");
    let output = std::process::Command::new(&bin)
        .args([
            "init",
            "--name",
            "TestProject",
            "--task-prefix",
            "TP",
            "--force",
        ])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "init should succeed: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        dir.join(".carryctx").join("config.toml").exists(),
        "config.toml should exist"
    );
}

#[test]
fn test_init_without_force_fails_on_second_call() {
    let (dir, bin) = common::setup_test_project("init_no_force");
    let first = std::process::Command::new(&bin)
        .args(["init", "--force"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(first.status.success(), "first init should succeed");
    let second = std::process::Command::new(&bin)
        .args(["init"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        !second.status.success(),
        "second init without --force should fail"
    );
}

/// CTX-0072 / issue #105: config-provided task prefixes are validated at the
/// persistence boundary (uppercase ASCII, 1-10 chars) instead of silently
/// entering the display-id space.
#[test]
fn test_init_rejects_invalid_task_prefix() {
    let (dir, bin) = common::setup_test_project("init_bad_prefix");

    for bad in ["ctx", "TOOLONGPREFIX", "WITH SPACE", ""] {
        let out = common::run_cmd(
            &dir,
            &bin,
            &["init", "--force", "--task-prefix", bad, "--json"],
        );
        assert!(!out.status.success(), "prefix '{bad}' must be rejected");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("VALIDATION_FAILED"),
            "invalid prefix must report VALIDATION_FAILED: {stderr}"
        );
    }

    // A valid custom prefix still works and is persisted.
    let ok = common::run_cmd(
        &dir,
        &bin,
        &["init", "--force", "--task-prefix", "ABC", "--json"],
    );
    assert!(
        ok.status.success(),
        "valid prefix must be accepted: {}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&ok.stdout)).expect("valid json");
    assert_eq!(value["data"]["task_prefix"], "ABC");
}
