//! CTX-0083 / issue #106 item 7: `doctor` exit codes reflect severity.
//! Exit 0 when findings are info/warning only; exit 1 is reserved for
//! error/critical findings (or infra failure). The rendered report itself
//! (statuses, messages, fix hints) stays byte-for-byte the same.

mod common;

use serde_json::Value;
use std::process::Command;

fn run(dir: &std::path::Path, bin: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(bin)
        .args(args)
        .env_remove("CARRYCTX_AGENT")
        .current_dir(dir)
        .output()
        .expect("doctor should execute")
}

fn doctor_json(out: &std::process::Output) -> Value {
    serde_json::from_slice(&out.stdout).expect("valid doctor JSON envelope")
}

#[test]
fn healthy_project_exits_zero() {
    let (dir, bin) = common::setup_test_project("doctor_exit_healthy");
    common::run_cmd(&dir, &bin, &["init", "--force"]);

    let out = run(&dir, &bin, &["doctor", "--json"]);
    assert!(
        out.status.success(),
        "healthy project must exit 0: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value = doctor_json(&out);
    assert_eq!(value["data"]["all_ok"], true);
}

/// A stale worktree registration is warning-class only: the report still
/// lists the finding and its fix command, but the process exits 0 instead
/// of the old blanket rc=1 for "any" issue.
#[test]
fn warning_only_findings_exit_zero_but_still_report() {
    let (dir, bin) = common::setup_test_project("doctor_exit_warning");
    common::run_cmd(&dir, &bin, &["init", "--force", "--task-prefix", "DT"]);
    common::run_cmd(
        &dir,
        &bin,
        &[
            "agent",
            "register",
            "--name",
            "tester",
            "--provider",
            "test",
        ],
    );
    common::run_cmd(&dir, &bin, &["task", "create", "--title", "stale wt"]);
    let created = common::run_cmd(&dir, &bin, &["worktree", "create", "DT-0001", "--json"]);
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );

    // Orphan the registration by deleting only the directory.
    std::fs::remove_dir_all(dir.join(".worktrees/dt-0001")).unwrap();

    let out = run(&dir, &bin, &["doctor", "--json"]);
    assert!(
        out.status.success(),
        "warning-only findings must exit 0 (severity-based exit codes)"
    );
    let value = doctor_json(&out);
    let stale = value["data"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["check"] == "worktrees.stale")
        .expect("stale worktree check present");
    // Output is unchanged: the warning and fix hint are still reported.
    assert_eq!(stale["status"], "warning");
    assert_eq!(stale["count"], 1);
    assert_eq!(
        stale["fix_command"],
        "carryctx doctor --prune-stale-worktrees"
    );
}

/// An error-class finding (invalid global config) keeps exit 1 while the
/// rest of the report renders normally.
#[test]
fn error_class_findings_exit_one_and_still_report() {
    let (dir, bin) = common::setup_test_project("doctor_exit_error");
    common::run_cmd(&dir, &bin, &["init", "--force"]);

    let xdg_config =
        std::env::temp_dir().join(format!("carryctx_doctor_xdg_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&xdg_config);
    std::fs::create_dir_all(xdg_config.join("carryctx")).unwrap();
    std::fs::write(
        xdg_config.join("carryctx").join("config.toml"),
        "not [valid toml",
    )
    .unwrap();

    let out = Command::new(&bin)
        .args(["doctor", "--json"])
        .env_remove("CARRYCTX_AGENT")
        .env("XDG_CONFIG_HOME", &xdg_config)
        .current_dir(&dir)
        .output()
        .expect("doctor should execute");

    assert!(!out.status.success(), "error-class findings must exit 1");
    assert_eq!(out.status.code(), Some(1));
    let value = doctor_json(&out);
    let config_check = value["data"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["check"] == "config.global")
        .expect("config.global check present");
    assert_eq!(config_check["status"], "error");

    let _ = std::fs::remove_dir_all(&xdg_config);
}
