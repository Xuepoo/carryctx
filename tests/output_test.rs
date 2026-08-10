mod common;

use std::process::{Command, Stdio};

#[cfg(unix)]
#[test]
fn piping_to_a_closed_stdout_does_not_panic() {
    let (dir, bin) = common::setup_test_project("pipe_epipe");
    init_project(&dir, &bin);
    register_agent(&dir, &bin, "tester");
    for i in 0..30 {
        create_task(
            &dir,
            &bin,
            &format!("Pipe test task {i} with a reasonably long title"),
        );
    }

    let mut child = Command::new(&bin)
        .args(["task", "list"])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(&dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Close the child's stdout immediately: the child's first write hits a
    // broken pipe, exactly like `carryctx task list | head -1`.
    drop(child.stdout.take());

    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked"),
        "broken pipe must not panic: {stderr}"
    );
}

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
// ── Issue #70: task create/edit must persist --description ───────────────

#[test]
fn task_create_persists_description() {
    let (dir, bin) = common::setup_test_project("task_desc_create");
    init_project(&dir, &bin);
    register_agent(&dir, &bin, "tester");

    let create = Command::new(&bin)
        .args([
            "task",
            "create",
            "--title",
            "Described task",
            "--description",
            "probe body",
            "--json",
        ])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(create.status.success(), "task create --description");
    let created: serde_json::Value = serde_json::from_slice(&create.stdout).unwrap();
    assert_eq!(created["data"]["description"], "probe body");

    // Re-read from the DB, not the create response.
    let show = Command::new(&bin)
        .args(["task", "show", "CTX-0001", "--json"])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(show.status.success());
    let shown: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(shown["data"]["description"], "probe body");
}

#[test]
fn task_edit_sets_description() {
    let (dir, bin) = common::setup_test_project("task_desc_edit");
    init_project(&dir, &bin);
    register_agent(&dir, &bin, "tester");
    create_task(&dir, &bin, "Undescribed task");

    let edit = Command::new(&bin)
        .args([
            "task",
            "edit",
            "CTX-0001",
            "--description",
            "filled in later",
            "--json",
        ])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(edit.status.success(), "task edit --description");

    let show = Command::new(&bin)
        .args(["task", "show", "CTX-0001", "--json"])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(&dir)
        .output()
        .unwrap();
    let shown: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(shown["data"]["description"], "filled in later");
}

// ── Issue #69: argument parse errors must render, not exit silently ──────

#[test]
fn task_depend_invalid_kind_renders_error_in_text_mode() {
    let (dir, bin) = common::setup_test_project("depend_kind_text");
    init_project(&dir, &bin);
    register_agent(&dir, &bin, "tester");
    create_task(&dir, &bin, "Prereq task");
    create_task(&dir, &bin, "Dependent task");

    let out = Command::new(&bin)
        .args([
            "task", "depend", "CTX-0002", "--on", "CTX-0001", "--kind", "blocks",
        ])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(!out.status.success(), "invalid --kind must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Unknown dependency kind: blocks"),
        "text mode must print the parse error: {stderr}"
    );
}

#[test]
fn task_depend_invalid_kind_emits_json_envelope() {
    let (dir, bin) = common::setup_test_project("depend_kind_json");
    init_project(&dir, &bin);
    register_agent(&dir, &bin, "tester");
    create_task(&dir, &bin, "Prereq task");
    create_task(&dir, &bin, "Dependent task");

    let out = Command::new(&bin)
        .args([
            "task", "depend", "CTX-0002", "--on", "CTX-0001", "--kind", "blocks", "--json",
        ])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        out.stdout.is_empty(),
        "stdout must stay clean for JSON consumers"
    );
    let envelope: serde_json::Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(envelope["success"], false);
    assert_eq!(envelope["error"]["code"], "INVALID_ARGUMENTS");
    assert_eq!(
        envelope["error"]["message"],
        "Unknown dependency kind: blocks"
    );

    // No edge may have been created.
    let show = Command::new(&bin)
        .args(["task", "show", "CTX-0002", "--json"])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(&dir)
        .output()
        .unwrap();
    let shown: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(shown["data"]["depends_on"].as_array().unwrap().len(), 0);
}

#[test]
fn task_depend_valid_kind_still_works() {
    let (dir, bin) = common::setup_test_project("depend_kind_ok");
    init_project(&dir, &bin);
    register_agent(&dir, &bin, "tester");
    create_task(&dir, &bin, "Prereq task");
    create_task(&dir, &bin, "Dependent task");

    let out = Command::new(&bin)
        .args([
            "task", "depend", "CTX-0002", "--on", "CTX-0001", "--kind", "strong",
        ])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "valid --kind must succeed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Dependency added to task CTX-0002"),
        "text mode should confirm the edge: {stdout}"
    );
}

#[test]
fn task_create_invalid_status_renders_error() {
    let (dir, bin) = common::setup_test_project("create_status_err");
    init_project(&dir, &bin);
    register_agent(&dir, &bin, "tester");

    let out = Command::new(&bin)
        .args(["task", "create", "--title", "x", "--status", "bogus"])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Unknown status: bogus"),
        "status parse errors must render too: {stderr}"
    );
}

// ── Issue #68: compact text must honor the field projection ──────────────

fn setup_dependent_pair(
    dir: &std::path::Path,
    bin: &std::path::Path,
) -> (std::process::Output, std::process::Output) {
    let a = Command::new(bin)
        .args(["task", "create", "--title", "Prereq pair task"])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(dir)
        .output()
        .unwrap();
    let b = Command::new(bin)
        .args(["task", "create", "--title", "Dependent pair task"])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(dir)
        .output()
        .unwrap();
    Command::new(bin)
        .args(["task", "depend", "CTX-0002", "--on", "CTX-0001"])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(dir)
        .output()
        .unwrap();
    (a, b)
}

#[test]
fn task_show_projected_depends_on_is_rendered_in_compact_text() {
    let (dir, bin) = common::setup_test_project("show_projected_deps");
    init_project(&dir, &bin);
    register_agent(&dir, &bin, "tester");
    setup_dependent_pair(&dir, &bin);

    let out = Command::new(&bin)
        .args([
            "task",
            "show",
            "CTX-0002",
            "--fields",
            "display_id,status,title,depends_on,blocks",
        ])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("needs: CTX-0001"),
        "projected depends_on must appear in compact text: {stdout}"
    );
}

#[test]
fn task_show_under_projection_never_prints_empty_brackets() {
    let (dir, bin) = common::setup_test_project("show_projection_min");
    init_project(&dir, &bin);
    register_agent(&dir, &bin, "tester");
    create_task(&dir, &bin, "Minimal projection task");

    let out = Command::new(&bin)
        .args(["task", "show", "CTX-0001", "--fields", "display_id"])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.trim(), "CTX-0001");
    assert!(
        !stdout.contains("[]"),
        "no empty brackets for projected-out fields: {stdout}"
    );
}

#[test]
fn task_list_under_projection_has_no_padded_empty_columns() {
    let (dir, bin) = common::setup_test_project("list_projection_min");
    init_project(&dir, &bin);
    register_agent(&dir, &bin, "tester");
    create_task(&dir, &bin, "List projection task");

    let out = Command::new(&bin)
        .args(["task", "list", "--fields", "display_id"])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.trim(), "CTX-0001");
}

#[test]
fn task_show_output_fields_config_renders_depends_on() {
    let (dir, bin) = common::setup_test_project("show_config_projection");
    init_project(&dir, &bin);
    register_agent(&dir, &bin, "tester");
    setup_dependent_pair(&dir, &bin);

    std::fs::create_dir_all(dir.join(".carryctx")).unwrap();
    std::fs::write(
        dir.join(".carryctx").join("config.toml"),
        "[output.fields]\n\"task.show\" = [\"display_id\", \"title\", \"depends_on\"]\n",
    )
    .unwrap();

    let out = Command::new(&bin)
        .args(["task", "show", "CTX-0002"])
        .env("CARRYCTX_AGENT", "tester")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("needs: CTX-0001"),
        "[output.fields] must apply to compact text too: {stdout}"
    );
    assert!(
        !stdout.contains("[]"),
        "config projection must not emit empty brackets: {stdout}"
    );
}
