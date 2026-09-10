#![allow(dead_code)]
// Common test utilities for carryctx integration tests
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU16, Ordering};

static TEST_COUNTER: AtomicU16 = AtomicU16::new(0);

pub fn test_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_carryctx"))
}

/// GIT_* variables stripped from every test-spawned git command so fixtures
/// never resolve into the repository under test (see `fixture_git`).
pub const GIT_STATE_VARS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_AUTHOR_DATE",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "GIT_COMMITTER_DATE",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
];

/// Run a fixture git command in `dir` with inherited GIT_* state stripped
/// and a hard success assertion.
///
/// Hook runners (lefthook) execute tests with GIT_DIR/GIT_INDEX_FILE set to
/// the repository under test; without scrubbing, fixture commands resolve
/// into that repo instead of the temp dir — CTX-0082: an unscrubbed fixture
/// "init" commit once landed on the feature branch and replaced the tree.
pub fn fixture_git(dir: &std::path::Path, args: &[&str]) {
    let mut command = Command::new("git");
    command.args(args).current_dir(dir);
    for var in GIT_STATE_VARS {
        command.env_remove(var);
    }
    let output = output_of(command);
    assert!(
        output.status.success(),
        "fixture git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn output_of(mut command: Command) -> std::process::Output {
    command.output().expect("git should spawn")
}

pub fn setup_test_project(name: &str) -> (PathBuf, PathBuf) {
    let count = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("carryctx_test_{name}_{count}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // Init git repo (with GIT_* env isolation — see fixture_git)
    fixture_git(&dir, &["init", "-b", "main"]);
    fixture_git(&dir, &["config", "user.email", "test@carryctx.dev"]);
    fixture_git(&dir, &["config", "user.name", "Test"]);
    fixture_git(&dir, &["commit", "--allow-empty", "-m", "init"]);

    (dir, test_binary())
}

pub fn run_cmd(
    dir: &std::path::Path,
    bin: &std::path::Path,
    args: &[&str],
) -> std::process::Output {
    run_cmd_as(dir, bin, "tester", args)
}

/// Run the CLI as a specific named agent (`CARRYCTX_AGENT`).
pub fn run_cmd_as(
    dir: &std::path::Path,
    bin: &std::path::Path,
    agent: &str,
    args: &[&str],
) -> std::process::Output {
    std::process::Command::new(bin)
        .args(args)
        .env("CARRYCTX_AGENT", agent)
        .current_dir(dir)
        .output()
        .expect("command should execute")
}

pub fn init_and_agent(dir: &std::path::Path, bin: &std::path::Path) {
    run_cmd(dir, bin, &["init", "--force"]);
    run_cmd(
        dir,
        bin,
        &[
            "agent",
            "register",
            "--name",
            "tester",
            "--provider",
            "test",
        ],
    );
}
