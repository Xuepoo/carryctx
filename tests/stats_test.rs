mod common;

use std::process::Command;

/// Regression tests for https://github.com/Xuepoo/carryctx/issues/60.
///
/// Two stats defects: (A) `session end` never wrote `ended_at`, so stats
/// billed every session from its start to *now*, producing absurd "Time
/// Spent" figures (opencode showed 4784h on a project weeks old); (B) the
/// per-agent checkpoint count joined `checkpoints.session_id = sessions.id`,
/// but session_id is never populated (checkpoints carry agent_id), so the
/// column was always 0 while the overview totalled real checkpoints.
fn setup(dir: &std::path::Path, bin: &std::path::Path) {
    common::run_cmd(dir, bin, &["init", "--force", "--task-prefix", "ST"]);
    common::run_cmd(
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

#[test]
fn test_session_end_writes_ended_at() {
    let (dir, bin) = common::setup_test_project("stats_ended_at");
    setup(&dir, &bin);

    let start = common::run_cmd(&dir, &bin, &["session", "start"]);
    assert!(start.status.success(), "session start failed");
    let end = common::run_cmd(&dir, &bin, &["session", "end"]);
    assert!(end.status.success(), "session end failed");

    let db_path = dir.join(".git/carryctx/state.sqlite");
    let out = Command::new("sqlite3")
        .arg(&db_path)
        .arg("SELECT ended_at IS NOT NULL FROM sessions WHERE state = 'ended';")
        .output()
        .unwrap();
    let val = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        val.trim(),
        "1",
        "ended session must have ended_at set after `session end`"
    );
}

#[test]
fn test_stats_counts_checkpoints_per_agent_and_does_not_inflate_time() {
    let (dir, bin) = common::setup_test_project("stats_checkpoints");
    setup(&dir, &bin);

    // One ended session and one checkpoint (session_id stays NULL by design).
    common::run_cmd(&dir, &bin, &["session", "start"]);
    common::run_cmd(&dir, &bin, &["session", "end"]);
    let task = common::run_cmd(&dir, &bin, &["task", "create", "--title", "T", "--json"]);
    let stdout = String::from_utf8_lossy(&task.stdout);
    let start = stdout.find("\"display_id\":\"").expect("display_id") + 14;
    let tid = &stdout[start..stdout[start..].find('"').unwrap() + start];
    let ckpt = common::run_cmd(
        &dir,
        &bin,
        &["checkpoint", "--task", tid, "--done", "things"],
    );
    assert!(ckpt.status.success(), "checkpoint failed");

    // Simulate a stale open session from long ago: started days back, no
    // ended_at (as if the agent crashed before 0.5.0).
    let db_path = dir.join(".git/carryctx/state.sqlite");
    let stale_sql = "UPDATE sessions SET started_at = datetime('now', '-30 days'), last_activity_at = datetime('now', '-30 days'), ended_at = NULL WHERE state = 'ended';";
    let s = Command::new("sqlite3")
        .arg(&db_path)
        .arg(stale_sql)
        .status()
        .unwrap();
    assert!(s.success(), "sqlite3 update failed");

    let stats = common::run_cmd(&dir, &bin, &["stats", "--json"]);
    assert!(stats.status.success(), "stats failed");
    let stdout = String::from_utf8_lossy(&stats.stdout);

    // (B) per-agent checkpoints counted via agent_id, not the NULL session_id.
    assert!(
        stdout.contains("\"total_checkpoints\":1"),
        "per-agent checkpoints must be 1, got: {stdout}"
    );
    // (A) the ended session contributes last_activity_at - started_at (~0s),
    // not 30 days; the stale open session caps at its last activity too.
    assert!(
        stdout.contains("\"total_seconds\":0"),
        "stale sessions must not bill days of wall-clock time, got: {stdout}"
    );
}
