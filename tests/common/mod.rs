// Common test utilities for carryctx integration tests
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU16, Ordering};

static TEST_COUNTER: AtomicU16 = AtomicU16::new(0);

pub fn test_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_carryctx"))
}

pub fn setup_test_project(name: &str) -> (PathBuf, PathBuf) {
    let count = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("carryctx_test_{name}_{count}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    
    // Init git repo
    Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(&dir)
        .output().unwrap();
    Command::new("git")
        .args(["config", "user.email", "test@carryctx.dev"])
        .current_dir(&dir)
        .output().unwrap();
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(&dir)
        .output().unwrap();
    Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(&dir)
        .output().unwrap();
    
    (dir, test_binary())
}
