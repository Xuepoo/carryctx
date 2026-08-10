mod common;

use std::process::Command;

fn init_project(dir: &std::path::Path, bin: &std::path::Path) {
    Command::new(bin)
        .args(["init", "--force"])
        .current_dir(dir)
        .output()
        .unwrap();
}

fn register_agent(dir: &std::path::Path, bin: &std::path::Path, name: &str) {
    Command::new(bin)
        .args(["agent", "register", "--name", name, "--provider", "test"])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(dir)
        .output()
        .unwrap();
}

fn create_task(dir: &std::path::Path, bin: &std::path::Path, title: &str) -> std::process::Output {
    Command::new(bin)
        .args(["task", "create", "--title", title])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(dir)
        .output()
        .unwrap()
}

// ── Issue #64: `agent current` must honor an explicit `--agent` ───────────

#[test]
fn agent_current_honors_explicit_agent_with_multiple_agents() {
    let (dir, bin) = common::setup_test_project("agent_current_flag");
    init_project(&dir, &bin);
    for name in ["opencode", "kiro", "omp", "claude-code", "antigravity"] {
        register_agent(&dir, &bin, name);
    }

    let out = Command::new(&bin)
        .args(["agent", "current", "--agent", "opencode", "--json"])
        .env("CARRYCTX_AGENT", "opencode")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "agent current --agent should succeed");
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(parsed["success"], true);
    assert_eq!(parsed["data"]["name"], "opencode");
}

#[test]
fn agent_current_without_flag_lists_available_agents_in_error() {
    let (dir, bin) = common::setup_test_project("agent_current_error");
    init_project(&dir, &bin);
    for name in ["opencode", "kiro", "omp"] {
        register_agent(&dir, &bin, name);
    }

    let out = Command::new(&bin)
        .args(["agent", "current", "--json"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Multiple agents exist"),
        "error should mention multiple agents: {stderr}"
    );
    assert!(
        stderr.contains("opencode") && stderr.contains("kiro") && stderr.contains("omp"),
        "error should list available agent names: {stderr}"
    );
}

// ── Issue #65: `task create` surfaces display_id prominently ─────────────

#[test]
fn task_create_text_output_surfaces_display_id() {
    let (dir, bin) = common::setup_test_project("task_create_display");
    init_project(&dir, &bin);
    register_agent(&dir, &bin, "tester");

    let out = create_task(&dir, &bin, "Projection integration task");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Task created: CTX-"),
        "text output should be a short summary with display_id: {stdout}"
    );
    assert!(
        !stdout.contains("\"id\""),
        "default text output should not be pretty-printed JSON: {stdout}"
    );
}

#[test]
fn task_create_json_still_contains_display_id() {
    let (dir, bin) = common::setup_test_project("task_create_json");
    init_project(&dir, &bin);
    register_agent(&dir, &bin, "tester");

    let out = Command::new(&bin)
        .args(["task", "create", "--title", "Json shape task", "--json"])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(&dir)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(parsed["success"], true);
    let data = &parsed["data"];
    assert!(data["display_id"].as_str().unwrap().starts_with("CTX-"));
    assert!(data["title"].as_str().unwrap().contains("Json shape task"));
}

// ── Compact text output by default ───────────────────────────────────────

#[test]
fn task_list_text_is_compact_by_default_and_verbose_with_flag() {
    let (dir, bin) = common::setup_test_project("task_list_compact");
    init_project(&dir, &bin);
    register_agent(&dir, &bin, "tester");
    create_task(&dir, &bin, "Compact list task");

    let out = Command::new(&bin)
        .args(["task", "list"])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(&dir)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("CTX-") && stdout.contains("Compact list task"));
    assert!(
        !stdout.contains("created_at"),
        "default text output should not include full record fields: {stdout}"
    );

    let out = Command::new(&bin)
        .args(["task", "list", "--verbose"])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(&dir)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("created_at"),
        "--verbose should restore full text output"
    );
}

#[test]
fn agent_list_text_is_compact() {
    let (dir, bin) = common::setup_test_project("agent_list_compact");
    init_project(&dir, &bin);
    register_agent(&dir, &bin, "tester");

    let out = Command::new(&bin)
        .args(["agent", "list"])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(&dir)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("tester"));
    assert!(
        !stdout.contains("created_at"),
        "agent list text should be compact: {stdout}"
    );
}

#[test]
fn agent_current_text_is_single_line() {
    let (dir, bin) = common::setup_test_project("agent_current_compact");
    init_project(&dir, &bin);
    register_agent(&dir, &bin, "tester");

    let out = Command::new(&bin)
        .args(["agent", "current", "--agent", "tester"])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.trim(), "tester");
}

// ── Field projection: --fields flag and [output.fields] config ───────────

#[test]
fn fields_flag_projects_json_output() {
    let (dir, bin) = common::setup_test_project("fields_flag");
    init_project(&dir, &bin);
    register_agent(&dir, &bin, "tester");
    create_task(&dir, &bin, "Projected task");

    let out = Command::new(&bin)
        .args(["task", "list", "--json", "--fields", "display_id,title"])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(out.status.success());
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let entry = &parsed["data"][0];
    assert!(entry.get("display_id").is_some(), "display_id kept");
    assert!(entry.get("title").is_some(), "title kept");
    assert!(entry.get("status").is_none(), "status projected out");
}

#[test]
fn output_fields_config_applies_projection_per_command() {
    let (dir, bin) = common::setup_test_project("fields_config");
    init_project(&dir, &bin);
    register_agent(&dir, &bin, "tester");
    create_task(&dir, &bin, "Config projected task");

    std::fs::create_dir_all(dir.join(".carryctx")).unwrap();
    let config = dir.join(".carryctx").join("config.toml");
    std::fs::write(
        &config,
        "[output.fields]\n\"task.list\" = [\"display_id\", \"title\"]\n",
    )
    .unwrap();

    let out = Command::new(&bin)
        .args(["task", "list", "--json"])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(out.status.success());
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let entry = &parsed["data"][0];
    assert!(entry.get("display_id").is_some());
    assert!(
        entry.get("status").is_none(),
        "config should project fields"
    );
}

#[test]
fn output_verbose_config_restores_full_text() {
    let (dir, bin) = common::setup_test_project("verbose_config");
    init_project(&dir, &bin);
    register_agent(&dir, &bin, "tester");
    create_task(&dir, &bin, "Verbose config task");

    std::fs::create_dir_all(dir.join(".carryctx")).unwrap();
    let config = dir.join(".carryctx").join("config.toml");
    std::fs::write(&config, "[output]\nverbose = true\n").unwrap();

    let out = Command::new(&bin)
        .args(["task", "list"])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(&dir)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("created_at"),
        "[output] verbose = true should restore full text output: {stdout}"
    );
}
