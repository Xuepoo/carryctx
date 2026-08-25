//! Regression tests for CTX-0070: reachable panics (#94), --dry-run contract
//! violations (#95), and output-envelope violations (#96).
//!
//! Each test drives the real binary in a disposable Git repository.

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

/// Parse the success envelope emitted on stdout and assert its shape.
fn assert_success_envelope(output: &std::process::Output, command: &str) -> Value {
    let stdout = stdout_str(output);
    let parsed: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout must be a valid JSON envelope ({e}): {stdout}"));
    assert_eq!(
        parsed["success"], true,
        "envelope must be successful: {parsed}"
    );
    assert_eq!(parsed["command"], command, "envelope command label");
    parsed
}

/// Parse the error envelope emitted on stderr and return it.
fn assert_error_envelope(output: &std::process::Output, command: &str) -> Value {
    let stderr = stderr_str(output);
    let last_line = stderr
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .unwrap_or_else(|| panic!("stderr must contain a JSON envelope: {stderr}"));
    let parsed: Value = serde_json::from_str(last_line.trim())
        .unwrap_or_else(|e| panic!("stderr must be valid JSON ({e}): {stderr}"));
    assert_eq!(
        parsed["success"], false,
        "must be an error envelope: {parsed}"
    );
    assert_eq!(parsed["command"], command, "envelope command label");
    parsed
}

// ── #94: reachable panics ────────────────────────────────────────────────

#[test]
fn dry_run_json_task_list_renders_normally_instead_of_panicking() {
    let (dir, bin) = common::setup_test_project("dryrun_task_list");
    common::init_and_agent(&dir, &bin);
    run(&dir, &bin, &["task", "create", "--title", "中文标题任务"]);

    // Regression: `--json --dry-run task list` hit `unreachable!()` (SIGABRT,
    // exit 134). Non-mutating subcommands must render normally.
    let output = run(&dir, &bin, &["--json", "--dry-run", "task", "list"]);
    assert_eq!(
        exit_code(&output),
        0,
        "exit must be 0, got {} (stderr: {})",
        exit_code(&output),
        stderr_str(&output)
    );
    let envelope = assert_success_envelope(&output, "task.list");
    let tasks = envelope["data"].as_array().expect("task list data array");
    assert!(
        tasks.iter().any(|t| t["title"] == "中文标题任务"),
        "list contents must render under dry-run"
    );

    // task show is equally non-mutating.
    let show = run(
        &dir,
        &bin,
        &[
            "--json",
            "--dry-run",
            "task",
            "show",
            tasks[0]["display_id"].as_str().unwrap(),
        ],
    );
    assert_eq!(exit_code(&show), 0, "task show must render under dry-run");
}

#[test]
fn markdown_renderers_survive_multibyte_content() {
    let (dir, bin) = common::setup_test_project("markdown_multibyte");
    common::init_and_agent(&dir, &bin);

    let created = run(
        &dir,
        &bin,
        &["--json", "task", "create", "--title", "中文任务"],
    );
    let envelope: Value =
        serde_json::from_str(stdout_str(&created).trim()).expect("create envelope");
    let display_id = envelope["data"]["display_id"]
        .as_str()
        .expect("display_id")
        .to_string();

    // Progress content that crosses the 40-byte boundary mid-character.
    run(
        &dir,
        &bin,
        &[
            "progress",
            "note",
            "这是一条非常长的中文进度内容用来触发字节截断边界崩溃测试内容继续加长",
            "--task",
            &display_id,
        ],
    );

    // Decision title crossing the same boundary.
    run(
        &dir,
        &bin,
        &[
            "decision",
            "add",
            "--title",
            "这是一个非常长的中文决策标题用于触发字符边界截断回归测试补充长度",
        ],
    );

    // A session so session-list has rows with agent ids.
    run(&dir, &bin, &["session", "start", "--agent", "tester"]);

    for args in [
        vec![
            "--format",
            "markdown",
            "progress",
            "list",
            "--task",
            &display_id,
        ],
        vec!["--format", "markdown", "decision", "list"],
        vec!["--format", "markdown", "session", "list"],
        vec!["--format", "markdown", "event", "list"],
        vec!["--format", "markdown", "task", "list"],
    ] {
        let output = run(&dir, &bin, &args);
        assert_eq!(
            exit_code(&output),
            0,
            "{args:?} must not abort; stderr={}",
            stderr_str(&output)
        );
        let out = stdout_str(&output);
        assert!(
            out.starts_with('#'),
            "{args:?} must render a markdown table"
        );
        assert!(
            !out.contains("Error:"),
            "{args:?} must not print an error document: {out}"
        );
    }
}

#[test]
fn markdown_truncation_clips_on_char_boundaries() {
    let (dir, bin) = common::setup_test_project("markdown_boundary");
    common::init_and_agent(&dir, &bin);

    let created = run(&dir, &bin, &["--json", "task", "create", "--title", "t"]);
    let envelope: Value =
        serde_json::from_str(stdout_str(&created).trim()).expect("create envelope");
    let display_id = envelope["data"]["display_id"].as_str().unwrap().to_string();

    run(
        &dir,
        &bin,
        &[
            "progress",
            "note",
            "🚀🚀🚀🚀🚀🚀🚀🚀🚀🚀🚀🚀🚀🚀🚀🚀🚀🚀🚀🚀",
            "--task",
            &display_id,
        ],
    );
    let output = run(
        &dir,
        &bin,
        &[
            "--format",
            "markdown",
            "progress",
            "list",
            "--task",
            &display_id,
        ],
    );
    assert_eq!(exit_code(&output), 0, "emoji content must not abort");
}

// ── #95: --dry-run contract ──────────────────────────────────────────────

#[test]
fn dry_run_json_mutating_commands_emit_stdout_envelopes() {
    let (dir, bin) = common::setup_test_project("dryrun_envelopes");
    common::init_and_agent(&dir, &bin);
    // Snapshot the scaffolded project config so we can prove no dry-run
    // mutated it (init writes a full default config.toml).
    let config_path = dir.join(".carryctx").join("config.toml");
    let config_before = std::fs::read_to_string(&config_path).unwrap();
    let created = run(&dir, &bin, &["--json", "task", "create", "--title", "t1"]);
    let envelope: Value =
        serde_json::from_str(stdout_str(&created).trim()).expect("create envelope");
    let display_id = envelope["data"]["display_id"].as_str().unwrap().to_string();

    let cases: Vec<(Vec<&str>, &str)> = vec![
        (
            vec![
                "--json",
                "--dry-run",
                "worktree",
                "create",
                &display_id,
                "--path",
                "wt-dryrun",
            ],
            "worktree.create",
        ),
        (
            vec![
                "--json",
                "--dry-run",
                "session",
                "start",
                "--agent",
                "tester",
            ],
            "session.start",
        ),
        (
            vec![
                "--json",
                "--dry-run",
                "checkpoint",
                "--note",
                "n",
                "--no-git",
            ],
            "checkpoint.create",
        ),
        (
            vec!["--json", "--dry-run", "decision", "add", "--title", "d1"],
            "decision.add",
        ),
        (
            vec![
                "--json",
                "--dry-run",
                "progress",
                "todo",
                "do things",
                "--task",
                &display_id,
            ],
            "progress.todo",
        ),
    ];

    for (args, command) in &cases {
        let output = run(&dir, &bin, args);
        assert_eq!(
            exit_code(&output),
            0,
            "{command} dry-run must succeed; stderr={}",
            stderr_str(&output)
        );
        let envelope = assert_success_envelope(&output, command);
        assert_eq!(
            envelope["data"]["operation"]["applied"], false,
            "{command} dry-run envelope must carry operation.applied=false"
        );
        assert!(
            stderr_str(&output).contains("[dry-run]"),
            "{command} text note must still appear on stderr"
        );
    }

    // The dry-runs above must not have mutated anything.
    assert!(
        !dir.join("wt-dryrun").exists(),
        "worktree create dry-run must not create directories"
    );
    let config_after = std::fs::read_to_string(&config_path).unwrap();
    assert_eq!(
        config_before, config_after,
        "config set dry-run must not write the file"
    );
}

#[test]
fn dry_run_graph_add_node_writes_nothing() {
    let (dir, bin) = common::setup_test_project("dryrun_graph");
    common::init_and_agent(&dir, &bin);

    let probe = format!("probe-node-{}", unique_suffix());
    let output = run(
        &dir,
        &bin,
        &[
            "--json",
            "--dry-run",
            "graph",
            "add-node",
            "--node-type",
            "file",
            "--name",
            &probe,
        ],
    );
    assert_eq!(exit_code(&output), 0, "dry-run add-node must succeed");
    let envelope = assert_success_envelope(&output, "graph.add-node");
    assert_eq!(envelope["data"]["operation"]["applied"], false);

    let export = run(&dir, &bin, &["graph", "export", "--type", "mermaid"]);
    let exported = stdout_str(&export);
    assert!(
        !exported.contains(&probe),
        "node must not be written during dry-run: {exported}"
    );

    // Control: a real add-node IS persisted and visible.
    run(
        &dir,
        &bin,
        &["graph", "add-node", "--node-type", "file", "--name", &probe],
    );
    let export = run(&dir, &bin, &["graph", "export", "--type", "mermaid"]);
    assert!(
        stdout_str(&export).contains(&probe),
        "control: real add-node must persist"
    );
}

#[test]
fn graph_dry_run_gates_link_and_scan_but_not_export() {
    let (dir, bin) = common::setup_test_project("dryrun_graph_more");
    common::init_and_agent(&dir, &bin);

    let link = run(
        &dir,
        &bin,
        &["--dry-run", "graph", "link", "a", "b", "uses"],
    );
    assert_eq!(exit_code(&link), 0);
    assert!(stderr_str(&link).contains("[dry-run]"));
    assert!(
        stdout_str(&link).is_empty(),
        "text mode prints nothing on stdout"
    );

    let scan = run(&dir, &bin, &["--dry-run", "graph", "scan", "--dir", "."]);
    assert_eq!(exit_code(&scan), 0);
    assert!(stderr_str(&scan).contains("[dry-run]"));

    // Read-only subcommands still render normally. Mermaid renders in-process
    // (ascii export shells out to the optional external `mermaid-ascii` tool,
    // which CI runners do not provide).
    let export = run(
        &dir,
        &bin,
        &["--dry-run", "graph", "export", "--type", "mermaid"],
    );
    assert_eq!(exit_code(&export), 0);
    assert!(
        stderr_str(&export).is_empty(),
        "non-mutating arms are not gated"
    );
}

/// CTX-0083 / issue #106 item 5: under `--format json --dry-run`, every
/// mutating graph subcommand prints the standard success envelope with
/// `operation.applied = false` on stdout (mirroring task/handoff dry-runs)
/// while keeping the `[dry-run]` note on stderr in text mode.
#[test]
fn graph_json_dry_run_emits_envelope_for_every_mutating_op() {
    let (dir, bin) = common::setup_test_project("graph_json_dryrun");
    common::init_and_agent(&dir, &bin);

    let cases: Vec<(Vec<&str>, &str)> = vec![
        (
            vec![
                "--format",
                "json",
                "--dry-run",
                "graph",
                "add-node",
                "--node-type",
                "file",
                "--name",
                "src/probe.ts",
            ],
            "graph.add-node",
        ),
        (
            vec![
                "--format",
                "json",
                "--dry-run",
                "graph",
                "link",
                "01ABCDEF",
                "01FEDCBA",
                "imports",
            ],
            "graph.link",
        ),
        (
            vec![
                "--format",
                "json",
                "--dry-run",
                "graph",
                "extract-deps",
                "src/probe.ts",
            ],
            "graph.extract-deps",
        ),
        (
            vec!["--format", "json", "--dry-run", "graph", "scan"],
            "graph.scan",
        ),
    ];

    for (args, command) in &cases {
        let output = run(&dir, &bin, args);
        assert_eq!(
            exit_code(&output),
            0,
            "{command} json dry-run must succeed; stderr={}",
            stderr_str(&output)
        );
        let envelope = assert_success_envelope(&output, command);
        assert_eq!(
            envelope["data"]["operation"]["applied"], false,
            "{command} json dry-run must report applied=false"
        );
        assert!(
            stderr_str(&output).contains("[dry-run]"),
            "{command} keeps the stderr note"
        );
    }

    // Nothing was written: the graph is still empty.
    let export = run(
        &dir,
        &bin,
        &["--format", "json", "graph", "export", "-t", "json"],
    );
    let exported: Value =
        serde_json::from_str(stdout_str(&export).trim()).expect("valid export envelope");
    let content = exported["data"]["content"].as_str().unwrap_or_default();
    assert!(
        !content.contains("probe"),
        "json dry-runs above must not write any nodes: {content}"
    );
}

#[test]
fn dry_run_json_team_error_produces_error_envelope() {
    let (dir, bin) = common::setup_test_project("dryrun_team_err");
    common::init_and_agent(&dir, &bin);

    // Regression: `.map_err(|e| e.exit_code)?` exited 7 with empty stdout AND
    // empty stderr.
    let output = run(
        &dir,
        &bin,
        &[
            "--json",
            "--dry-run",
            "team",
            "member",
            "add",
            "missing-team",
            "--agent",
            "alice",
        ],
    );
    assert_eq!(exit_code(&output), 7);
    let envelope = assert_error_envelope(&output, "team.member_add");
    assert_eq!(envelope["error"]["code"], "RESOURCE_NOT_FOUND");
}

#[test]
fn dry_run_json_task_team_set_error_produces_error_envelope() {
    let (dir, bin) = common::setup_test_project("dryrun_task_team_err");
    common::init_and_agent(&dir, &bin);

    let output = run(
        &dir,
        &bin,
        &[
            "--json",
            "--dry-run",
            "task",
            "team",
            "set",
            "TASK-DOES-NOT-EXIST",
            "--team",
            "some-team",
        ],
    );
    assert_eq!(exit_code(&output), 7);
    let envelope = assert_error_envelope(&output, "task.team_set");
    assert_eq!(envelope["error"]["code"], "RESOURCE_NOT_FOUND");

    let unset = run(
        &dir,
        &bin,
        &[
            "--json",
            "--dry-run",
            "task",
            "team",
            "unset",
            "ALSO-MISSING",
        ],
    );
    assert_eq!(exit_code(&unset), 7);
    assert_error_envelope(&unset, "task.team_unset");
}

// ── #96: output envelopes ────────────────────────────────────────────────

#[test]
fn checkpoint_show_missing_renders_resource_not_found_envelope() {
    let (dir, bin) = common::setup_test_project("checkpoint_show_missing");
    common::init_and_agent(&dir, &bin);

    let text = run(
        &dir,
        &bin,
        &["checkpoint", "show", "01NOSUCHCHECKPOINT000000"],
    );
    assert_eq!(exit_code(&text), 7);
    let err = stderr_str(&text);
    assert!(
        err.contains("not found") || err.contains("RESOURCE_NOT_FOUND"),
        "text mode must explain the failure: {err}"
    );
    assert!(
        !stdout_str(&text).contains("Error"),
        "error text must not land on stdout"
    );

    let json = run(
        &dir,
        &bin,
        &["--json", "checkpoint", "show", "01NOSUCHCHECKPOINT000000"],
    );
    assert_eq!(exit_code(&json), 7);
    let envelope = assert_error_envelope(&json, "checkpoint.show");
    assert_eq!(envelope["error"]["code"], "RESOURCE_NOT_FOUND");
}

#[test]
fn project_register_unregister_fail_honestly() {
    let (dir, bin) = common::setup_test_project("project_register_honest");
    common::init_and_agent(&dir, &bin);

    let cases: Vec<(Vec<&str>, &str)> = vec![
        (
            vec!["project", "register", "/tmp/somewhere"],
            "project.register",
        ),
        (
            vec!["project", "unregister", "SOMEPROJECT"],
            "project.unregister",
        ),
    ];

    for (args, command) in &cases {
        let text = run(&dir, &bin, args);
        assert_ne!(exit_code(&text), 0, "{command} must not fake success");
        let out = stdout_str(&text);
        assert!(
            !out.contains("\"status\""),
            "{command} must not emit a fabricated success payload"
        );
        assert!(
            !stderr_str(&text).is_empty(),
            "{command} must explain itself on stderr"
        );

        let mut json_args = vec!["--json"];
        json_args.extend_from_slice(args);
        let full = run(&dir, &bin, &json_args);
        assert_eq!(
            exit_code(&full),
            10,
            "{command} must exit UNSUPPORTED(10); stdout={}",
            stdout_str(&full)
        );
        let envelope = assert_error_envelope(&full, command);
        assert_eq!(envelope["error"]["code"], "UNSUPPORTED_OPERATION");
    }
}

#[test]
fn preset_list_json_serializes_lockfile_presets() {
    let (dir, bin) = common::setup_test_project("preset_list_json");
    common::init_and_agent(&dir, &bin);

    std::fs::create_dir_all(dir.join(".carryctx")).unwrap();
    std::fs::write(
        dir.join(".carryctx").join("presets.lock"),
        "version = 1\n\n[presets.carryctx-core]\nversion = \"0.5.8\"\nsource = \"packs/carryctx-core\"\nintegrity = \"sha256-deadbeef\"\n[presets.carryctx-core.permissions_granted]\nrequires_filesystem = true\nrequires_network = false\nrequires_env = []\n\n[presets.zeta-pack]\nversion = \"1.1.0\"\nsource = \"local/zeta\"\nintegrity = \"sha256-cafebabe\"\n[presets.zeta-pack.permissions_granted]\nrequires_filesystem = false\nrequires_network = true\nrequires_env = [\"KEY\"]\n",
    )
    .unwrap();

    let output = run(&dir, &bin, &["--json", "preset", "list"]);
    assert_eq!(exit_code(&output), 0);
    let envelope = assert_success_envelope(&output, "preset.list");
    let presets = envelope["data"]["presets"]
        .as_array()
        .expect("presets array");
    assert_eq!(presets.len(), 2, "lockfile presets must be serialized");
    assert_eq!(presets[0]["name"], "carryctx-core", "sorted by name");
    assert_eq!(presets[1]["name"], "zeta-pack");
    assert_eq!(presets[0]["version"], "0.5.8");
    assert_eq!(presets[0]["permissionsGranted"]["filesystem"], true);

    // Text mode keeps working.
    let text = run(&dir, &bin, &["preset", "list"]);
    let out = stdout_str(&text);
    assert!(
        out.contains("carryctx-core"),
        "text list shows presets: {out}"
    );
}

#[test]
fn preset_show_emits_valid_escaped_json() {
    let (dir, bin) = common::setup_test_project("preset_show_escape");
    common::init_and_agent(&dir, &bin);

    std::fs::create_dir_all(dir.join(".carryctx")).unwrap();
    std::fs::write(
        dir.join(".carryctx").join("tricky.md"),
        "# Title with \"quotes\"\n\nLine\twith special 中文 content\n",
    )
    .unwrap();

    let output = run(&dir, &bin, &["--json", "preset", "show", "tricky.md"]);
    assert_eq!(exit_code(&output), 0, "{}", stderr_str(&output));
    let envelope = assert_success_envelope(&output, "preset.show");
    let content = envelope["data"]["content"]
        .as_str()
        .expect("content string");
    assert!(
        content.contains(r#""quotes""#) && content.contains("中文"),
        "content must round-trip unescaped through JSON parsing"
    );

    // Missing presets produce RESOURCE_NOT_FOUND instead of a bare message.
    let missing = run(&dir, &bin, &["--json", "preset", "show", "absent-pack"]);
    assert_eq!(exit_code(&missing), 7);
    let envelope = assert_error_envelope(&missing, "preset.show");
    assert_eq!(envelope["error"]["code"], "RESOURCE_NOT_FOUND");
}

#[test]
fn outside_git_repo_failure_prints_message_in_both_modes() {
    let unique = std::env::temp_dir().join(format!("carryctx_nogit_{}", unique_suffix()));
    std::fs::create_dir_all(&unique).unwrap();
    let bin = common::test_binary();

    // Text mode: previously silent with exit 4.
    let text = common::run_cmd(&unique, &bin, &["task", "list"]);
    assert_eq!(exit_code(&text), 4, "git discovery failure exits GIT(4)");
    let err = stderr_str(&text);
    assert!(
        err.contains("Error") && (err.contains("git") || err.contains("Git")),
        "text mode must surface the failure: {err}"
    );

    // JSON mode: standard error envelope on stderr.
    let json = common::run_cmd(&unique, &bin, &["--json", "task", "list"]);
    assert_eq!(exit_code(&json), 4);
    let envelope = assert_error_envelope(&json, "runtime.open");
    assert_eq!(envelope["error"]["code"], "GIT_ERROR");

    let _ = std::fs::remove_dir_all(&unique);
}

// ── helpers ──────────────────────────────────────────────────────────────

fn unique_suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    format!("{nanos:x}")
}
