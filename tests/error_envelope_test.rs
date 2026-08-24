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

/// CTX-0076 / PX-0135: a handler still on the legacy `try_open_runtime` path
/// collapsed open failures to a bare exit code with no output at all — even in
/// JSON mode. Every entity command must report through the standard error path
/// (envelope on stderr in JSON mode, readable line in text mode).
///
/// Repro: corrupt the state database so the pre-dispatch admission lock still
/// succeeds but the runtime open inside the command handler fails.
fn assert_open_failure_is_reported(args: &[&str], setup_name: &str) {
    let (dir, bin) = common::setup_test_project(setup_name);
    common::init_and_agent(&dir, &bin);

    let db_path = dir.join(".git/carryctx/state.sqlite");
    assert!(db_path.exists(), "state db must exist after init");
    std::fs::write(&db_path, b"this is definitely not a sqlite database").unwrap();

    // JSON mode: error envelope on stderr, nothing silent.
    let json_args: Vec<&str> = ["--json"].iter().chain(args.iter()).copied().collect();
    let json_out = common::run_cmd(&dir, &bin, &json_args);
    assert!(
        !json_out.status.success(),
        "{args:?} must fail on an unwritable state db"
    );
    let stderr = String::from_utf8_lossy(&json_out.stderr);
    assert!(
        !stderr.trim().is_empty(),
        "JSON mode must print an error envelope on stderr for {args:?}"
    );
    let envelope: serde_json::Value = serde_json::from_str(stderr.trim()).unwrap_or_else(|e| {
        panic!("stderr must be a JSON envelope for {args:?}: {e}; stderr={stderr}")
    });
    assert_eq!(envelope["success"], serde_json::Value::Bool(false));
    assert!(
        envelope["error"]["code"].is_string(),
        "envelope must carry an error code: {envelope}"
    );

    // Text mode: readable Error [CODE] line.
    let text_out = common::run_cmd(&dir, &bin, args);
    assert!(!text_out.status.success());
    let text_stderr = String::from_utf8_lossy(&text_out.stderr);
    assert!(
        text_stderr.starts_with("Error ["),
        "text mode must print an Error [CODE] line for {args:?}: {text_stderr}"
    );
}

#[test]
fn test_entity_commands_report_runtime_open_failures() {
    for (args, name) in [
        (&["task", "list"][..], "px135_task"),
        (&["context"][..], "px135_context"),
        (&["decision", "list"][..], "px135_decision"),
        (&["event", "list"][..], "px135_event"),
        (
            &["graph", "edges", "01J000000000000000000000000"][..],
            "px135_graph",
        ),
        (&["handoff", "list"][..], "px135_handoff"),
        (&["preset", "list"][..], "px135_preset"),
        (&["progress", "list"][..], "px135_progress"),
        (&["resume"][..], "px135_resume"),
    ] {
        assert_open_failure_is_reported(args, name);
    }
}
