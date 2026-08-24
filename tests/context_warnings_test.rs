//! CTX-0076 / issue #105 remainders in `context`: the `--output` write failure
//! was swallowed (`let _ = fs::write`, exit 0) so agents believed context was
//! persisted when the path was unwritable, and event/decision/progress query
//! failures were collapsed to confident empty output via
/// `.ok().unwrap_or_default()`.
mod common;

#[test]
fn test_context_output_write_failure_fails_the_command() {
    let (dir, bin) = common::setup_test_project("context_output_failure");
    common::init_and_agent(&dir, &bin);

    // An unwritable target: parent directory does not exist.
    let bad_path = dir.join("no/such/dir/context.json");
    let out = common::run_cmd(
        &dir,
        &bin,
        &["--json", "context", "--output", bad_path.to_str().unwrap()],
    );
    assert!(
        !out.status.success(),
        "an unwritable --output path must fail the command"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.trim().is_empty(),
        "JSON mode must report the write failure on stderr"
    );
    let envelope: serde_json::Value = serde_json::from_str(stderr.trim())
        .unwrap_or_else(|e| panic!("stderr must be an error envelope: {e}; stderr={stderr}"));
    assert_eq!(envelope["success"], serde_json::Value::Bool(false));
    let message = envelope["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("output") || message.to_lowercase().contains("write"),
        "the message must explain the write failure: {envelope}"
    );

    // Text mode: readable Error line, still non-zero.
    let out = common::run_cmd(
        &dir,
        &bin,
        &["context", "--output", bad_path.to_str().unwrap()],
    );
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.starts_with("Error ["),
        "text mode must print an Error [CODE] line: {stderr}"
    );
}

#[test]
fn test_context_surfaces_query_failures_as_warnings_instead_of_empty_data() {
    let (dir, bin) = common::setup_test_project("context_query_warning");
    common::init_and_agent(&dir, &bin);
    let created = common::run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", "Warn test", "--json"],
    );
    assert!(created.status.success());

    // Break the events table without touching the migration bookkeeping.
    let db = dir.join(".git/carryctx/state.sqlite");
    let status = std::process::Command::new("python3")
        .args([
            "-c",
            &format!(
                "import sqlite3; c = sqlite3.connect({db:?}); c.execute('ALTER TABLE events RENAME TO events_broken'); c.commit()"),
        ])
        .output()
        .expect("sqlite tweak should execute");
    assert!(
        status.status.success(),
        "test fixture failed to break the events table: {}",
        String::from_utf8_lossy(&status.stderr)
    );

    // JSON mode: the command still succeeds (context assembly is best-effort)
    // but the envelope must carry a warning instead of confident empty data.
    let out = common::run_cmd(&dir, &bin, &["--json", "context", "--include-events"]);
    assert!(
        out.status.success(),
        "a broken secondary query must not hard-fail context: {} {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let warnings = value["warnings"].as_array().expect("warnings array");
    assert!(
        warnings.iter().any(|w| w
            .as_str()
            .unwrap_or_default()
            .to_lowercase()
            .contains("event")),
        "a warning about the events query must be present: {value}"
    );

    // Text mode: the same warning surfaces on stderr.
    let out = common::run_cmd(&dir, &bin, &["context", "--include-events"]);
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.to_lowercase().contains("warning") && stderr.to_lowercase().contains("event"),
        "text mode must print a warning line about the events query: {stderr}"
    );
}

#[test]
fn test_resume_surfaces_query_failures_as_warnings_instead_of_empty_data() {
    let (dir, bin) = common::setup_test_project("resume_query_warning");
    common::init_and_agent(&dir, &bin);
    let created = common::run_cmd(
        &dir,
        &bin,
        &["task", "create", "--title", "Resume warn", "--json"],
    );
    assert!(created.status.success());
    let tid_start = String::from_utf8_lossy(&created.stdout);
    let start = tid_start.find("\"display_id\":\"").expect("display id") + 14;
    let rest = &tid_start[start..];
    let tid = &rest[..rest.find('"').expect("closing quote")];

    // Bind the task to the session/worktree so resume actually queries progress.
    common::run_cmd(&dir, &bin, &["task", "claim", tid]);

    let db = dir.join(".git/carryctx/state.sqlite");
    let status = std::process::Command::new("python3")
        .args([
            "-c",
            &format!(
                "import sqlite3; c = sqlite3.connect({db:?}); c.execute('ALTER TABLE progress_items RENAME TO progress_items_broken'); c.commit()"),
        ])
        .output()
        .expect("sqlite tweak should execute");
    assert!(
        status.status.success(),
        "test fixture failed to break the progress table: {}",
        String::from_utf8_lossy(&status.stderr)
    );

    let out = common::run_cmd(&dir, &bin, &["--json", "resume", "--task", tid]);
    assert!(
        out.status.success(),
        "a broken secondary query must not hard-fail resume: {} {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let warnings = value["warnings"].as_array().expect("warnings array");
    assert!(
        warnings.iter().any(|w| w
            .as_str()
            .unwrap_or_default()
            .to_lowercase()
            .contains("progress")),
        "a warning about the progress query must be present: {value}"
    );
}
