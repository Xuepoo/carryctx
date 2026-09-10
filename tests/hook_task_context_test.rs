//! CTX-0150 / issue #149: the `hooks dispatch` prepare-commit-msg prefix and
//! the post-commit checkpoint must be derived from the commit's own context
//! (task-bound branch name, commit-subject prefix, explicit `CARRYCTX_TASK`)
//! — never from a stale/ambient active session belonging to another task.
//!
//! Precedence under test: branch/commit binding > explicit env > worktree >
//! the only task-carrying active session. Multiple active sessions are
//! ambiguous and must not tag at all; an unknown branch binding must not
//! fall through to ambient state.

mod common;

use std::path::Path;
use std::process::{Command, Output};

fn task_create(dir: &Path, bin: &Path, title: &str) -> String {
    let out = common::run_cmd(dir, bin, &["task", "create", "--title", title, "--json"]);
    assert!(
        out.status.success(),
        "task create failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    value["data"]["display_id"].as_str().unwrap().to_string()
}

fn register_agent(dir: &Path, bin: &Path, name: &str) {
    let out = common::run_cmd(
        dir,
        bin,
        &["agent", "register", "--name", name, "--provider", "test"],
    );
    assert!(
        out.status.success(),
        "agent register failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn start_session(dir: &Path, bin: &Path, agent: &str, task: &str) {
    let out = common::run_cmd_as(
        dir,
        bin,
        agent,
        &["session", "start", "--task", task, "--json"],
    );
    assert!(
        out.status.success(),
        "session start failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn checkout_branch(dir: &Path, display_id: &str) {
    let branch = format!("{}/feat-x", display_id.to_lowercase());
    common::fixture_git(dir, &["checkout", "-q", "-b", &branch]);
}

fn commit(dir: &Path, subject: &str) {
    common::fixture_git(dir, &["commit", "--allow-empty", "-q", "-m", subject]);
}

/// Run `hooks dispatch` as `tester`, optionally with `CARRYCTX_TASK` set.
fn dispatch(dir: &Path, bin: &Path, event: &str, args: &[&str], env_task: Option<&str>) -> Output {
    let mut cmd = Command::new(bin);
    // `--json` is passed as a global flag before the subcommand: after the
    // dispatch event it would be swallowed by `trailing_var_arg`.
    cmd.env("CARRYCTX_AGENT", "tester")
        .env_remove("CARRYCTX_TASK")
        .current_dir(dir)
        .args(["--json", "hooks", "dispatch", event])
        .args(args);
    if let Some(task) = env_task {
        cmd.env("CARRYCTX_TASK", task);
    }
    cmd.output().expect("hook dispatch should run")
}

fn write_msg(dir: &Path) -> std::path::PathBuf {
    let msg = dir.join("COMMIT_EDITMSG");
    std::fs::write(&msg, "feat: add thing\n").unwrap();
    msg
}

fn envelope(out: &Output) -> serde_json::Value {
    assert!(
        out.status.success(),
        "hook dispatch failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("hook dispatch must emit a JSON envelope")
}

fn checkpoints_for_task(dir: &Path, bin: &Path, task: &str) -> Vec<serde_json::Value> {
    let out = common::run_cmd(dir, bin, &["--task", task, "checkpoint", "list", "--json"]);
    assert!(
        out.status.success(),
        "checkpoint list failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    value["data"]
        .as_array()
        .expect("checkpoint list data")
        .clone()
}

// ── prepare-commit-msg ────────────────────────────────────────────────────

#[test]
fn prepare_commit_msg_branch_task_beats_ambient_active_session() {
    let (dir, bin) = common::setup_test_project("hook_prefix_branch_vs_ambient");
    common::init_and_agent(&dir, &bin);
    let branch_task = task_create(&dir, &bin, "branch task");
    let ambient_task = task_create(&dir, &bin, "ambient task");
    register_agent(&dir, &bin, "other");
    start_session(&dir, &bin, "other", &ambient_task);
    checkout_branch(&dir, &branch_task);

    let msg = write_msg(&dir);
    let out = dispatch(
        &dir,
        &bin,
        "git.prepare-commit-msg",
        &[msg.to_str().unwrap()],
        None,
    );

    let value = envelope(&out);
    assert_eq!(
        value["data"]["task_id"].as_str(),
        Some(branch_task.as_str()),
        "the task-bound branch name is authoritative: {value}"
    );
    assert_eq!(
        std::fs::read_to_string(&msg).unwrap(),
        format!("[{branch_task}] feat: add thing\n")
    );
}

#[test]
fn prepare_commit_msg_branch_task_beats_explicit_env_task() {
    let (dir, bin) = common::setup_test_project("hook_prefix_branch_vs_env");
    common::init_and_agent(&dir, &bin);
    let branch_task = task_create(&dir, &bin, "branch task");
    let env_task = task_create(&dir, &bin, "env task");
    checkout_branch(&dir, &branch_task);

    let msg = write_msg(&dir);
    let out = dispatch(
        &dir,
        &bin,
        "git.prepare-commit-msg",
        &[msg.to_str().unwrap()],
        Some(&env_task),
    );

    let value = envelope(&out);
    assert_eq!(
        value["data"]["task_id"].as_str(),
        Some(branch_task.as_str()),
        "branch binding outranks the explicit env task: {value}"
    );
    assert_eq!(
        std::fs::read_to_string(&msg).unwrap(),
        format!("[{branch_task}] feat: add thing\n")
    );
}

#[test]
fn prepare_commit_msg_explicit_env_task_used_without_branch_binding() {
    let (dir, bin) = common::setup_test_project("hook_prefix_env_only");
    common::init_and_agent(&dir, &bin);
    let env_task = task_create(&dir, &bin, "env task");

    let msg = write_msg(&dir);
    let out = dispatch(
        &dir,
        &bin,
        "git.prepare-commit-msg",
        &[msg.to_str().unwrap()],
        Some(&env_task),
    );

    let value = envelope(&out);
    assert_eq!(value["data"]["task_id"].as_str(), Some(env_task.as_str()));
    assert_eq!(
        std::fs::read_to_string(&msg).unwrap(),
        format!("[{env_task}] feat: add thing\n")
    );
}

#[test]
fn prepare_commit_msg_ambiguous_active_sessions_do_not_tag() {
    let (dir, bin) = common::setup_test_project("hook_prefix_ambiguous_sessions");
    common::init_and_agent(&dir, &bin);
    let first = task_create(&dir, &bin, "first task");
    let second = task_create(&dir, &bin, "second task");
    register_agent(&dir, &bin, "other");
    start_session(&dir, &bin, "tester", &first);
    start_session(&dir, &bin, "other", &second);

    let msg = write_msg(&dir);
    let out = dispatch(
        &dir,
        &bin,
        "git.prepare-commit-msg",
        &[msg.to_str().unwrap()],
        None,
    );

    let value = envelope(&out);
    assert_eq!(
        value["data"]["dispatched"].as_bool(),
        Some(false),
        "ambiguous active sessions must not tag: {value}"
    );
    assert_eq!(
        std::fs::read_to_string(&msg).unwrap(),
        "feat: add thing\n",
        "the message must be left untouched"
    );
}

#[test]
fn prepare_commit_msg_single_active_session_still_tags() {
    let (dir, bin) = common::setup_test_project("hook_prefix_single_session");
    common::init_and_agent(&dir, &bin);
    let task = task_create(&dir, &bin, "only task");
    start_session(&dir, &bin, "tester", &task);

    let msg = write_msg(&dir);
    let out = dispatch(
        &dir,
        &bin,
        "git.prepare-commit-msg",
        &[msg.to_str().unwrap()],
        None,
    );

    let value = envelope(&out);
    assert_eq!(value["data"]["task_id"].as_str(), Some(task.as_str()));
    assert_eq!(
        std::fs::read_to_string(&msg).unwrap(),
        format!("[{task}] feat: add thing\n")
    );
}

#[test]
fn prepare_commit_msg_unknown_branch_task_never_falls_back_to_ambient() {
    let (dir, bin) = common::setup_test_project("hook_prefix_unknown_branch");
    common::init_and_agent(&dir, &bin);
    let ambient_task = task_create(&dir, &bin, "ambient task");
    register_agent(&dir, &bin, "other");
    start_session(&dir, &bin, "other", &ambient_task);
    // Branch names a task that does not exist in this project database.
    common::fixture_git(&dir, &["checkout", "-q", "-b", "ctx-9999/feat-x"]);

    let msg = write_msg(&dir);
    let out = dispatch(
        &dir,
        &bin,
        "git.prepare-commit-msg",
        &[msg.to_str().unwrap()],
        None,
    );

    let value = envelope(&out);
    assert_eq!(
        value["data"]["dispatched"].as_bool(),
        Some(false),
        "an unknown branch binding must not fall through to ambient state: {value}"
    );
    assert_eq!(
        std::fs::read_to_string(&msg).unwrap(),
        "feat: add thing\n",
        "no prefix may be invented from ambient state"
    );
}

// ── post-commit ───────────────────────────────────────────────────────────

#[test]
fn post_commit_checkpoints_commit_task_not_ambient_session() {
    let (dir, bin) = common::setup_test_project("hook_checkpoint_commit_task");
    common::init_and_agent(&dir, &bin);
    let commit_task = task_create(&dir, &bin, "commit task");
    let ambient_task = task_create(&dir, &bin, "ambient task");
    register_agent(&dir, &bin, "other");
    start_session(&dir, &bin, "other", &ambient_task);
    // The commit itself carries the task prefix, but the ambient session
    // belongs to a different task.
    commit(&dir, &format!("[{commit_task}] feat: add thing"));

    let out = dispatch(&dir, &bin, "git.post-commit", &[], None);

    let value = envelope(&out);
    assert_eq!(
        value["data"]["task_id"].as_str(),
        Some(commit_task.as_str()),
        "the checkpoint must belong to the commit's task: {value}"
    );
    assert!(
        !checkpoints_for_task(&dir, &bin, &commit_task).is_empty(),
        "the commit's task must receive the checkpoint"
    );
    assert!(
        checkpoints_for_task(&dir, &bin, &ambient_task).is_empty(),
        "the ambient task must not receive a spurious checkpoint"
    );
}

#[test]
fn post_commit_checkpoints_branch_task_when_subject_unprefixed() {
    let (dir, bin) = common::setup_test_project("hook_checkpoint_branch_task");
    common::init_and_agent(&dir, &bin);
    let branch_task = task_create(&dir, &bin, "branch task");
    let ambient_task = task_create(&dir, &bin, "ambient task");
    register_agent(&dir, &bin, "other");
    start_session(&dir, &bin, "other", &ambient_task);
    checkout_branch(&dir, &branch_task);
    commit(&dir, "feat: add thing");

    let out = dispatch(&dir, &bin, "git.post-commit", &[], None);

    let value = envelope(&out);
    assert_eq!(
        value["data"]["task_id"].as_str(),
        Some(branch_task.as_str()),
        "the task-bound branch name guards the checkpoint: {value}"
    );
    assert!(
        checkpoints_for_task(&dir, &bin, &ambient_task).is_empty(),
        "the ambient task must not receive a spurious checkpoint"
    );
}

#[test]
fn post_commit_no_checkpoint_when_task_context_ambiguous() {
    let (dir, bin) = common::setup_test_project("hook_checkpoint_ambiguous");
    common::init_and_agent(&dir, &bin);
    let first = task_create(&dir, &bin, "first task");
    let second = task_create(&dir, &bin, "second task");
    register_agent(&dir, &bin, "other");
    start_session(&dir, &bin, "tester", &first);
    start_session(&dir, &bin, "other", &second);
    commit(&dir, "feat: add thing");

    let out = dispatch(&dir, &bin, "git.post-commit", &[], None);

    let value = envelope(&out);
    assert_eq!(
        value["data"]["dispatched"].as_bool(),
        Some(false),
        "ambiguous active sessions must not create a checkpoint: {value}"
    );
    assert!(
        checkpoints_for_task(&dir, &bin, &first).is_empty()
            && checkpoints_for_task(&dir, &bin, &second).is_empty(),
        "no task may receive a spurious checkpoint"
    );
}

#[test]
fn post_commit_unknown_commit_task_never_falls_back_to_ambient() {
    let (dir, bin) = common::setup_test_project("hook_checkpoint_unknown_task");
    common::init_and_agent(&dir, &bin);
    let ambient_task = task_create(&dir, &bin, "ambient task");
    register_agent(&dir, &bin, "other");
    start_session(&dir, &bin, "other", &ambient_task);
    commit(&dir, "[CTX-9999] feat: add thing");

    let out = dispatch(&dir, &bin, "git.post-commit", &[], None);

    let value = envelope(&out);
    assert_eq!(
        value["data"]["dispatched"].as_bool(),
        Some(false),
        "an unknown commit task must not fall through to ambient state: {value}"
    );
    assert!(
        checkpoints_for_task(&dir, &bin, &ambient_task).is_empty(),
        "the ambient task must not receive a spurious checkpoint"
    );
}

#[test]
fn post_commit_single_active_session_still_checkpoints() {
    let (dir, bin) = common::setup_test_project("hook_checkpoint_single_session");
    common::init_and_agent(&dir, &bin);
    let task = task_create(&dir, &bin, "only task");
    start_session(&dir, &bin, "tester", &task);
    commit(&dir, "feat: add thing");

    let out = dispatch(&dir, &bin, "git.post-commit", &[], None);

    let value = envelope(&out);
    assert_eq!(value["data"]["task_id"].as_str(), Some(task.as_str()));
    assert!(
        !checkpoints_for_task(&dir, &bin, &task).is_empty(),
        "the single unambiguous active session still checkpoints"
    );
}
