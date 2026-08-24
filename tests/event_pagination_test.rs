//! CTX-0072 / issue #105: event cursor pagination must be strict on the
//! `(occurred_at, id)` tuple. Bulk transitions emit many events sharing one
//! timestamp; the old inclusive `occurred_at <=` cursor re-served those rows
//! page after page — an infinite pager with duplicates.

use carryctx::adapter::sqlite::ProjectDatabase;
use carryctx::adapter::unit_of_work::UnitOfWork;
use carryctx::application::event::list_events;
use carryctx::repository::event::{EventFilter, EventRepository, NewEvent};

const PROJECT: &str = "project-a";
const SHARED_TS: &str = "2026-08-24T10:00:00+00:00";
const TOTAL: usize = 11;
/// Larger than DEFAULT_EVENT_LIST_LIMIT (200) for the cap test.
const OVER_LIMIT: usize = 210;

fn seed_count(db_path: &std::path::Path, total: usize) {
    let mut db = ProjectDatabase::create_fresh(db_path).unwrap();
    db.connection()
        .execute(
            "INSERT INTO projects (id, name, task_prefix, repository_root, git_common_dir, main_branch, schema_version, created_at, updated_at)
             VALUES (?1, 'test', 'CTX', '/tmp/r', '/tmp/r/.git', 'main', 4, 'now', 'now')",
            [PROJECT],
        )
        .unwrap();

    let conn = db.connection_mut();
    let uow = UnitOfWork::begin(conn).unwrap();
    let repo = carryctx::adapter::sqlite_repos::SqliteEventRepository::new(uow.connection());
    // Every event shares ONE timestamp, like a bulk promotion batch.
    for i in 0..total {
        repo.append(&NewEvent {
            id: format!("evt-{i:04}"),
            project_id: PROJECT.into(),
            event_type: "task.unblocked".into(),
            actor_agent_id: None,
            session_id: None,
            task_id: None,
            payload: serde_json::json!({ "i": i }),
            occurred_at: SHARED_TS.into(),
        })
        .unwrap();
    }
    uow.commit().unwrap();
}

#[test]
fn same_timestamp_events_paginate_without_duplicates_or_loops() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.sqlite");
    seed_count(&db_path, TOTAL);

    let mut db = ProjectDatabase::open(&db_path).unwrap();
    let uow = UnitOfWork::begin(db.connection_mut()).unwrap();

    let filter = EventFilter {
        project_id: PROJECT.into(),
        task_id: None,
        agent_id: None,
        session_id: None,
        event_type: None,
        since: None,
        until: None,
        limit: Some(3),
    };

    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..TOTAL {
        let page = list_events(PROJECT, &filter, cursor.as_deref(), &uow).unwrap();
        assert!(page.events.len() <= 3);
        for e in &page.events {
            seen.push(e.id.clone());
        }
        match &page.next_cursor {
            Some(next) => cursor = Some(next.clone()),
            None => break,
        }
    }

    assert_eq!(
        seen.len(),
        TOTAL,
        "must page through every event exactly once"
    );
    let unique: std::collections::HashSet<_> = seen.iter().collect();
    assert_eq!(unique.len(), TOTAL, "pages must not repeat events");
}

#[test]
fn garbage_cursor_is_rejected_not_silently_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.sqlite");
    seed_count(&db_path, TOTAL);

    let mut db = ProjectDatabase::open(&db_path).unwrap();
    let uow = UnitOfWork::begin(db.connection_mut()).unwrap();

    let filter = EventFilter {
        project_id: PROJECT.into(),
        task_id: None,
        agent_id: None,
        session_id: None,
        event_type: None,
        since: None,
        until: None,
        limit: Some(3),
    };

    let result = list_events(PROJECT, &filter, Some("garbage"), &uow);
    assert!(result.is_err(), "malformed cursors must be rejected");
}

#[test]
fn default_limit_caps_unbounded_event_lists() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.sqlite");
    seed_count(&db_path, OVER_LIMIT);

    let db = ProjectDatabase::open_readonly(&db_path).unwrap();
    let repo = carryctx::adapter::sqlite_repos::SqliteEventRepository::new(db.connection());

    let filter = EventFilter {
        project_id: PROJECT.into(),
        task_id: None,
        agent_id: None,
        session_id: None,
        event_type: None,
        since: None,
        until: None,
        limit: None, // no explicit limit: default cap applies
    };
    let events = repo.list(&filter).unwrap();
    assert_eq!(
        events.len(),
        carryctx::adapter::sqlite_repos::DEFAULT_EVENT_LIST_LIMIT as usize,
        "default limit must cap the result set"
    );
}

// CTX-0080: with no explicit --limit the application layer never fetched a
// lookahead row, so a full default-sized page reported `next_cursor: null`
// and the remaining events were unreachable through pagination.
#[test]
fn default_cap_page_carries_next_cursor_and_resumes_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.sqlite");
    seed_count(&db_path, OVER_LIMIT);

    let mut db = ProjectDatabase::open(&db_path).unwrap();
    let uow = UnitOfWork::begin(db.connection_mut()).unwrap();

    let filter = EventFilter {
        project_id: PROJECT.into(),
        task_id: None,
        agent_id: None,
        session_id: None,
        event_type: None,
        since: None,
        until: None,
        limit: None, // default cap applies
    };

    let first = list_events(PROJECT, &filter, None, &uow).unwrap();
    assert_eq!(
        first.events.len(),
        carryctx::adapter::sqlite_repos::DEFAULT_EVENT_LIST_LIMIT as usize,
        "first page honours the default cap"
    );
    let cursor = first
        .next_cursor
        .expect("a full default-size page must emit next_cursor");

    let second = list_events(PROJECT, &filter, Some(&cursor), &uow).unwrap();
    let first_ids: std::collections::HashSet<_> =
        first.events.iter().map(|e| e.id.clone()).collect();
    assert_eq!(second.events.len(), OVER_LIMIT - first.events.len());
    assert!(
        second.events.iter().all(|e| !first_ids.contains(&e.id)),
        "the resume page must not repeat first-page rows"
    );
    assert!(
        second.next_cursor.is_none(),
        "the tail page has nothing left to paginate"
    );
}
