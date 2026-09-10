//! Regression tests for CTX-0068 / issue #101 (SQL robustness).
//!
//! `SqliteWorktreeRepository::prune_stale` used to issue manual
//! BEGIN IMMEDIATE/COMMIT via execute_batch on the shared connection,
//! which broke with "cannot start a transaction within a transaction"
//! whenever a caller passed a unit-of-work connection, and swallowed
//! ROLLBACK errors. It must instead use the rusqlite Transaction API,
//! joining an ambient transaction when one is already open.

mod common;

use carryctx_cli::adapter::sqlite::ProjectDatabase;
use carryctx_cli::adapter::sqlite_repos::SqliteWorktreeRepository;
use carryctx_cli::repository::worktree::WorktreeRepository;

fn seed_project(db: &ProjectDatabase) {
    db.connection()
        .execute(
            "INSERT INTO projects (id, name, task_prefix, repository_root, git_common_dir, main_branch, schema_version, created_at, updated_at)
             VALUES ('proj1', 'seed', 'SD', '/nonexistent/root', '/nonexistent/common', 'main', 4,
                     '2020-01-01T00:00:00+00:00', '2020-01-01T00:00:00+00:00')",
            [],
        )
        .unwrap();
}

fn insert_worktree(db: &ProjectDatabase, id: &str, path: &str) {
    db.connection()
        .execute(
            "INSERT INTO worktrees (id, project_id, normalized_path, git_common_dir, bound_at, updated_at)
             VALUES (?1, 'proj1', ?2, '/nonexistent/common',
                     '2020-01-01T00:00:00+00:00', '2020-01-01T00:00:00+00:00')",
            rusqlite::params![id, path],
        )
        .unwrap();
}

fn worktree_count(db: &ProjectDatabase) -> i64 {
    db.connection()
        .query_row("SELECT COUNT(*) FROM worktrees", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn prune_stale_removes_only_missing_directories_on_autocommit_connection() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.sqlite");
    let db = ProjectDatabase::create_fresh(&db_path).unwrap();
    seed_project(&db);

    let stale_dir = tempfile::tempdir().unwrap();
    let stale_path = stale_dir.path().join("gone");
    std::fs::create_dir_all(&stale_path).unwrap();
    let live_path = stale_dir.path().join("live");
    std::fs::create_dir_all(&live_path).unwrap();

    insert_worktree(&db, "w_stale", &stale_path.to_string_lossy());
    insert_worktree(&db, "w_live", &live_path.to_string_lossy());
    std::fs::remove_dir(&stale_path).unwrap();

    let repo = SqliteWorktreeRepository::new(db.connection());
    let pruned = repo
        .prune_stale("proj1", dir.path(), None, None, "2020-01-02T00:00:00+00:00")
        .expect("prune_stale should succeed on an autocommit connection");
    assert_eq!(pruned.len(), 1, "only the missing directory is stale");
    assert_eq!(pruned[0].id, "w_stale");
    assert_eq!(worktree_count(&db), 1, "the live registration must survive");
}

#[test]
fn prune_stale_joins_an_ambient_unit_of_work_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("state.sqlite");
    let db = ProjectDatabase::create_fresh(&db_path).unwrap();
    seed_project(&db);

    let stale_dir = tempfile::tempdir().unwrap();
    let stale_path = stale_dir.path().join("gone");
    std::fs::create_dir_all(&stale_path).unwrap();
    insert_worktree(&db, "w_stale", &stale_path.to_string_lossy());
    std::fs::remove_dir(&stale_path).unwrap();

    let mut db = db;
    let uow = db.begin_unit_of_work().unwrap();
    let repo = SqliteWorktreeRepository::new(uow.connection());

    // Previously failed with "cannot start a transaction within a
    // transaction" because the repository issued its own BEGIN IMMEDIATE.
    let pruned = repo
        .prune_stale("proj1", dir.path(), None, None, "2020-01-02T00:00:00+00:00")
        .expect("prune_stale must join the ambient transaction");
    assert_eq!(pruned.len(), 1);

    // The delete is visible inside the ambient transaction...
    let inside_count: i64 = uow
        .connection()
        .query_row("SELECT COUNT(*) FROM worktrees", [], |row| row.get(0))
        .unwrap();
    assert_eq!(inside_count, 0);
    uow.rollback().unwrap();

    // ...and rolling the unit of work back undoes it entirely.
    let db = ProjectDatabase::open_readonly(&db_path).unwrap();
    assert_eq!(
        worktree_count(&db),
        1,
        "rollback of the ambient transaction must restore the registration"
    );
}
