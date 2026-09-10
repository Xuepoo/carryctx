mod common;

use rusqlite::Connection;
use std::path::Path;
use std::process::Output;

fn state_db(dir: &Path) -> Connection {
    Connection::open(dir.join(".git/carryctx/state.sqlite")).unwrap()
}

fn json_of(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
        panic!(
            "stdout was not JSON: {}; stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn error_of(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stderr).unwrap_or_else(|_| {
        panic!(
            "stderr was not a JSON error envelope: stdout={}; stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

/// Initialize the project, create + start a task, and start a session bound
/// to it. Returns `(task_display_id, session_ulid)`.
fn start_task_session(dir: &Path, bin: &Path) -> (String, String) {
    common::init_and_agent(dir, bin);
    let created = common::run_cmd(
        dir,
        bin,
        &["--json", "task", "create", "--title", "session refs"],
    );
    assert!(
        created.status.success(),
        "task create failed: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    let task = json_of(&created)["data"]["display_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        common::run_cmd(dir, bin, &["task", "start", &task])
            .status
            .success()
    );
    let started = common::run_cmd(dir, bin, &["--json", "session", "start", "--task", &task]);
    assert!(
        started.status.success(),
        "session start failed: {}",
        String::from_utf8_lossy(&started.stderr)
    );
    let session = json_of(&started)["data"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        session.chars().count(),
        26,
        "session id must be a full ULID"
    );
    (task, session)
}

/// CTX-0148 issue #137 step 1: `session start` must print the full ULID so the
/// value can be passed to `--session`/`CARRYCTX_SESSION` without truncation.
#[test]
fn session_start_text_prints_full_ulid() {
    let (dir, bin) = common::setup_test_project("session_ref_full_ulid");
    common::init_and_agent(&dir, &bin);

    let started = common::run_cmd(&dir, &bin, &["session", "start"]);
    assert!(
        started.status.success(),
        "session start failed: {}",
        String::from_utf8_lossy(&started.stderr)
    );
    let stdout = String::from_utf8_lossy(&started.stdout);
    let id = stdout
        .lines()
        .find_map(|line| line.strip_prefix("Session started: "))
        .expect("session start summary line")
        .trim()
        .to_string();
    assert_eq!(
        id.chars().count(),
        26,
        "session start must print the full ULID, got {id:?}"
    );
    let count: i64 = state_db(&dir)
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE id = ?1",
            [&id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "printed id must be the persisted session ULID");
}

/// CTX-0148 issue #137 step 2: a unique 8-char prefix must resolve to the full
/// ULID for `checkpoint create --session` instead of failing the FK check.
#[test]
fn checkpoint_create_resolves_unique_short_session_ref() {
    let (dir, bin) = common::setup_test_project("session_ref_checkpoint");
    let (task, session) = start_task_session(&dir, &bin);
    let short = &session[..8];

    let cp = common::run_cmd(
        &dir,
        &bin,
        &[
            "--json",
            "--non-interactive",
            "checkpoint",
            "--task",
            &task,
            "--session",
            short,
            "--no-git",
        ],
    );
    assert!(
        cp.status.success(),
        "checkpoint with short ref failed: {}",
        String::from_utf8_lossy(&cp.stderr)
    );
    let stored: String = state_db(&dir)
        .query_row("SELECT session_id FROM checkpoints LIMIT 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(stored, session, "checkpoint must persist the full ULID");
}

/// CTX-0148 issue #137: a full ULID keeps working unchanged.
#[test]
fn checkpoint_create_accepts_full_session_ulid_unchanged() {
    let (dir, bin) = common::setup_test_project("session_ref_full_ulid_passthrough");
    let (task, session) = start_task_session(&dir, &bin);

    let cp = common::run_cmd(
        &dir,
        &bin,
        &[
            "--json",
            "--non-interactive",
            "checkpoint",
            "--task",
            &task,
            "--session",
            &session,
            "--no-git",
        ],
    );
    assert!(
        cp.status.success(),
        "checkpoint with full ref failed: {}",
        String::from_utf8_lossy(&cp.stderr)
    );
    let stored: String = state_db(&dir)
        .query_row("SELECT session_id FROM checkpoints LIMIT 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(stored, session);
}

/// CTX-0148 issue #137 step 3: `progress_items.source_session_id` must store the
/// canonical full ULID, never the raw short prefix.
#[test]
fn progress_create_persists_full_session_id_for_short_ref() {
    let (dir, bin) = common::setup_test_project("session_ref_progress");
    let (task, session) = start_task_session(&dir, &bin);
    let short = &session[..8];

    let note = common::run_cmd(
        &dir,
        &bin,
        &[
            "--json",
            "--session",
            short,
            "progress",
            "todo",
            "short ref progress",
            "--task",
            &task,
        ],
    );
    assert!(
        note.status.success(),
        "progress todo with short ref failed: {}",
        String::from_utf8_lossy(&note.stderr)
    );
    let stored: Option<String> = state_db(&dir)
        .query_row(
            "SELECT source_session_id FROM progress_items WHERE content = 'short ref progress'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        stored.as_deref(),
        Some(session.as_str()),
        "source_session_id must be the full ULID"
    );
}

/// CTX-0148 issue #137: `handoff create` must resolve a short session ref too.
#[test]
fn handoff_create_resolves_unique_short_session_ref() {
    let (dir, bin) = common::setup_test_project("session_ref_handoff");
    let (task, session) = start_task_session(&dir, &bin);
    let short = &session[..8];

    let handoff = common::run_cmd(
        &dir,
        &bin,
        &[
            "--json",
            "--session",
            short,
            "handoff",
            "create",
            "--target",
            "tester",
            "--summary",
            "short ref handoff",
            "--task",
            &task,
        ],
    );
    assert!(
        handoff.status.success(),
        "handoff create with short ref failed: {}",
        String::from_utf8_lossy(&handoff.stderr)
    );
    let stored: String = state_db(&dir)
        .query_row("SELECT session_id FROM handoffs LIMIT 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(stored, session, "handoff must persist the full ULID");
}

/// CTX-0148: an ambiguous prefix must be rejected with a clear validation
/// error, never passed to SQLite as a prefix or resolved arbitrarily.
#[test]
fn ambiguous_session_prefix_is_rejected_without_writing() {
    let (dir, bin) = common::setup_test_project("session_ref_ambiguous");
    let (task, session) = start_task_session(&dir, &bin);
    let short = session[..8].to_string();

    // Insert a second session sharing the 8-char prefix. The ULID body is
    // irrelevant to the resolver: ambiguity is decided purely by prefix.
    let db = state_db(&dir);
    let (project_id, agent_id): (String, String) = db
        .query_row(
            "SELECT project_id, agent_id FROM sessions WHERE id = ?1",
            [&session],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let twin = format!("{short}ZZZZZZZZZZZZZZZZZZ");
    db.execute(
        "INSERT INTO sessions (id, project_id, agent_id, state, provider, working_directory, metadata_json, started_at, last_activity_at, updated_at)
         VALUES (?1, ?2, ?3, 'active', 'test', '', '{}', 'now', 'now', 'now')",
        rusqlite::params![twin, project_id, agent_id],
    )
    .unwrap();
    drop(db);

    let cp = common::run_cmd(
        &dir,
        &bin,
        &[
            "--json",
            "--non-interactive",
            "checkpoint",
            "--task",
            &task,
            "--session",
            &short,
            "--no-git",
        ],
    );
    assert!(!cp.status.success(), "ambiguous ref must fail");
    assert_eq!(cp.status.code(), Some(8), "ambiguous ref must exit 8");
    let error = error_of(&cp);
    assert_eq!(error["error"]["code"], "VALIDATION_FAILED");
    let message = error["error"]["message"].as_str().unwrap();
    assert!(message.contains("ambiguous"), "message: {message}");
    assert!(
        message.contains(&session),
        "message must list candidate: {message}"
    );
    assert!(
        message.contains(&twin),
        "message must list candidate: {message}"
    );
    let checkpoints: i64 = state_db(&dir)
        .query_row("SELECT COUNT(*) FROM checkpoints", [], |row| row.get(0))
        .unwrap();
    assert_eq!(checkpoints, 0, "ambiguous ref must not persist anything");

    // Positional session refs resolve through the same policy.
    let show = common::run_cmd(&dir, &bin, &["--json", "session", "show", &short]);
    assert!(!show.status.success(), "ambiguous show must fail");
    assert_eq!(error_of(&show)["error"]["code"], "VALIDATION_FAILED");
}

/// CTX-0148: unknown short refs and unknown full ULIDs are rejected with a
/// clear not-found error instead of a DATABASE_ERROR FK failure.
#[test]
fn unknown_session_refs_are_rejected_without_fk_crash() {
    let (dir, bin) = common::setup_test_project("session_ref_unknown");
    let (task, _session) = start_task_session(&dir, &bin);

    let cp = common::run_cmd(
        &dir,
        &bin,
        &[
            "--json",
            "--non-interactive",
            "checkpoint",
            "--task",
            &task,
            "--session",
            "ZZZZZZZZ",
            "--no-git",
        ],
    );
    assert!(!cp.status.success(), "unknown ref must fail");
    assert_eq!(cp.status.code(), Some(7), "unknown ref must exit 7");
    let error = error_of(&cp);
    assert_eq!(error["error"]["code"], "RESOURCE_NOT_FOUND");
    assert!(
        !String::from_utf8_lossy(&cp.stderr).contains("FOREIGN KEY"),
        "unknown ref must not reach the FK constraint"
    );

    // An unknown full ULID must fail the same way (also no FK crash).
    let unknown_full = format!("01{}", "Z".repeat(24));
    let cp_full = common::run_cmd(
        &dir,
        &bin,
        &[
            "--json",
            "--non-interactive",
            "checkpoint",
            "--task",
            &task,
            "--session",
            &unknown_full,
            "--no-git",
        ],
    );
    assert!(!cp_full.status.success(), "unknown full ULID must fail");
    assert_eq!(error_of(&cp_full)["error"]["code"], "RESOURCE_NOT_FOUND");

    // Global `--session` (CARRYCTX_SESSION equivalent) is validated too.
    let progress = common::run_cmd(
        &dir,
        &bin,
        &[
            "--json",
            "--session",
            "ZZZZZZZZ",
            "progress",
            "todo",
            "unknown session",
            "--task",
            &task,
        ],
    );
    assert!(!progress.status.success(), "unknown global ref must fail");
    assert_eq!(error_of(&progress)["error"]["code"], "RESOURCE_NOT_FOUND");
    let progress_rows: i64 = state_db(&dir)
        .query_row("SELECT COUNT(*) FROM progress_items", [], |row| row.get(0))
        .unwrap();
    assert_eq!(progress_rows, 0, "unknown ref must not persist progress");

    let checkpoints: i64 = state_db(&dir)
        .query_row("SELECT COUNT(*) FROM checkpoints", [], |row| row.get(0))
        .unwrap();
    assert_eq!(checkpoints, 0, "unknown ref must not persist checkpoints");
}

/// CTX-0148: `session show`/`pause` accept unique short refs.
#[test]
fn session_commands_accept_unique_short_refs() {
    let (dir, bin) = common::setup_test_project("session_ref_commands");
    let (_task, session) = start_task_session(&dir, &bin);
    let short = &session[..8];

    let show = common::run_cmd(&dir, &bin, &["--json", "session", "show", short]);
    assert!(
        show.status.success(),
        "session show with short ref failed: {}",
        String::from_utf8_lossy(&show.stderr)
    );
    assert_eq!(json_of(&show)["data"]["id"].as_str().unwrap(), session);

    let pause = common::run_cmd(&dir, &bin, &["--json", "session", "pause", short]);
    assert!(
        pause.status.success(),
        "session pause with short ref failed: {}",
        String::from_utf8_lossy(&pause.stderr)
    );
    assert_eq!(json_of(&pause)["data"]["state"].as_str().unwrap(), "paused");
}
