use carryctx::adapter::sqlite::ProjectDatabase;
use carryctx::adapter::sqlite_repos::{SqliteAgentRepository, SqliteTaskRepository};
use carryctx::domain::task::{TaskPriority, TaskStatus};
use carryctx::repository::{AgentRepository, NewAgent, NewTask, TaskRepository};

fn project(db: &ProjectDatabase, id: &str) {
    db.connection()
        .execute(
            "INSERT INTO projects (id, name, task_prefix, repository_root, git_common_dir, main_branch, schema_version, created_at, updated_at)
             VALUES (?1, 'test', 'CTX', '/tmp/test-repo', '/tmp/test-repo/.git', 'main', 12, 'now', 'now')",
            [id],
        )
        .unwrap();
}

#[test]
fn migration_exposes_team_fields_and_composite_task_foreign_key() {
    let dir = tempfile::tempdir().unwrap();
    let db = ProjectDatabase::create_fresh(dir.path().join("state.sqlite")).unwrap();
    for (table, column) in [
        ("agents", "kind"),
        ("tasks", "required_role"),
        ("tasks", "team_id"),
    ] {
        let found: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
                [table, column],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(found, 1, "missing {table}.{column}");
    }
    let foreign_keys: Vec<(String, String, String, String)> = db
        .connection()
        .prepare(
            "SELECT \"table\", \"from\", \"to\", on_delete FROM pragma_foreign_key_list('tasks')",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    assert!(foreign_keys.iter().any(|(table, from, to, action)| {
        table == "teams" && from == "team_id" && to == "id" && action == "SET NULL"
    }));
}

#[test]
fn task_team_round_trip_and_cross_project_delete_behavior() {
    let dir = tempfile::tempdir().unwrap();
    let db = ProjectDatabase::create_fresh(dir.path().join("state.sqlite")).unwrap();
    project(&db, "project-a");
    db.connection()
        .execute("INSERT INTO teams (id, project_id, name, created_at, updated_at) VALUES ('team-a', 'project-a', 'alpha', 'now', 'now')", [])
        .unwrap();
    let task = SqliteTaskRepository::new(db.connection())
        .create(
            &NewTask {
                id: "task-a".into(),
                display_id: "CTX-1".into(),
                project_id: "project-a".into(),
                title: "task".into(),
                description: None,
                status: TaskStatus::Planned,
                priority: TaskPriority::Normal,
                owner_agent_id: None,
                parent_task_id: None,
                required_role: Some("reviewer".into()),
                team_id: Some("team-a".into()),
            },
            "now",
        )
        .unwrap();
    assert_eq!(task.required_role.as_deref(), Some("reviewer"));
    assert_eq!(task.team_id.as_deref(), Some("team-a"));
    db.connection()
        .execute(
            "DELETE FROM teams WHERE id = 'team-a' AND project_id = 'project-a'",
            [],
        )
        .unwrap();
    let team_id: Option<String> = db
        .connection()
        .query_row("SELECT team_id FROM tasks WHERE id = 'task-a'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(team_id, None);
}

#[test]
fn agent_kind_is_nullable_and_validated() {
    let dir = tempfile::tempdir().unwrap();
    let db = ProjectDatabase::create_fresh(dir.path().join("state.sqlite")).unwrap();
    project(&db, "project-a");
    let invalid = SqliteAgentRepository::new(db.connection()).register(
        &NewAgent {
            id: "agent-a".into(),
            project_id: "project-a".into(),
            name: "agent".into(),
            provider: "test".into(),
            role: None,
            kind: Some("invalid".into()),
            metadata: serde_json::json!({}),
        },
        "now",
    );
    assert!(invalid.is_err());
}
