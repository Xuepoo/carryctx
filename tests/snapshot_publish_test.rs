#![cfg(unix)]
mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

const SECRET: &str = "sk-live-SECRETVALUE0123456789";

fn script_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/publish-snapshot.sh")
}

fn scrub(command: &mut Command) {
    for var in common::GIT_STATE_VARS {
        command.env_remove(var);
    }
}

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    let mut command = Command::new("git");
    command.args(args).current_dir(dir);
    scrub(&mut command);
    command.output().expect("git should run")
}

fn git_ok(dir: &Path, args: &[&str]) -> String {
    let output = git(dir, args);
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn script(dir: &Path, bin: &Path, extra: &[&str]) -> std::process::Output {
    let mut command = Command::new("bash");
    command
        .arg(script_path())
        .args(extra)
        .current_dir(dir)
        .env("CARRYCTX_BIN", bin)
        .env("GIT_TIMEOUT", "60");
    scrub(&mut command);
    command.output().expect("publish script should run")
}

fn json(output: &std::process::Output) -> serde_json::Value {
    let stream = if output.stdout.is_empty() {
        &output.stderr
    } else {
        &output.stdout
    };
    serde_json::from_slice(stream).unwrap_or_else(|error| {
        panic!(
            "expected JSON output, got {error}: {}",
            String::from_utf8_lossy(stream)
        )
    })
}

fn seed_task_with_secret(dir: &Path, bin: &Path) {
    common::init_and_agent(dir, bin);
    let created = common::run_cmd(dir, bin, &["task", "create", "--title", "snapshot task"]);
    assert!(created.status.success(), "task create failed: {created:?}");
    let listed = common::run_cmd(dir, bin, &["task", "list", "--json"]);
    assert!(listed.status.success(), "task list failed: {listed:?}");
    let task_id = json(&listed)["data"][0]["display_id"]
        .as_str()
        .expect("task list returns display_id")
        .to_string();
    let note = common::run_cmd(
        dir,
        bin,
        &[
            "progress",
            "note",
            "--task",
            &task_id,
            &format!("OPENAI_API_KEY={SECRET}"),
        ],
    );
    assert!(note.status.success(), "progress note failed: {note:?}");
}

fn assert_script_failed(output: &std::process::Output, context: &str) {
    assert!(
        !output.status.success(),
        "{context} should fail; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn publish_creates_branch_redacts_and_is_repeatable() {
    let (dir, bin) = common::setup_test_project("snapshot_publish");
    seed_task_with_secret(&dir, &bin);

    // A local bare origin stands in for the GitHub remote.
    let origin = tempfile::tempdir().unwrap();
    git_ok(origin.path(), &["init", "--bare"]);
    git_ok(
        &dir,
        &["remote", "add", "origin", origin.path().to_str().unwrap()],
    );
    git_ok(&dir, &["push", "-q", "origin", "main"]);

    let wt_parent = tempfile::tempdir().unwrap();
    let wt = wt_parent.path().join("snap");
    let repo_arg = dir.to_str().unwrap();
    let wt_arg = wt.to_str().unwrap();

    // First publish: creates the orphan branch, commits, pushes.
    let first = script(
        &dir,
        &bin,
        &["--repo", repo_arg, "--worktree", wt_arg, "--no-push"],
    );
    assert!(
        first.status.success(),
        "first publish failed: {}\n{}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    assert_eq!(git_ok(&wt, &["rev-list", "--count", "HEAD"]), "1");
    assert!(
        git(
            &dir,
            &["show-ref", "--verify", "refs/heads/carryctx-snapshots"]
        )
        .status
        .success(),
        "local snapshot branch must exist"
    );

    // The staging copy is redacted; the secret never reaches the branch.
    let progress = git_ok(&wt, &["show", "HEAD:progress_items.jsonl"]);
    assert!(progress.contains("***REDACTED***"), "progress: {progress}");
    assert!(!progress.contains(SECRET), "secret leaked into snapshot");
    let grep = git(&wt, &["grep", "-F", SECRET, "HEAD"]);
    assert!(
        !grep.status.success(),
        "secret leaked into snapshot: {}",
        String::from_utf8_lossy(&grep.stdout)
    );

    // The local database keeps the original value.
    let conn = rusqlite::Connection::open(dir.join(".git/carryctx/state.sqlite")).unwrap();
    let stored: String = conn
        .query_row(
            "SELECT content FROM progress_items WHERE content LIKE '%' || ?1 || '%'",
            [SECRET],
            |row| row.get(0),
        )
        .unwrap();
    assert!(stored.contains(SECRET), "local DB must keep the original");

    // Second publish targets the remote: reuses the worktree and appends
    // exactly one snapshot commit per invocation.
    let second = script(&dir, &bin, &["--repo", repo_arg, "--worktree", wt_arg]);
    assert!(
        second.status.success(),
        "second publish failed: {}\n{}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(git_ok(&wt, &["rev-list", "--count", "HEAD"]), "2");
    let pushed = git_ok(
        origin.path(),
        &["rev-parse", "refs/heads/carryctx-snapshots"],
    );
    assert_eq!(pushed, git_ok(&wt, &["rev-parse", "HEAD"]));

    // Dry-run: full export + redaction + validation, zero git state change.
    let head_before = git_ok(&wt, &["rev-parse", "HEAD"]);
    let dry = script(
        &dir,
        &bin,
        &["--repo", repo_arg, "--worktree", wt_arg, "--dry-run"],
    );
    assert!(
        dry.status.success(),
        "dry-run failed: {}",
        String::from_utf8_lossy(&dry.stderr)
    );
    assert_eq!(git_ok(&wt, &["rev-parse", "HEAD"]), head_before);
    assert_eq!(git_ok(&wt, &["status", "--porcelain"]), "");
}

#[test]
fn publish_fails_closed_without_project_state() {
    // Git repository with no `carryctx init`: export must fail before any
    // branch, commit, or push exists (a remote is configured so the failure
    // is genuinely the missing project state, not the early remote check).
    let (dir, bin) = common::setup_test_project("snapshot_publish_uninit");
    let origin = tempfile::tempdir().unwrap();
    git_ok(origin.path(), &["init", "--bare"]);
    git_ok(
        &dir,
        &["remote", "add", "origin", origin.path().to_str().unwrap()],
    );
    let wt_parent = tempfile::tempdir().unwrap();
    let wt = wt_parent.path().join("snap");

    let out = script(
        &dir,
        &bin,
        &[
            "--repo",
            dir.to_str().unwrap(),
            "--worktree",
            wt.to_str().unwrap(),
        ],
    );
    assert_script_failed(&out, "publish without project state");
    assert!(
        !git(
            &dir,
            &["show-ref", "--verify", "refs/heads/carryctx-snapshots"]
        )
        .status
        .success(),
        "no snapshot branch may be created on failure"
    );
    assert!(
        !wt.exists()
            || !git(&wt, &["rev-parse", "--verify", "HEAD"])
                .status
                .success(),
        "no snapshot commit may exist on failure"
    );
    assert!(
        !git(
            origin.path(),
            &["rev-parse", "--verify", "refs/heads/carryctx-snapshots"]
        )
        .status
        .success(),
        "nothing may be pushed on failure"
    );
}
