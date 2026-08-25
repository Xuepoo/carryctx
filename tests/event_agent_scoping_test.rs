//! CTX-0083 / issue #106 item 2: `event list` is an audit surface and must
//! NOT be implicitly scoped by the ambient `CARRYCTX_AGENT` environment
//! variable. Without an explicit `--agent` flag it shows everything,
//! including null-actor system events (`project.initialized`, …); with an
//! explicit `--agent` it filters to that agent. Mutating-command identity
//! attribution via the ambient env must keep working unchanged.

mod common;

use serde_json::Value;
use std::process::Command;

fn run_scoped(
    dir: &std::path::Path,
    bin: &std::path::Path,
    agent_env: Option<(&str, &str)>,
    args: &[&str],
) -> std::process::Output {
    let mut command = Command::new(bin);
    command.args(args).current_dir(dir);
    if let Some((key, value)) = agent_env {
        command.env(key, value);
    } else {
        command.env_remove("CARRYCTX_AGENT");
    }
    command.output().expect("command should execute")
}

fn json_stdout(out: &std::process::Output) -> Value {
    serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("valid JSON envelope")
}

fn setup_two_agent_project(name: &str) -> (std::path::PathBuf, std::path::PathBuf, String) {
    let (dir, bin) = common::setup_test_project(name);
    common::run_cmd(&dir, &bin, &["init", "--force"]);
    for who in ["alice", "bob"] {
        let out = run_scoped(
            &dir,
            &bin,
            Some(("CARRYCTX_AGENT", who)),
            &["agent", "register", "--name", who, "--json"],
        );
        assert!(out.status.success(), "register {who} failed");
    }
    let mut bob_display_id = String::new();
    for (who, title) in [("alice", "alice task"), ("bob", "bob task")] {
        let out = run_scoped(
            &dir,
            &bin,
            Some(("CARRYCTX_AGENT", who)),
            &["task", "create", "--title", title, "--json"],
        );
        assert!(out.status.success(), "{who} task create failed");
        if who == "bob" {
            bob_display_id = json_stdout(&out)["data"]["display_id"]
                .as_str()
                .unwrap()
                .to_string();
        }
    }
    (dir, bin, bob_display_id)
}

#[test]
fn ambient_carryctx_agent_does_not_scope_event_list() {
    let (dir, bin, _) = setup_two_agent_project("event_scope_ambient");

    // Ground truth: no env → all events.
    let all = run_scoped(&dir, &bin, None, &["--format", "json", "event", "list"]);
    assert!(all.status.success());
    let all_value = json_stdout(&all);
    let all_events = all_value["data"]["events"].as_array().unwrap().len();
    assert!(all_events >= 5, "expected several events, got {all_events}");

    // Ambient env must not narrow the audit listing.
    let scoped = run_scoped(
        &dir,
        &bin,
        Some(("CARRYCTX_AGENT", "alice")),
        &["--format", "json", "event", "list"],
    );
    assert!(
        scoped.status.success(),
        "ambient env must not turn into a failed filter: {}",
        String::from_utf8_lossy(&scoped.stderr)
    );
    let scoped_value = json_stdout(&scoped);
    let scoped_events = scoped_value["data"]["events"].as_array().unwrap();

    assert_eq!(
        scoped_events.len(),
        all_events,
        "`event list` with ambient CARRYCTX_AGENT must return ALL events"
    );

    // Null-actor system events stay visible under ambient env.
    assert!(
        scoped_events.iter().any(|e| e["actor_agent_id"].is_null()),
        "null-actor events (e.g. project.initialized) must not be hidden"
    );
}

#[test]
fn explicit_event_list_agent_flag_filters() {
    let (dir, bin, _) = setup_two_agent_project("event_scope_explicit");

    let alice_id: String = {
        let db = rusqlite::Connection::open(dir.join(".git/carryctx/state.sqlite")).unwrap();
        db.query_row("SELECT id FROM agents WHERE name = 'alice'", [], |row| {
            row.get(0)
        })
        .unwrap()
    };

    let out = run_scoped(
        &dir,
        &bin,
        None,
        &["--format", "json", "event", "list", "--agent", &alice_id],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value = json_stdout(&out);
    let events = value["data"]["events"].as_array().unwrap();
    assert!(!events.is_empty(), "alice has events");
    for e in events {
        assert_eq!(
            e["actor_agent_id"].as_str(),
            Some(alice_id.as_str()),
            "explicit --agent must filter to that actor"
        );
    }

    // Explicit flag wins even when the ambient env holds another agent.
    let out_mixed = run_scoped(
        &dir,
        &bin,
        Some(("CARRYCTX_AGENT", "bob")),
        &["--format", "json", "event", "list", "--agent", &alice_id],
    );
    assert!(out_mixed.status.success());
    let mixed_value = json_stdout(&out_mixed);
    for e in mixed_value["data"]["events"].as_array().unwrap() {
        assert_eq!(e["actor_agent_id"].as_str(), Some(alice_id.as_str()));
    }
}

#[test]
fn ambient_agent_still_attributed_on_mutations() {
    let (dir, bin, task_ref) = setup_two_agent_project("event_scope_attribution");

    // Identity resolution for mutating commands must keep working from the
    // ambient env: a new write is attributed to the ambient actor.
    let out = run_scoped(
        &dir,
        &bin,
        Some(("CARRYCTX_AGENT", "bob")),
        &[
            "--format",
            "json",
            "progress",
            "note",
            "--task",
            &task_ref,
            "still here",
        ],
    );
    assert!(
        out.status.success(),
        "ambient-env mutation failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
