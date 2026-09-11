//! End-to-end tests for the executable-policy trust model (CTX-0100).
//!
//! Every test runs the real binary with isolated XDG directories so the
//! user-local trust registry never touches the developer's real state.

mod common;

use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::Value;

struct Env {
    dir: PathBuf,
    bin: PathBuf,
    xdg: PathBuf,
}

fn new_env(name: &str) -> Env {
    let (dir, bin) = common::setup_test_project(name);
    let xdg = dir.join("xdg");
    std::fs::create_dir_all(&xdg).unwrap();
    let env = Env { dir, bin, xdg };
    assert!(run(&env, &["init", "--force"]).status.success());
    assert!(
        run(
            &env,
            &[
                "agent",
                "register",
                "--name",
                "tester",
                "--provider",
                "test"
            ]
        )
        .status
        .success()
    );
    env
}

fn run(env: &Env, args: &[&str]) -> Output {
    Command::new(&env.bin)
        .args(args)
        .env("CARRYCTX_AGENT", "tester")
        .env_remove("CARRYCTX_SESSION")
        .env_remove("CARRYCTX_ALLOW_PROJECT_COMMANDS")
        .env("XDG_STATE_HOME", env.xdg.join("state"))
        .env("XDG_CONFIG_HOME", env.xdg.join("config"))
        .env("XDG_DATA_HOME", env.xdg.join("data"))
        .env("XDG_CACHE_HOME", env.xdg.join("cache"))
        .current_dir(&env.dir)
        .output()
        .expect("carryctx should execute")
}

fn json(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout was not JSON ({error}); stderr={}",
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

/// Error envelopes are written to stderr in JSON mode.
fn json_err(out: &Output) -> Value {
    serde_json::from_slice(&out.stderr).unwrap_or_else(|error| {
        panic!(
            "stderr was not JSON ({error}); stderr={}",
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn registry_path(env: &Env) -> PathBuf {
    env.xdg
        .join("state")
        .join("carryctx")
        .join("trusted-projects.json")
}

fn set_global_gate(env: &Env, allow: bool) {
    let dir = env.xdg.join("config").join("carryctx");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("config.toml"),
        format!("[security]\nallow_project_commands = {allow}\n"),
    )
    .unwrap();
}

fn set_verification(env: &Env, commands: &[&str]) {
    let path = env.dir.join(".carryctx").join("config.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    let list = commands
        .iter()
        .map(|command| format!("\"{command}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let replacement = format!("commands = [{list}]");
    let rewritten: Vec<String> = text
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("commands =") {
                replacement.clone()
            } else {
                line.to_string()
            }
        })
        .collect();
    assert!(
        rewritten.iter().any(|line| line == &replacement),
        "generated config must declare a verification command list"
    );
    std::fs::write(&path, rewritten.join("\n") + "\n").unwrap();
}

fn write_registry_raw(env: &Env, contents: &str) {
    let path = registry_path(env);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, contents).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn effective(env: &Env) -> Value {
    let out = run(env, &["trust", "status", "--json"]);
    assert!(out.status.success(), "trust status should succeed");
    json(&out)["data"].clone()
}

fn assert_exit(out: &Output, code: i32) {
    assert_eq!(
        out.status.code(),
        Some(code),
        "unexpected exit; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn deny_by_default_reports_blocked_and_creates_no_registry() {
    let env = new_env("trust_deny_default");
    set_verification(&env, &["echo hello"]);

    let status = effective(&env);
    assert_eq!(status["effective"], "blocked");
    assert_eq!(status["reason"], "global_disabled");
    assert_eq!(status["global_allow_project_commands"], false);
    assert_eq!(status["external_policy_present"], true);
    assert_eq!(status["trusted"], false);
    assert_eq!(status["registry_state"], "absent");
    assert!(!registry_path(&env).exists());

    // Built-in (non-executable) commands are never gated by this model.
    assert!(run(&env, &["task", "list", "--json"]).status.success());
}

#[test]
fn grant_without_yes_fails_closed_and_writes_nothing() {
    let env = new_env("trust_grant_no_yes");
    set_verification(&env, &["echo hello"]);
    set_global_gate(&env, true);

    let out = run(&env, &["trust", "grant", "--json"]);
    assert_exit(&out, 2);
    assert_eq!(json_err(&out)["error"]["code"], "INVALID_ARGUMENTS");
    assert!(
        !registry_path(&env).exists(),
        "a refused grant must not create the registry"
    );
}

#[test]
fn grant_creates_private_registry_and_allows_when_gate_on() {
    let env = new_env("trust_grant_ok");
    set_verification(&env, &["echo hello"]);
    set_global_gate(&env, true);

    let out = run(&env, &["trust", "grant", "--yes", "--json"]);
    assert!(out.status.success(), "grant should succeed with --yes");
    let data = json(&out)["data"].clone();
    assert_eq!(data["trusted"], true);
    assert_eq!(data["command_count"], 1);
    assert!(data["decided_by"].is_string());

    let path = registry_path(&env);
    assert!(path.exists(), "registry must exist after grant");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "registry must be 0600");
        let dir_mode = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700, "registry directory must be 0700");
    }

    let status = effective(&env);
    assert_eq!(status["effective"], "allowed");
    assert_eq!(status["reason"], "allowed");
    assert_eq!(status["trusted"], true);
}

#[test]
fn global_gate_off_blocks_even_a_trusted_project() {
    let env = new_env("trust_gate_off");
    set_verification(&env, &["echo hello"]);
    set_global_gate(&env, false);

    assert!(
        run(&env, &["trust", "grant", "--yes", "--json"])
            .status
            .success()
    );
    let status = effective(&env);
    assert_eq!(status["effective"], "blocked");
    assert_eq!(status["reason"], "global_disabled");
    // The per-project grant is still visible for reporting.
    assert_eq!(status["trusted"], true);
}

#[test]
fn policy_change_invalidates_the_grant() {
    let env = new_env("trust_policy_change");
    set_verification(&env, &["echo hello"]);
    set_global_gate(&env, true);
    assert!(
        run(&env, &["trust", "grant", "--yes", "--json"])
            .status
            .success()
    );
    assert_eq!(effective(&env)["effective"], "allowed");

    set_verification(&env, &["echo hello", "curl evil.example"]);
    let status = effective(&env);
    assert_eq!(status["effective"], "blocked");
    assert_eq!(status["reason"], "policy_changed");
    assert_eq!(status["trusted"], false);
}

#[test]
fn revoke_blocks_and_is_idempotent() {
    let env = new_env("trust_revoke");
    set_verification(&env, &["echo hello"]);
    set_global_gate(&env, true);
    assert!(
        run(&env, &["trust", "grant", "--yes", "--json"])
            .status
            .success()
    );

    let first = run(&env, &["trust", "revoke", "--json"]);
    assert!(first.status.success());
    assert_eq!(json(&first)["data"]["removed"], true);
    assert_eq!(effective(&env)["effective"], "blocked");

    let second = run(&env, &["trust", "revoke", "--json"]);
    assert!(second.status.success());
    assert_eq!(json(&second)["data"]["removed"], false);
}

#[test]
fn audit_events_record_trust_changes_without_command_text() {
    let env = new_env("trust_audit");
    set_verification(&env, &["echo super-secret-token"]);
    set_global_gate(&env, true);
    assert!(
        run(&env, &["trust", "grant", "--yes", "--json"])
            .status
            .success()
    );
    assert!(run(&env, &["trust", "revoke", "--json"]).status.success());

    let out = run(&env, &["event", "list", "--json"]);
    assert!(out.status.success());
    let events = json(&out)["data"]["events"].as_array().unwrap().clone();
    let granted = events
        .iter()
        .find(|event| event["event_type"] == "project.trust_granted")
        .expect("project.trust_granted event");
    let revoked = events
        .iter()
        .find(|event| event["event_type"] == "project.trust_revoked")
        .expect("project.trust_revoked event");

    assert_eq!(granted["payload"]["command_count"], 1);
    assert!(
        granted["payload"]["policy_fingerprint"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert!(granted["actor_agent_id"].is_string());
    assert_eq!(revoked["payload"], serde_json::json!({}));

    // No event payload may embed repository command text.
    let serialized = serde_json::to_string(&granted["payload"]).unwrap();
    assert!(
        !serialized.contains("super-secret-token") && !serialized.contains("echo"),
        "event payload leaked command text: {serialized}"
    );
}

#[test]
fn malformed_registry_fails_closed_and_grant_refuses_to_overwrite() {
    let env = new_env("trust_malformed");
    set_verification(&env, &["echo hello"]);
    set_global_gate(&env, true);
    write_registry_raw(&env, "{ not json");

    let status = effective(&env);
    assert_eq!(status["effective"], "blocked");
    assert_eq!(status["registry_state"], "malformed");
    assert_eq!(status["trusted"], false);

    let out = run(&env, &["trust", "grant", "--yes", "--json"]);
    assert_exit(&out, 6);
    assert_eq!(json_err(&out)["error"]["code"], "CONFIGURATION_ERROR");
    assert_eq!(
        std::fs::read_to_string(registry_path(&env)).unwrap(),
        "{ not json",
        "a malformed registry must never be silently rewritten"
    );
}

#[cfg(unix)]
#[test]
fn group_readable_registry_fails_closed() {
    use std::os::unix::fs::PermissionsExt;

    let env = new_env("trust_insecure");
    set_verification(&env, &["echo hello"]);
    set_global_gate(&env, true);
    write_registry_raw(&env, r#"{"schema_version":1,"trusted":{}}"#);
    std::fs::set_permissions(registry_path(&env), std::fs::Permissions::from_mode(0o644)).unwrap();

    let status = effective(&env);
    assert_eq!(status["effective"], "blocked");
    assert_eq!(status["registry_state"], "insecure");
}

#[test]
fn dry_run_grant_does_not_write_the_registry() {
    let env = new_env("trust_dry_run");
    set_verification(&env, &["echo hello"]);
    set_global_gate(&env, true);

    let out = run(&env, &["trust", "grant", "--yes", "--dry-run", "--json"]);
    assert!(out.status.success());
    assert_eq!(json(&out)["data"]["dry_run"], true);
    assert!(!registry_path(&env).exists());
}

#[test]
fn list_works_outside_a_repository() {
    let dir = std::env::temp_dir().join(format!("carryctx_trust_list_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let xdg = dir.join("xdg");

    let out = Command::new(common::test_binary())
        .args(["trust", "list", "--json"])
        .env_remove("CARRYCTX_AGENT")
        .env("XDG_STATE_HOME", xdg.join("state"))
        .env("XDG_CONFIG_HOME", xdg.join("config"))
        .env("XDG_DATA_HOME", xdg.join("data"))
        .env("XDG_CACHE_HOME", xdg.join("cache"))
        .current_dir(&dir)
        .output()
        .expect("trust list should execute");

    assert!(
        out.status.success(),
        "trust list must not require a Git repository; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let payload = json(&out);
    assert_eq!(payload["data"]["registry_state"], "absent");
    assert_eq!(payload["data"]["projects"].as_array().unwrap().len(), 0);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn project_level_security_table_is_ignored() {
    // A repository must not be able to enable the global security gate.
    let env = new_env("trust_project_security_ignored");
    set_verification(&env, &["echo hello"]);
    let project_config = env.dir.join(".carryctx").join("config.toml");
    let text = std::fs::read_to_string(&project_config).unwrap();
    std::fs::write(
        &project_config,
        format!("{text}\n[security]\nallow_project_commands = true\n"),
    )
    .unwrap();

    let status = effective(&env);
    assert_eq!(
        status["global_allow_project_commands"], false,
        "project-level [security] must not enable the global gate"
    );
    assert_eq!(status["effective"], "blocked");
}
