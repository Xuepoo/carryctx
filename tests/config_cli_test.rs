//! Regression tests for CTX-0070 issue #97: `config get/set/unset` TOML
//! handling. Dotted keys must land in the right table, values keep their
//! types, reads query the typed config, and scope flags are enforced.

mod common;

use serde_json::Value;

fn run(dir: &std::path::Path, bin: &std::path::Path, args: &[&str]) -> std::process::Output {
    common::run_cmd(dir, bin, args)
}

fn stdout_str(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_str(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn exit_code(output: &std::process::Output) -> i32 {
    output.status.code().unwrap_or(-1)
}

fn json_envelope(output: &std::process::Output) -> Value {
    serde_json::from_str(stdout_str(output).trim()).unwrap_or_else(|e| {
        panic!(
            "stdout must be a JSON envelope ({e}): {}",
            stdout_str(output)
        )
    })
}

fn project_with_config(name: &str, initial: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let (dir, bin) = common::setup_test_project(name);
    common::init_and_agent(&dir, &bin);
    let cfg_dir = dir.join(".carryctx");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(cfg_dir.join("config.toml"), initial).unwrap();
    (dir, bin)
}

fn read_project_config(dir: &std::path::Path) -> String {
    std::fs::read_to_string(dir.join(".carryctx").join("config.toml")).unwrap()
}

#[test]
fn set_dotted_key_under_existing_table_keeps_tables_intact() {
    // Regression: `config set task.strict_completion true` appended at EOF and
    // landed INSIDE the last `[verification]` table.
    let (dir, bin) = project_with_config(
        "cfg_set_dotted",
        "# managed header\n[verification]\ncommands = [\"cargo test\"]\n",
    );

    let out = run(
        &dir,
        &bin,
        &[
            "--json",
            "config",
            "set",
            "--cfg-project",
            "task.strict_completion",
            "true",
        ],
    );
    assert_eq!(
        exit_code(&out),
        0,
        "set must succeed; stderr={}",
        stderr_str(&out)
    );
    let envelope = json_envelope(&out);
    assert_eq!(envelope["data"]["value"], true, "typed preview in envelope");

    let file = read_project_config(&dir);
    assert!(
        file.contains("# managed header"),
        "comments preserved:\n{file}"
    );
    assert!(
        file.contains("commands = [\"cargo test\"]"),
        "existing table content preserved:\n{file}"
    );
    // strict_completion must not appear in the [verification] section body.
    let verification_body = file.split("[verification]").nth(1).expect("table present");
    let verification_body = match verification_body.split_once('[') {
        Some((before_next_table, _)) => before_next_table,
        None => verification_body,
    };
    assert!(
        !verification_body.contains("strict_completion"),
        "key must not leak into [verification]:\n{file}"
    );

    // The typed value is readable through the loader.
    let get = run(
        &dir,
        &bin,
        &["--json", "config", "get", "task.strict_completion"],
    );
    assert_eq!(exit_code(&get), 0);
    let envelope = json_envelope(&get);
    assert_eq!(envelope["data"]["value"], true, "bool round-trips typed");
}

#[test]
fn set_and_get_nested_keys_round_trip() {
    let (dir, bin) = project_with_config("cfg_roundtrip", "");

    // bool
    run(
        &dir,
        &bin,
        &[
            "--json",
            "config",
            "set",
            "--cfg-project",
            "context.include_git_status",
            "false",
        ],
    );
    // int
    run(
        &dir,
        &bin,
        &[
            "--json",
            "config",
            "set",
            "--cfg-project",
            "context.max_events",
            "250",
        ],
    );
    // string with spaces and quotes
    run(
        &dir,
        &bin,
        &[
            "--json",
            "config",
            "set",
            "--cfg-project",
            "project.name",
            "My \"Demo\" 项目 🚀",
        ],
    );

    for (key, expected) in [
        ("context.include_git_status", Value::Bool(false)),
        ("context.max_events", Value::Number(250.into())),
        (
            "project.name",
            Value::String("My \"Demo\" 项目 🚀".to_string()),
        ),
    ] {
        let get = run(&dir, &bin, &["--json", "config", "get", key]);
        assert_eq!(exit_code(&get), 0, "{key} get failed: {}", stderr_str(&get));
        let envelope = json_envelope(&get);
        assert_eq!(envelope["data"]["key"], key);
        assert_eq!(envelope["data"]["value"], expected, "{key} round-trip");
    }
}

#[test]
fn set_overwrites_scalar_in_place_without_duplicating() {
    let (dir, bin) = project_with_config("cfg_overwrite", "[context]\nmax_events = 100\n");
    run(
        &dir,
        &bin,
        &[
            "--json",
            "config",
            "set",
            "--cfg-project",
            "context.max_events",
            "7",
        ],
    );
    let file = read_project_config(&dir);
    assert!(!file.contains("100"), "old value replaced:\n{file}");
    assert_eq!(
        file.matches("max_events").count(),
        1,
        "no duplicates:\n{file}"
    );

    let get = run(
        &dir,
        &bin,
        &["--json", "config", "get", "context.max_events"],
    );
    assert_eq!(json_envelope(&get)["data"]["value"], 7);
}

#[test]
fn unset_removes_only_the_target_leaf() {
    let (dir, bin) = project_with_config(
        "cfg_unset",
        "[task]\nstrict_completion = true\nsingle_active_task_per_agent = false\n\n[verification]\ncommands = []\n",
    );

    let out = run(
        &dir,
        &bin,
        &[
            "--json",
            "config",
            "unset",
            "--cfg-project",
            "task.strict_completion",
        ],
    );
    assert_eq!(exit_code(&out), 0, "{}", stderr_str(&out));
    let file = read_project_config(&dir);
    assert!(!file.contains("strict_completion"), "leaf removed:\n{file}");
    assert!(
        file.contains("single_active_task_per_agent"),
        "sibling kept:\n{file}"
    );
    assert!(
        file.contains("[verification]"),
        "other tables kept:\n{file}"
    );

    // Unsetting an absent key succeeds without creating the file.
    let absent = run(
        &dir,
        &bin,
        &["--json", "config", "unset", "--global", "never.set.key"],
    );
    assert_eq!(exit_code(&absent), 0);
}

#[test]
fn get_queries_typed_config_not_raw_lines() {
    // Regression: line-prefix matching returned empty for nested keys and the
    // wrong hit on prefix collisions.
    let (dir, bin) = project_with_config(
        "cfg_get_typed",
        "[task]\nstrict_completion = false\n\n[git]\nmain_branch = \"trunk\"\n",
    );

    let get = run(
        &dir,
        &bin,
        &["--json", "config", "get", "task.strict_completion"],
    );
    assert_eq!(exit_code(&get), 0);
    assert_eq!(
        json_envelope(&get)["data"]["value"],
        false,
        "nested dotted key must resolve"
    );

    // Prefix collision: 'task' vs 'task_prefix' style names.
    let collision = run(&dir, &bin, &["--json", "config", "get", "proj"]);
    assert_eq!(exit_code(&collision), 0);
    assert_eq!(
        json_envelope(&collision)["data"]["value"],
        Value::Null,
        "unknown/prefix keys return null, not a wrong line"
    );

    // Whole-table lookup works too.
    let table = run(&dir, &bin, &["--json", "config", "get", "git"]);
    let value = json_envelope(&table)["data"]["value"].clone();
    assert_eq!(
        value["main_branch"], "trunk",
        "table keys resolve as objects"
    );
}

#[test]
fn scope_flags_are_enforced_with_clear_errors() {
    let (dir, bin) = project_with_config("cfg_scope", "");

    // --dry-run emits a stdout envelope with operation.applied=false and
    // leaves the file untouched.
    let before = read_project_config(&dir);
    let dry = run(
        &dir,
        &bin,
        &[
            "--json",
            "--dry-run",
            "config",
            "set",
            "--cfg-project",
            "context.max_events",
            "500",
        ],
    );
    assert_eq!(exit_code(&dry), 0);
    let envelope = json_envelope(&dry);
    assert_eq!(envelope["command"], "config.set");
    assert_eq!(envelope["data"]["operation"]["applied"], false);
    assert_eq!(
        read_project_config(&dir),
        before,
        "config set dry-run must not write the file"
    );

    // No scope: clear error instead of silent exit 2.
    let none = run(&dir, &bin, &["--json", "config", "set", "a.b", "c"]);
    assert_eq!(exit_code(&none), 2);
    let stderr = stderr_str(&none);
    assert!(
        stderr.contains("--global") && stderr.contains("--cfg-project"),
        "error must name the scopes: {stderr}"
    );

    let none_text = run(&dir, &bin, &["config", "set", "a.b", "c"]);
    assert_eq!(exit_code(&none_text), 2);
    assert!(
        !stderr_str(&none_text).is_empty(),
        "text mode gets a message too"
    );

    // Multiple scopes rejected as well.
    let both = run(
        &dir,
        &bin,
        &[
            "--json",
            "config",
            "set",
            "--global",
            "--cfg-project",
            "a.b",
            "c",
        ],
    );
    assert_eq!(exit_code(&both), 2);

    // --local is explicitly rejected because nothing reads local.toml yet.
    let local = run(
        &dir,
        &bin,
        &[
            "--json",
            "config",
            "set",
            "--local",
            "agent.default_name",
            "x",
        ],
    );
    assert_eq!(exit_code(&local), 10, "rejected as unsupported");
    assert!(
        stderr_str(&local).contains("local.toml"),
        "rejection explains why: {}",
        stderr_str(&local)
    );
    assert!(
        !dir.join(".carryctx").join("local.toml").exists(),
        "no orphan local.toml may be written"
    );

    let unset_local = run(&dir, &bin, &["--json", "config", "unset", "--local", "a.b"]);
    assert_eq!(exit_code(&unset_local), 10);
}

#[test]
fn unreadable_files_surface_as_errors_not_silent_empty() {
    let (dir, bin) = common::setup_test_project("cfg_unreadable");
    common::init_and_agent(&dir, &bin);

    // Replace the scaffolded config file with a directory: read_to_string on
    // a directory fails even when running as root, unlike a chmod-000 file.
    let cfg = dir.join(".carryctx").join("config.toml");
    std::fs::remove_file(&cfg).unwrap();
    std::fs::create_dir_all(&cfg).unwrap();

    let list = run(&dir, &bin, &["--json", "config", "list"]);
    assert_ne!(exit_code(&list), 0, "unreadable file must fail loudly");
    assert!(
        !stderr_str(&list).is_empty(),
        "failure must carry a message"
    );
}
