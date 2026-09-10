//! CTX-0080: `event list` must route through the keyset pagination
//! application service: a full page emits a real `next_cursor`, and
//! `--cursor <token>` resumes strictly after the previous page's last
//! `(occurred_at, id)` tuple instead of re-serving rows.

mod common;

use carryctx_cli::adapter::sqlite::ProjectDatabase;
use carryctx_cli::adapter::unit_of_work::UnitOfWork;
use carryctx_cli::repository::event::{EventRepository, NewEvent};
use serde_json::Value;
use std::process::Command;

const SEED_COUNT: usize = 5;

fn seed_events(dir: &std::path::Path) {
    let db_path = dir.join(".git/carryctx/state.sqlite");
    assert!(db_path.exists(), "project database should exist after init");
    let mut db = ProjectDatabase::open(&db_path).unwrap();
    let project_id: String = db
        .connection()
        .query_row("SELECT id FROM projects LIMIT 1", [], |row| row.get(0))
        .unwrap();
    let uow = UnitOfWork::begin(db.connection_mut()).unwrap();
    let repo = carryctx_cli::adapter::sqlite_repos::SqliteEventRepository::new(uow.connection());
    for i in 0..SEED_COUNT {
        repo.append(&NewEvent {
            id: format!("evt-{i:04}"),
            project_id: project_id.clone(),
            event_type: "task.unblocked".into(),
            actor_agent_id: None,
            session_id: None,
            task_id: None,
            payload: serde_json::json!({ "i": i }),
            occurred_at: format!("2026-08-24T10:00:{:02}+00:00", i),
        })
        .unwrap();
    }
    uow.commit().unwrap();
}

fn run_json(dir: &std::path::Path, bin: &std::path::Path, extra: &[&str]) -> Value {
    let out = Command::new(bin)
        .args(extra)
        // No CARRYCTX_AGENT: the global agent env leaks into the local
        // `--agent` filter and would scope the listing to that actor.
        .current_dir(dir)
        .output()
        .expect("event list should execute");
    assert!(
        out.status.success(),
        "event list {:?} failed: {}",
        extra,
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("valid json envelope")
}

#[test]
fn event_list_pages_follow_next_cursor_without_overlap() {
    let (dir, bin) = common::setup_test_project("event_cursor_cli");
    common::init_and_agent(&dir, &bin);
    seed_events(&dir);

    // init + agent register already emitted system events; page sizes are
    // asserted relative to the real total so the test stays robust.
    let db_path = dir.join(".git/carryctx/state.sqlite");
    let db = ProjectDatabase::open_readonly(&db_path).unwrap();
    let total: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
        .unwrap();
    assert!(total >= 5, "seeded events plus system events expected");

    let page1 = run_json(
        &dir,
        &bin,
        &["--format", "json", "event", "list", "--limit", "2"],
    );
    let events1 = page1["data"]["events"].as_array().unwrap();
    assert_eq!(events1.len(), 2);
    let cursor = page1["data"]["next_cursor"]
        .as_str()
        .expect("a full page must emit a next_cursor token")
        .to_string();

    let page2 = run_json(
        &dir,
        &bin,
        &[
            "--format", "json", "event", "list", "--limit", "2", "--cursor", &cursor,
        ],
    );
    let events2 = page2["data"]["events"].as_array().unwrap();
    assert_eq!(events2.len(), 2);
    for e in events2 {
        assert!(
            !events1.iter().any(|f| f["id"] == e["id"]),
            "page 2 must not repeat page 1 rows"
        );
    }

    // Tail page drains the stream and closes the pager.
    let cursor2 = page2["data"]["next_cursor"].as_str().unwrap().to_string();
    let page3 = run_json(
        &dir,
        &bin,
        &["--format", "json", "event", "list", "--cursor", &cursor2],
    );
    let events3 = page3["data"]["events"].as_array().unwrap();
    assert_eq!(events3.len(), total as usize - 4);
    assert!(
        page3["data"]["next_cursor"].is_null(),
        "exhausted pages must report null next_cursor"
    );

    let mut all: Vec<&str> = events1
        .iter()
        .chain(events2.iter())
        .chain(events3.iter())
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    all.sort();
    all.dedup();
    assert_eq!(
        all.len(),
        total as usize,
        "every event is served exactly once across pages"
    );
}

#[test]
fn garbage_cursor_fails_loudly_through_error_envelope() {
    let (dir, bin) = common::setup_test_project("event_cursor_garbage");
    common::init_and_agent(&dir, &bin);

    let out = Command::new(&bin)
        .args([
            "--format",
            "json",
            "event",
            "list",
            "--cursor",
            "not-a-cursor",
        ])
        .current_dir(&dir)
        .output()
        .expect("event list should execute");

    assert!(!out.status.success(), "malformed cursors must be rejected");
    // Error envelopes print on stderr by contract, even in JSON mode.
    let value: Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stderr)).expect("error envelope");
    assert_eq!(value["success"], Value::Bool(false));
    assert_eq!(value["error"]["code"], "VALIDATION_FAILED");
}

/// CTX-0083: emitted cursors are opaque (`base64url(ts|id).checksum`). A
/// token whose payload was hand-edited fails its checksum and is rejected
/// through the same clean VALIDATION_FAILED envelope as any other malformed
/// cursor — instead of silently serving a wrong page.
#[test]
fn tampered_cursor_token_is_rejected() {
    let (dir, bin) = common::setup_test_project("event_cursor_tamper");
    common::init_and_agent(&dir, &bin);
    seed_events(&dir);

    let page1 = run_json(
        &dir,
        &bin,
        &["--format", "json", "event", "list", "--limit", "2"],
    );
    let cursor = page1["data"]["next_cursor"]
        .as_str()
        .expect("full page emits a cursor")
        .to_string();
    assert!(
        !cursor.contains('|'),
        "emitted tokens must not carry plaintext tuples: {cursor}"
    );

    // Tamper the checksum suffix of a structurally valid token.
    let (body, _) = cursor.split_once('.').expect("opaque tokens have a dot");
    let out = Command::new(&bin)
        .args([
            "--format",
            "json",
            "event",
            "list",
            "--cursor",
            &format!("{body}.deadbeef"),
        ])
        .current_dir(&dir)
        .output()
        .expect("event list should execute");

    assert!(!out.status.success(), "tampered cursors must be rejected");
    let value: Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stderr)).expect("error envelope");
    assert_eq!(value["success"], Value::Bool(false));
    assert_eq!(value["error"]["code"], "VALIDATION_FAILED");

    // The untouched token still pages correctly.
    let replay = run_json(
        &dir,
        &bin,
        &[
            "--format", "json", "event", "list", "--limit", "2", "--cursor", &cursor,
        ],
    );
    assert!(replay["data"]["events"].as_array().unwrap().len() == 2);
}
