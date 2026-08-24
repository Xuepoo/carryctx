//! CTX-0076 / issue #105: `--config-compat` was declared as a global flag but
//! never consumed — the runtime hardcoded Warn, so the documented `error`
//! mode (fail on unknown configuration fields) was unreachable.

mod common;

/// Append unknown keys (a misspelled section and a wholly unknown section)
/// to an otherwise valid project config. Appending keeps the file valid TOML:
/// trailing bare keys would land inside the last real section, so unknown
/// entries are expressed as sections.
fn plant_unknown_keys(dir: &std::path::Path) {
    let path = dir.join(".carryctx/config.toml");
    let mut content = std::fs::read_to_string(&path).unwrap();
    if !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str("\n[sesson]\nstale_after = \"3h\"\n\n[brand_new_section]\nfoo = 1\n");
    std::fs::write(&path, content).unwrap();
}

#[test]
fn test_config_compat_error_mode_fails_on_unknown_keys() {
    let (dir, bin) = common::setup_test_project("config_compat_error");
    common::init_and_agent(&dir, &bin);
    plant_unknown_keys(&dir);

    let out = common::run_cmd(
        &dir,
        &bin,
        &["--json", "--config-compat", "error", "task", "list"],
    );
    assert!(
        !out.status.success(),
        "error mode must fail on unknown config keys"
    );
    // The failure happens before any command output: the envelope lands on
    // stderr from the pre-dispatch error path.
    let stderr = String::from_utf8_lossy(&out.stderr);
    let value: serde_json::Value = serde_json::from_str(stderr.trim())
        .or_else(|_| serde_json::from_slice(&out.stdout))
        .expect("an error envelope must be rendered");
    assert_eq!(value["success"], serde_json::Value::Bool(false));
    let message = value["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("sesson.stale_after"), "{message}");
    assert!(message.contains("brand_new_section.foo"), "{message}");
    assert!(
        message.contains("Did you mean: session.stale_after?"),
        "the Did-you-mean hint must suggest the near miss: {message}"
    );
}

#[test]
fn test_config_compat_warn_mode_and_default_stay_lenient() {
    let (dir, bin) = common::setup_test_project("config_compat_warn");
    common::init_and_agent(&dir, &bin);
    plant_unknown_keys(&dir);

    for args in [
        vec!["--json", "--config-compat", "warn", "task", "list"],
        vec!["--json", "task", "list"],
    ] {
        let out = common::run_cmd(&dir, &bin, &args);
        assert!(
            out.status.success(),
            "{args:?} must succeed despite unknown keys: {} {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
