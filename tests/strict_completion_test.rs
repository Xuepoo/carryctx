mod common;

use common::{init_and_agent, run_cmd, setup_test_project};

/// With `strict_completion = false` (the default), completing a task that
/// still has an open progress item must succeed and surface a warning in the
/// success envelope. Regression test for the warnings being silently
/// discarded at every `task <transition>` call site.
#[test]
fn test_complete_with_open_progress_warns_when_not_strict() {
    let (dir, bin) = setup_test_project("strict_completion_warn");
    init_and_agent(&dir, &bin);

    let create = run_cmd(
        &dir,
        &bin,
        &["--json", "task", "create", "--title", "warn test"],
    );
    assert!(create.status.success());
    let created: serde_json::Value =
        serde_json::from_slice(&create.stdout).expect("task create should print JSON");
    let task_ref = created["data"]["display_id"].as_str().unwrap().to_string();

    let todo = run_cmd(
        &dir,
        &bin,
        &[
            "--json",
            "progress",
            "todo",
            "--task",
            &task_ref,
            "an open item",
        ],
    );
    assert!(todo.status.success(), "progress todo should succeed");

    let start = run_cmd(&dir, &bin, &["task", "start", &task_ref]);
    assert!(start.status.success(), "task start should succeed");

    let complete = run_cmd(&dir, &bin, &["--json", "task", "complete", &task_ref]);
    assert!(
        complete.status.success(),
        "non-strict complete with an open item should still succeed: {}",
        String::from_utf8_lossy(&complete.stderr)
    );
    let envelope: serde_json::Value =
        serde_json::from_slice(&complete.stdout).expect("task complete should print JSON");
    assert_eq!(envelope["data"]["status"], "completed");
    let warnings = envelope["warnings"]
        .as_array()
        .expect("success envelope should carry a warnings array");
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap_or_default().contains("open progress")),
        "expected an open-progress warning, got: {warnings:?}"
    );
}

/// With `strict_completion = true`, completing a task that still has an open
/// progress item must be REJECTED (not silently allowed). Regression test for
/// the match-arm ordering bug in `evaluate_transition` where
/// `(Ac::Complete, St::Review | St::InProgress) => true` matched before the
/// `strict_completion` guard could ever run.
#[test]
fn test_complete_with_open_progress_blocked_when_strict() {
    let (dir, bin) = setup_test_project("strict_completion_block");
    init_and_agent(&dir, &bin);

    let create = run_cmd(
        &dir,
        &bin,
        &["--json", "task", "create", "--title", "strict test"],
    );
    assert!(create.status.success());
    let created: serde_json::Value =
        serde_json::from_slice(&create.stdout).expect("task create should print JSON");
    let task_ref = created["data"]["display_id"].as_str().unwrap().to_string();

    let todo = run_cmd(
        &dir,
        &bin,
        &[
            "--json",
            "progress",
            "todo",
            "--task",
            &task_ref,
            "an open item",
        ],
    );
    assert!(todo.status.success(), "progress todo should succeed");
    let todo_json: serde_json::Value =
        serde_json::from_slice(&todo.stdout).expect("progress todo should print JSON");
    let progress_ref = todo_json["data"]["display_id"]
        .as_str()
        .unwrap()
        .to_string();

    let start = run_cmd(&dir, &bin, &["task", "start", &task_ref]);
    assert!(start.status.success(), "task start should succeed");

    // strict_completion = true via env, from a state where Complete would
    // otherwise be legal.
    let complete = std::process::Command::new(&bin)
        .args(["--json", "task", "complete", &task_ref])
        .env("CARRYCTX_AGENT", "tester")
        .env("CARRYCTX_STRICT_COMPLETION", "true")
        .current_dir(&dir)
        .output()
        .expect("command should execute");
    assert!(
        !complete.status.success(),
        "strict complete with an open item must be rejected, but succeeded: {}",
        String::from_utf8_lossy(&complete.stdout)
    );
    let envelope: serde_json::Value =
        serde_json::from_slice(&complete.stderr).expect("error envelope should print JSON");
    assert_eq!(envelope["error"]["code"], "STATE_CONFLICT");

    // The task must still be in_progress, not completed.
    let show = run_cmd(&dir, &bin, &["task", "show", &task_ref, "--json"]);
    assert!(show.status.success());
    let show_json: serde_json::Value =
        serde_json::from_slice(&show.stdout).expect("task show should print JSON");
    assert_eq!(show_json["data"]["status"], "in_progress");

    // Closing the open item, then completing, must succeed under the same
    // strict setting.
    let progress_complete = std::process::Command::new(&bin)
        .args(["progress", "complete", &progress_ref])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(&dir)
        .output()
        .expect("command should execute");
    assert!(progress_complete.status.success());

    let complete_again = std::process::Command::new(&bin)
        .args(["--json", "task", "complete", &task_ref])
        .env("CARRYCTX_AGENT", "tester")
        .env("CARRYCTX_STRICT_COMPLETION", "true")
        .current_dir(&dir)
        .output()
        .expect("command should execute");
    assert!(
        complete_again.status.success(),
        "strict complete with no open items should succeed: {}",
        String::from_utf8_lossy(&complete_again.stderr)
    );
}

// ── Issue #223: native completion hooks and verification profiles (CTX-0010) ──

fn run_with_xdg(
    dir: &std::path::Path,
    bin: &std::path::Path,
    xdg: &std::path::Path,
    args: &[&str],
) -> std::process::Output {
    std::process::Command::new(bin)
        .args(args)
        .env("CARRYCTX_AGENT", "tester")
        .env("CARRYCTX_ALLOW_PROJECT_COMMANDS", "true")
        .env("XDG_STATE_HOME", xdg.join("state"))
        .env("XDG_CONFIG_HOME", xdg.join("config"))
        .env("XDG_DATA_HOME", xdg.join("data"))
        .env("XDG_CACHE_HOME", xdg.join("cache"))
        .current_dir(dir)
        .output()
        .expect("carryctx should execute")
}

#[test]
fn test_pre_task_complete_verification_profile_enforced_and_audited() {
    let (dir, bin) = setup_test_project("pre_task_complete_gate");
    let xdg = dir.join("xdg");
    std::fs::create_dir_all(&xdg).unwrap();
    init_and_agent(&dir, &bin);

    // Configure [task.completion] and [verification.profiles]
    let cfg_path = dir.join(".carryctx").join("config.toml");
    let mut cfg = std::fs::read_to_string(&cfg_path).unwrap();
    cfg.push_str(
        r#"
[task.completion]
strict = true
profile = "strict"

[verification.profiles]
fast = ["true"]
strict = ["false"]
"#,
    );
    std::fs::write(&cfg_path, cfg).unwrap();

    let created: serde_json::Value = serde_json::from_slice(
        &run_with_xdg(
            &dir,
            &bin,
            &xdg,
            &["--json", "task", "create", "--title", "verify me"],
        )
        .stdout,
    )
    .unwrap();
    let task_ref = created["data"]["display_id"].as_str().unwrap().to_string();
    assert!(
        run_with_xdg(&dir, &bin, &xdg, &["task", "start", &task_ref])
            .status
            .success()
    );

    // 1. Without project trust granted, completion must fail closed with TRUST_DENIED (exit 9).
    let untrusted = run_with_xdg(&dir, &bin, &xdg, &["--json", "task", "complete", &task_ref]);
    assert_eq!(untrusted.status.code(), Some(9));
    let untrusted_err: serde_json::Value = serde_json::from_slice(&untrusted.stderr).unwrap();
    assert_eq!(untrusted_err["error"]["code"], "TRUST_DENIED");

    // 2. Grant project trust.
    assert!(
        run_with_xdg(&dir, &bin, &xdg, &["--json", "trust", "grant", "--yes"])
            .status
            .success()
    );

    // 3. Default configured profile is 'strict' (which runs `false` -> exit 1).
    //    Completion must abort with VERIFICATION_FAILED (exit 8) and keep task in_progress.
    let failed = run_with_xdg(&dir, &bin, &xdg, &["--json", "task", "complete", &task_ref]);
    assert_eq!(failed.status.code(), Some(8));
    let failed_err: serde_json::Value = serde_json::from_slice(&failed.stderr).unwrap();
    assert_eq!(failed_err["error"]["code"], "VERIFICATION_FAILED");
    assert_eq!(failed_err["error"]["details"]["profile"], "strict");

    let show: serde_json::Value = serde_json::from_slice(
        &run_with_xdg(&dir, &bin, &xdg, &["--json", "task", "show", &task_ref]).stdout,
    )
    .unwrap();
    assert_eq!(show["data"]["status"], "in_progress");

    // 4. Passing `--verification-profile fast` runs `true` and completes the task.
    let passed = run_with_xdg(
        &dir,
        &bin,
        &xdg,
        &[
            "--json",
            "task",
            "complete",
            &task_ref,
            "--verification-profile",
            "fast",
        ],
    );
    assert!(
        passed.status.success(),
        "fast profile should pass: {}",
        String::from_utf8_lossy(&passed.stderr)
    );
    let passed_env: serde_json::Value = serde_json::from_slice(&passed.stdout).unwrap();
    assert_eq!(passed_env["data"]["status"], "completed");

    // 5. Verify `carryctx.pre-task-complete` audit events were recorded for both attempts.
    let events_out = run_with_xdg(
        &dir,
        &bin,
        &xdg,
        &[
            "--json",
            "event",
            "list",
            "--event-type",
            "carryctx.pre-task-complete",
        ],
    );
    assert!(events_out.status.success());
    let events_json: serde_json::Value = serde_json::from_slice(&events_out.stdout).unwrap();
    let events = events_json["data"]["events"].as_array().unwrap();
    assert_eq!(events.len(), 2);
}

#[test]
fn test_pre_task_complete_no_verify_skips_hooks_with_warning() {
    let (dir, bin) = setup_test_project("pre_task_complete_skip");
    let xdg = dir.join("xdg");
    std::fs::create_dir_all(&xdg).unwrap();
    init_and_agent(&dir, &bin);

    let cfg_path = dir.join(".carryctx").join("config.toml");
    let mut cfg = std::fs::read_to_string(&cfg_path).unwrap();
    cfg.push_str(
        r#"
[task.completion]
strict = true
commands = ["false"]
"#,
    );
    std::fs::write(&cfg_path, cfg).unwrap();

    let created: serde_json::Value = serde_json::from_slice(
        &run_with_xdg(
            &dir,
            &bin,
            &xdg,
            &["--json", "task", "create", "--title", "skip verify"],
        )
        .stdout,
    )
    .unwrap();
    let task_ref = created["data"]["display_id"].as_str().unwrap().to_string();
    assert!(
        run_with_xdg(&dir, &bin, &xdg, &["task", "start", &task_ref])
            .status
            .success()
    );

    let skipped = run_with_xdg(
        &dir,
        &bin,
        &xdg,
        &["--json", "task", "complete", &task_ref, "--no-verify"],
    );
    assert!(
        skipped.status.success(),
        "--no-verify should bypass failing hook: {}",
        String::from_utf8_lossy(&skipped.stderr)
    );
    let envelope: serde_json::Value = serde_json::from_slice(&skipped.stdout).unwrap();
    assert_eq!(envelope["data"]["status"], "completed");
    assert!(
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap_or_default().contains("--no-verify"))
    );
}
