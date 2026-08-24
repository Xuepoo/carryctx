//! CTX-0074: failures inside runtime opening used to be mapped to bare exit
//! codes (`build_invocation_context`, `try_open_runtime`), so text mode
//! printed nothing and JSON consumers got no document at all.

mod common;

#[test]
fn test_runtime_open_failure_renders_error_envelope_in_json_mode() {
    let (dir, bin) = common::setup_test_project("open_error_envelope");
    common::run_cmd(&dir, &bin, &["init", "--force"]);

    // Corrupt the project config so every state-touching command fails while
    // opening the runtime.
    std::fs::write(dir.join(".carryctx/config.toml"), "not [valid toml").unwrap();

    let out = common::run_cmd(&dir, &bin, &["--json", "status"]);
    assert!(!out.status.success(), "broken config must fail the command");
    let json: serde_json::Value = serde_json::from_slice(&out.stderr).unwrap_or_else(|e| {
        panic!(
            "JSON mode must render an error envelope on stderr: {e}; stderr={} stdout={}",
            String::from_utf8_lossy(&out.stderr),
            String::from_utf8_lossy(&out.stdout)
        )
    });
    assert_eq!(json["success"], serde_json::Value::Bool(false));
    let message = json["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.to_lowercase().contains("config"),
        "envelope must explain the config failure: {message}"
    );
}

#[test]
fn test_runtime_open_failure_prints_readable_line_in_text_mode() {
    let (dir, bin) = common::setup_test_project("open_error_text");
    common::run_cmd(&dir, &bin, &["init", "--force"]);
    std::fs::write(dir.join(".carryctx/config.toml"), "not [valid toml").unwrap();

    let out = common::run_cmd(&dir, &bin, &["status"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.starts_with("Error ["),
        "text mode must print an Error [CODE] line: {stderr}"
    );
    assert!(
        stderr.to_lowercase().contains("config"),
        "the line must explain the config failure: {stderr}"
    );
}
