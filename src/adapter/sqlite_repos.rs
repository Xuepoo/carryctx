use rusqlite::{Connection, Row, params};

use crate::domain::agent::Agent;
use crate::domain::checkpoint::{Checkpoint, CheckpointCorrection};
use crate::domain::collaboration::{Decision, Handoff, HandoffStatus, TaskScope};
use crate::domain::dependency::{DependencyEdge, DependencyKind};
use crate::domain::progress::{ProgressStatus, ProgressType};
use crate::domain::session::SessionState;
use crate::domain::task::{TaskPriority, TaskStatus};
use crate::error::CarryCtxError;
use crate::repository::{
    AgentFilter, AgentRepository, CheckpointRepository, DecisionRepository, DependencyRepository,
    EventFilter, EventRecord, EventRepository, HandoffRepository, NewAgent, NewEvent,
    NewProgressItem, NewSession, NewTask, NewWorktree, ProgressFilter, ProgressItemRecord,
    ProgressRepository, ScopeRepository, SessionRecord, SessionRepository, TaskFilter, TaskRecord,
    TaskRepository, WorktreeRecord, WorktreeRepository,
};

// ── Status / enum conversions ──────────────────────────────────────────

fn task_status_from_sql(s: &str) -> Result<TaskStatus, CarryCtxError> {
    match s {
        "planned" => Ok(TaskStatus::Planned),
        "ready" => Ok(TaskStatus::Ready),
        "in_progress" => Ok(TaskStatus::InProgress),
        "blocked" => Ok(TaskStatus::Blocked),
        "review" => Ok(TaskStatus::Review),
        "completed" => Ok(TaskStatus::Completed),
        "cancelled" => Ok(TaskStatus::Cancelled),
        other => Err(CarryCtxError::database_error(format!(
            "Unknown task status: {other}"
        ))),
    }
}

fn task_status_to_sql(s: &TaskStatus) -> &'static str {
    match s {
        TaskStatus::Planned => "planned",
        TaskStatus::Ready => "ready",
        TaskStatus::InProgress => "in_progress",
        TaskStatus::Blocked => "blocked",
        TaskStatus::Review => "review",
        TaskStatus::Completed => "completed",
        TaskStatus::Cancelled => "cancelled",
    }
}

fn task_priority_from_sql(s: &str) -> Result<TaskPriority, CarryCtxError> {
    match s {
        "low" => Ok(TaskPriority::Low),
        "normal" => Ok(TaskPriority::Normal),
        "high" => Ok(TaskPriority::High),
        "urgent" => Ok(TaskPriority::Urgent),
        other => Err(CarryCtxError::database_error(format!(
            "Unknown task priority: {other}"
        ))),
    }
}

fn task_priority_to_sql(s: &TaskPriority) -> &'static str {
    match s {
        TaskPriority::Low => "low",
        TaskPriority::Normal => "normal",
        TaskPriority::High => "high",
        TaskPriority::Urgent => "urgent",
    }
}

fn agent_status_to_sql(s: &crate::domain::agent::AgentStatus) -> &'static str {
    match s {
        crate::domain::agent::AgentStatus::Active => "active",
        crate::domain::agent::AgentStatus::Deactivated => "deactivated",
    }
}

fn agent_status_from_sql(s: &str) -> Result<crate::domain::agent::AgentStatus, CarryCtxError> {
    match s {
        "active" => Ok(crate::domain::agent::AgentStatus::Active),
        "inactive" | "deactivated" => Ok(crate::domain::agent::AgentStatus::Deactivated),
        other => Err(CarryCtxError::database_error(format!(
            "Unknown agent status: {other}"
        ))),
    }
}

fn session_state_to_sql(s: &SessionState) -> &'static str {
    match s {
        SessionState::Active => "active",
        SessionState::Paused => "paused",
        SessionState::Ended => "ended",
        SessionState::Stale => "stale",
        SessionState::Abandoned => "abandoned",
    }
}

fn session_state_from_sql(s: &str) -> Result<SessionState, CarryCtxError> {
    match s {
        "active" => Ok(SessionState::Active),
        "paused" => Ok(SessionState::Paused),
        "ended" => Ok(SessionState::Ended),
        "stale" => Ok(SessionState::Stale),
        "abandoned" => Ok(SessionState::Abandoned),
        other => Err(CarryCtxError::database_error(format!(
            "Unknown session state: {other}"
        ))),
    }
}

fn progress_type_to_sql(s: &ProgressType) -> &'static str {
    match s {
        ProgressType::Todo => "todo",
        ProgressType::Blocker => "blocker",
        ProgressType::Risk => "risk",
        ProgressType::Note => "note",
    }
}

fn progress_type_from_sql(s: &str) -> Result<ProgressType, CarryCtxError> {
    match s {
        "todo" => Ok(ProgressType::Todo),
        "blocker" => Ok(ProgressType::Blocker),
        "risk" => Ok(ProgressType::Risk),
        "note" => Ok(ProgressType::Note),
        other => Err(CarryCtxError::database_error(format!(
            "Unknown progress type: {other}"
        ))),
    }
}

fn progress_status_to_sql(s: &ProgressStatus) -> &'static str {
    match s {
        ProgressStatus::Open => "open",
        ProgressStatus::Completed => "completed",
        ProgressStatus::Removed => "removed",
    }
}

fn progress_status_from_sql(s: &str) -> Result<ProgressStatus, CarryCtxError> {
    match s {
        "open" => Ok(ProgressStatus::Open),
        "completed" => Ok(ProgressStatus::Completed),
        "removed" => Ok(ProgressStatus::Removed),
        other => Err(CarryCtxError::database_error(format!(
            "Unknown progress status: {other}"
        ))),
    }
}

fn handoff_status_from_sql(s: &str) -> Result<HandoffStatus, CarryCtxError> {
    match s {
        "pending" => Ok(HandoffStatus::Open),
        "accepted" => Ok(HandoffStatus::Accepted),
        "declined" => Ok(HandoffStatus::Rejected),
        "expired" | "closed" => Ok(HandoffStatus::Closed),
        other => Err(CarryCtxError::database_error(format!(
            "Unknown handoff status: {other}"
        ))),
    }
}

fn handoff_status_to_sql(s: &HandoffStatus) -> &'static str {
    match s {
        HandoffStatus::Open => "pending",
        HandoffStatus::Accepted => "accepted",
        HandoffStatus::Rejected => "declined",
        HandoffStatus::Closed => "closed",
    }
}

fn dependency_kind_from_sql(s: &str) -> Result<DependencyKind, CarryCtxError> {
    match s {
        "strong" => Ok(DependencyKind::Strong),
        "informational" => Ok(DependencyKind::Informational),
        other => Err(CarryCtxError::database_error(format!(
            "Unknown dependency kind: {other}"
        ))),
    }
}

fn dependency_kind_to_sql(s: &DependencyKind) -> &'static str {
    match s {
        DependencyKind::Strong => "strong",
        DependencyKind::Informational => "informational",
    }
}

// ── Helpers ────────────────────────────────────────────────────────────

fn db_err(e: rusqlite::Error) -> CarryCtxError {
    CarryCtxError::database_error(format!("SQLite error: {e}")).with_source(e)
}

fn json_vec_or_default(s: Option<String>) -> Vec<String> {
    match s {
        Some(val) => serde_json::from_str(&val).unwrap_or_default(),
        None => Vec::new(),
    }
}

fn json_vec_to_string(v: &[String]) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "[]".into())
}

#[allow(dead_code)]
fn opt_i64_to_string(v: Option<i64>) -> Option<String> {
    v.map(|v| v.to_string())
}

// ── Agent Repository ───────────────────────────────────────────────────

pub struct SqliteAgentRepository<'a> {
    conn: &'a Connection,
}

impl<'a> SqliteAgentRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    fn row_to_agent(row: &Row) -> rusqlite::Result<Agent> {
        let status_str: String = row.get("status")?;
        Ok(Agent {
            id: row.get("id")?,
            project_id: row.get("project_id")?,
            name: row.get("name")?,
            provider: row.get("provider")?,
            role: row.get("role")?,
            metadata: row
                .get::<_, String>("metadata_json")
                .map(|s| serde_json::from_str(&s).unwrap_or(serde_json::Value::Null))?,
            status: agent_status_from_sql(&status_str)
                .unwrap_or(crate::domain::agent::AgentStatus::Active),
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
            last_active_at: row.get("last_active_at")?,
        })
    }
}

impl AgentRepository for SqliteAgentRepository<'_> {
    fn register(&self, input: &NewAgent, now: &str) -> Result<Agent, CarryCtxError> {
        let metadata_str = serde_json::to_string(&input.metadata).unwrap_or_else(|_| "{}".into());
        self.conn
            .execute(
                "INSERT INTO agents (id, project_id, name, provider, role, status, metadata_json, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'active', ?6, ?7, ?7)",
                params![
                    input.id,
                    input.project_id,
                    input.name,
                    input.provider,
                    input.role,
                    metadata_str,
                    now
                ],
            )
            .map_err(|e| {
                if is_unique_violation(&e) {
                    CarryCtxError::state_conflict(format!(
                        "Agent '{}' already exists in project",
                        input.name
                    ))
                    .with_source(e)
                } else {
                    db_err(e)
                }
            })?;
        self.find_by_id(&input.project_id, &input.id)
            .map(|opt| opt.expect("just inserted"))
    }

    fn list(&self, filter: &AgentFilter) -> Result<Vec<Agent>, CarryCtxError> {
        let mut sql = "SELECT * FROM agents WHERE project_id = ?1".to_string();
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> =
            vec![Box::new(filter.project_id.clone())];
        if let Some(ref status) = filter.status {
            sql.push_str(" AND status = ?2");
            param_values.push(Box::new(agent_status_to_sql(status).to_string()));
        }
        sql.push_str(" ORDER BY name");
        let mut stmt = self.conn.prepare(&sql).map_err(db_err)?;
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();
        let rows = stmt
            .query_map(param_refs.as_slice(), Self::row_to_agent)
            .map_err(db_err)?;
        let mut agents = Vec::new();
        for row in rows {
            agents.push(row.map_err(db_err)?);
        }
        Ok(agents)
    }

    fn find_by_name(&self, project_id: &str, name: &str) -> Result<Option<Agent>, CarryCtxError> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM agents WHERE project_id = ?1 AND name = ?2")
            .map_err(db_err)?;
        let mut rows = stmt
            .query_map(params![project_id, name], Self::row_to_agent)
            .map_err(db_err)?;
        match rows.next() {
            Some(Ok(agent)) => Ok(Some(agent)),
            Some(Err(e)) => Err(db_err(e)),
            None => Ok(None),
        }
    }

    fn find_by_id(&self, project_id: &str, id: &str) -> Result<Option<Agent>, CarryCtxError> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM agents WHERE project_id = ?1 AND id = ?2")
            .map_err(db_err)?;
        let mut rows = stmt
            .query_map(params![project_id, id], Self::row_to_agent)
            .map_err(db_err)?;
        match rows.next() {
            Some(Ok(agent)) => Ok(Some(agent)),
            Some(Err(e)) => Err(db_err(e)),
            None => Ok(None),
        }
    }

    fn rename(
        &self,
        id: &str,
        project_id: &str,
        new_name: &str,
        now: &str,
    ) -> Result<Agent, CarryCtxError> {
        let affected = self
            .conn
            .execute(
                "UPDATE agents SET name = ?1, updated_at = ?2 WHERE id = ?3 AND project_id = ?4",
                params![new_name, now, id, project_id],
            )
            .map_err(db_err)?;
        if affected == 0 {
            return Err(CarryCtxError::resource_not_found(format!(
                "Agent {id} not found in project {project_id}"
            )));
        }
        self.find_by_id(project_id, id)
            .map(|opt| opt.expect("just updated"))
    }

    fn deactivate(&self, id: &str, project_id: &str, now: &str) -> Result<Agent, CarryCtxError> {
        let affected = self
            .conn
            .execute(
                "UPDATE agents SET status = 'deactivated', updated_at = ?1 WHERE id = ?2 AND project_id = ?3",
                params![now, id, project_id],
            )
            .map_err(db_err)?;
        if affected == 0 {
            return Err(CarryCtxError::resource_not_found(format!(
                "Agent {id} not found in project {project_id}"
            )));
        }
        self.find_by_id(project_id, id)
            .map(|opt| opt.expect("just updated"))
    }

    fn has_nonterminal_tasks(
        &self,
        project_id: &str,
        agent_id: &str,
    ) -> Result<bool, CarryCtxError> {
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM tasks WHERE project_id = ?1 AND owner_agent_id = ?2 AND status NOT IN ('completed', 'cancelled')",
                params![project_id, agent_id],
                |row| row.get(0),
            )
            .map_err(db_err)?;
        Ok(count > 0)
    }
}

// ── Task Repository ────────────────────────────────────────────────────

pub struct SqliteTaskRepository<'a> {
    conn: &'a Connection,
}

impl<'a> SqliteTaskRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    fn row_to_task(row: &Row) -> rusqlite::Result<TaskRecord> {
        let status_str: String = row.get("status")?;
        let priority_str: String = row.get("priority")?;
        Ok(TaskRecord {
            id: row.get("id")?,
            display_id: row.get("display_id")?,
            project_id: row.get("project_id")?,
            title: row.get("title")?,
            description: row.get("description")?,
            status: task_status_from_sql(&status_str).unwrap_or(TaskStatus::Planned),
            priority: task_priority_from_sql(&priority_str).unwrap_or(TaskPriority::Normal),
            owner_agent_id: row.get("owner_agent_id")?,
            parent_task_id: row.get("parent_task_id")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
            started_at: row.get("started_at")?,
            completed_at: row.get("completed_at")?,
        })
    }
}

impl TaskRepository for SqliteTaskRepository<'_> {
    fn allocate_display_id(&self, project_id: &str, prefix: &str) -> Result<u32, CarryCtxError> {
        let kind = format!("display_id_{prefix}");
        let affected = self
            .conn
            .execute(
                "INSERT INTO sequences (project_id, kind, next_value) VALUES (?1, ?2, 2)
                 ON CONFLICT(project_id, kind) DO UPDATE SET next_value = next_value + 1",
                params![project_id, kind],
            )
            .map_err(db_err)?;
        if affected > 0 {
            // First insert or increment — read back the value
            let val: i64 = self
                .conn
                .query_row(
                    "SELECT next_value - 1 FROM sequences WHERE project_id = ?1 AND kind = ?2",
                    params![project_id, kind],
                    |row| row.get(0),
                )
                .map_err(db_err)?;
            Ok(val as u32)
        } else {
            Err(CarryCtxError::database_error(
                "Failed to allocate display id",
            ))
        }
    }

    fn create(&self, input: &NewTask, now: &str) -> Result<TaskRecord, CarryCtxError> {
        let status_str = task_status_to_sql(&input.status);
        let priority_str = task_priority_to_sql(&input.priority);
        let metadata_str = "{}";
        self.conn
            .execute(
                "INSERT INTO tasks (id, project_id, display_id, title, description, status, priority, owner_agent_id, parent_task_id, metadata_json, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
                params![
                    input.id,
                    input.project_id,
                    input.display_id,
                    input.title,
                    input.description,
                    status_str,
                    priority_str,
                    input.owner_agent_id,
                    input.parent_task_id,
                    metadata_str,
                    now,
                ],
            )
            .map_err(db_err)?;
        self.find_by_id(&input.project_id, &input.id)
            .map(|opt| opt.expect("just inserted"))
    }

    fn find_by_id(&self, project_id: &str, id: &str) -> Result<Option<TaskRecord>, CarryCtxError> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM tasks WHERE project_id = ?1 AND id = ?2")
            .map_err(db_err)?;
        let mut rows = stmt
            .query_map(params![project_id, id], Self::row_to_task)
            .map_err(db_err)?;
        match rows.next() {
            Some(Ok(task)) => Ok(Some(task)),
            Some(Err(e)) => Err(db_err(e)),
            None => Ok(None),
        }
    }

    fn find_by_display_id(
        &self,
        project_id: &str,
        display_id: &str,
    ) -> Result<Option<TaskRecord>, CarryCtxError> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM tasks WHERE project_id = ?1 AND display_id = ?2")
            .map_err(db_err)?;
        let mut rows = stmt
            .query_map(params![project_id, display_id], Self::row_to_task)
            .map_err(db_err)?;
        match rows.next() {
            Some(Ok(task)) => Ok(Some(task)),
            Some(Err(e)) => Err(db_err(e)),
            None => Ok(None),
        }
    }

    fn list(&self, filter: &TaskFilter) -> Result<Vec<TaskRecord>, CarryCtxError> {
        let mut sql = "SELECT * FROM tasks WHERE project_id = ?1".to_string();
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> =
            vec![Box::new(filter.project_id.clone())];
        let mut idx = 2;

        if let Some(ref status) = filter.status {
            sql.push_str(&format!(" AND status = ?{idx}"));
            param_values.push(Box::new(task_status_to_sql(status).to_string()));
            idx += 1;
        }
        if let Some(ref owner) = filter.owner_agent_id {
            sql.push_str(&format!(" AND owner_agent_id = ?{idx}"));
            param_values.push(Box::new(owner.clone()));
            idx += 1;
        }
        if filter.ready {
            sql.push_str(" AND status IN ('planned', 'ready')");
        }
        if filter.blocked {
            sql.push_str(" AND status = 'blocked'");
        }
        if let Some(ref mine) = filter.mine {
            sql.push_str(&format!(" AND owner_agent_id = ?{idx}"));
            param_values.push(Box::new(mine.clone()));
        }
        sql.push_str(" ORDER BY created_at DESC");

        let mut stmt = self.conn.prepare(&sql).map_err(db_err)?;
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();
        let rows = stmt
            .query_map(param_refs.as_slice(), Self::row_to_task)
            .map_err(db_err)?;
        let mut tasks = Vec::new();
        for row in rows {
            tasks.push(row.map_err(db_err)?);
        }
        Ok(tasks)
    }

    fn update_status(
        &self,
        id: &str,
        project_id: &str,
        status: TaskStatus,
        owner_agent_id: Option<String>,
        now: &str,
    ) -> Result<TaskRecord, CarryCtxError> {
        let status_str = task_status_to_sql(&status);
        let affected = self
            .conn
            .execute(
                "UPDATE tasks SET \
                 status = ?1, \
                 owner_agent_id = ?2, \
                 updated_at = ?3, \
                 started_at = CASE WHEN ?1 = 'in_progress' THEN COALESCE(started_at, ?3) ELSE started_at END, \
                 completed_at = CASE WHEN ?1 = 'completed' THEN ?3 ELSE NULL END \
                 WHERE id = ?4 AND project_id = ?5",
                params![status_str, owner_agent_id, now, id, project_id],
            )
            .map_err(db_err)?;
        if affected == 0 {
            return Err(CarryCtxError::resource_not_found(format!(
                "Task {id} not found in project {project_id}"
            )));
        }
        self.find_by_id(project_id, id)
            .map(|opt| opt.expect("just updated"))
    }

    fn count_open_progress(&self, project_id: &str, task_id: &str) -> Result<u64, CarryCtxError> {
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM progress_items WHERE project_id = ?1 AND task_id = ?2 AND status = 'open'",
                params![project_id, task_id],
                |row| row.get(0),
            )
            .map_err(db_err)?;
        Ok(count as u64)
    }

    fn has_active_session(&self, project_id: &str, task_id: &str) -> Result<bool, CarryCtxError> {
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE project_id = ?1 AND task_id = ?2 AND state = 'active'",
                params![project_id, task_id],
                |row| row.get(0),
            )
            .map_err(db_err)?;
        Ok(count > 0)
    }

    fn list_incomplete_strong_dependencies(
        &self,
        project_id: &str,
        task_id: &str,
    ) -> Result<Vec<String>, CarryCtxError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT td.prerequisite_task_id
                 FROM task_dependencies td
                 JOIN tasks t ON t.id = td.prerequisite_task_id
                 WHERE td.project_id = ?1 AND td.task_id = ?2 AND td.kind = 'strong'
                   AND t.status NOT IN ('completed', 'cancelled')",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![project_id, task_id], |row| row.get(0))
            .map_err(db_err)?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(row.map_err(db_err)?);
        }
        Ok(ids)
    }

    fn edit(
        &self,
        id: &str,
        project_id: &str,
        title: &str,
        priority: TaskPriority,
        now: &str,
    ) -> Result<TaskRecord, CarryCtxError> {
        let priority_str = task_priority_to_sql(&priority);
        let affected = self
            .conn
            .execute(
                "UPDATE tasks SET title = ?1, priority = ?2, updated_at = ?3 WHERE id = ?4 AND project_id = ?5",
                params![title, priority_str, now, id, project_id],
            )
            .map_err(db_err)?;
        if affected == 0 {
            return Err(CarryCtxError::resource_not_found(format!(
                "Task {id} not found in project {project_id}"
            )));
        }
        self.find_by_id(project_id, id)
            .map(|opt| opt.expect("just updated"))
    }
}

// ── Session Repository ─────────────────────────────────────────────────

pub struct SqliteSessionRepository<'a> {
    conn: &'a Connection,
}

impl<'a> SqliteSessionRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    fn row_to_session(row: &Row) -> rusqlite::Result<SessionRecord> {
        let state_str: String = row.get("state")?;
        Ok(SessionRecord {
            id: row.get("id")?,
            project_id: row.get("project_id")?,
            agent_id: row.get("agent_id")?,
            task_id: row.get("task_id")?,
            worktree_id: row.get("worktree_id")?,
            state: session_state_from_sql(&state_str).unwrap_or(SessionState::Active),
            provider: row.get("provider")?,
            metadata: row
                .get::<_, String>("metadata_json")
                .map(|s| serde_json::from_str(&s).unwrap_or(serde_json::Value::Null))?,
            branch: row.get("branch")?,
            head: row.get("head")?,
            cwd: row.get("working_directory")?,
            summary: row.get("summary")?,
            created_at: row.get("started_at")?,
            updated_at: row.get("updated_at")?,
            last_activity_at: row.get("last_activity_at")?,
        })
    }
}

impl SessionRepository for SqliteSessionRepository<'_> {
    fn create(&self, input: &NewSession, now: &str) -> Result<SessionRecord, CarryCtxError> {
        let state_str = session_state_to_sql(&SessionState::Active);
        let provider = input.provider.as_deref().unwrap_or("unknown");
        let cwd = input.cwd.as_deref().unwrap_or("");
        let metadata_str = "{}";
        self.conn
            .execute(
                "INSERT INTO sessions (id, project_id, agent_id, task_id, worktree_id, state, provider, working_directory, branch, head, metadata_json, started_at, last_activity_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12, ?12)",
                params![
                    input.id, input.project_id, input.agent_id, input.task_id, input.worktree_id,
                    state_str, provider, cwd, input.branch, input.head, metadata_str, now,
                ],
            )
            .map_err(db_err)?;
        self.find_by_id(&input.project_id, &input.id)
            .map(|opt| opt.expect("just inserted"))
    }

    fn find_by_id(
        &self,
        project_id: &str,
        id: &str,
    ) -> Result<Option<SessionRecord>, CarryCtxError> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM sessions WHERE project_id = ?1 AND id = ?2")
            .map_err(db_err)?;
        let mut rows = stmt
            .query_map(params![project_id, id], Self::row_to_session)
            .map_err(db_err)?;
        match rows.next() {
            Some(Ok(s)) => Ok(Some(s)),
            Some(Err(e)) => Err(db_err(e)),
            None => Ok(None),
        }
    }

    fn list(&self, project_id: &str) -> Result<Vec<SessionRecord>, CarryCtxError> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM sessions WHERE project_id = ?1 ORDER BY started_at DESC")
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![project_id], Self::row_to_session)
            .map_err(db_err)?;
        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(row.map_err(db_err)?);
        }
        Ok(sessions)
    }

    fn find_active(
        &self,
        project_id: &str,
        agent_id: &str,
        worktree_id: Option<&str>,
    ) -> Result<Vec<SessionRecord>, CarryCtxError> {
        let mut sql =
            "SELECT * FROM sessions WHERE project_id = ?1 AND agent_id = ?2 AND state = 'active'"
                .to_string();
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = vec![
            Box::new(project_id.to_string()),
            Box::new(agent_id.to_string()),
        ];
        if let Some(wt) = worktree_id {
            sql.push_str(" AND worktree_id = ?3");
            param_values.push(Box::new(wt.to_string()));
        }
        sql.push_str(" ORDER BY last_activity_at DESC");
        let mut stmt = self.conn.prepare(&sql).map_err(db_err)?;
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();
        let rows = stmt
            .query_map(param_refs.as_slice(), Self::row_to_session)
            .map_err(db_err)?;
        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(row.map_err(db_err)?);
        }
        Ok(sessions)
    }

    fn update_state(
        &self,
        id: &str,
        project_id: &str,
        state: SessionState,
        now: &str,
        summary: Option<&str>,
    ) -> Result<SessionRecord, CarryCtxError> {
        let state_str = session_state_to_sql(&state);
        let affected = self
            .conn
            .execute(
                "UPDATE sessions SET state = ?1, summary = ?2, updated_at = ?3 WHERE id = ?4 AND project_id = ?5",
                params![state_str, summary, now, id, project_id],
            )
            .map_err(db_err)?;
        if affected == 0 {
            return Err(CarryCtxError::resource_not_found(format!(
                "Session {id} not found in project {project_id}"
            )));
        }
        self.find_by_id(project_id, id)
            .map(|opt| opt.expect("just updated"))
    }

    fn touch_activity(&self, id: &str, project_id: &str, now: &str) -> Result<(), CarryCtxError> {
        let affected = self
            .conn
            .execute(
                "UPDATE sessions SET last_activity_at = ?1, updated_at = ?1 WHERE id = ?2 AND project_id = ?3",
                params![now, id, project_id],
            )
            .map_err(db_err)?;
        if affected == 0 {
            return Err(CarryCtxError::resource_not_found(format!(
                "Session {id} not found in project {project_id}"
            )));
        }
        Ok(())
    }

    fn mark_overdue_stale(
        &self,
        project_id: &str,
        stale_before: &str,
        now: &str,
    ) -> Result<u64, CarryCtxError> {
        let affected = self
            .conn
            .execute(
                "UPDATE sessions SET state = 'stale', updated_at = ?1
                 WHERE project_id = ?2 AND state = 'active' AND last_activity_at < ?3",
                params![now, project_id, stale_before],
            )
            .map_err(db_err)?;
        Ok(affected as u64)
    }
}

// ── Progress Repository ────────────────────────────────────────────────

pub struct SqliteProgressRepository<'a> {
    conn: &'a Connection,
}

impl<'a> SqliteProgressRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    fn row_to_item(row: &Row) -> rusqlite::Result<ProgressItemRecord> {
        let type_str: String = row.get("type")?;
        let status_str: String = row.get("status")?;
        Ok(ProgressItemRecord {
            id: row.get("id")?,
            display_id: row.get("display_id")?,
            project_id: row.get("project_id")?,
            task_id: row.get("task_id")?,
            source_session_id: row.get("source_session_id")?,
            item_type: progress_type_from_sql(&type_str).unwrap_or(ProgressType::Todo),
            status: progress_status_from_sql(&status_str).unwrap_or(ProgressStatus::Open),
            content: row.get("content")?,
            position: row.get("position")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
            completed_at: row.get("completed_at")?,
            removed_at: row.get("removed_at")?,
        })
    }
}

impl ProgressRepository for SqliteProgressRepository<'_> {
    fn allocate_display_id(&self, project_id: &str) -> Result<u32, CarryCtxError> {
        let kind = "display_id_progress".to_string();
        let affected = self
            .conn
            .execute(
                "INSERT INTO sequences (project_id, kind, next_value) VALUES (?1, ?2, 2)
                 ON CONFLICT(project_id, kind) DO UPDATE SET next_value = next_value + 1",
                params![project_id, kind],
            )
            .map_err(db_err)?;
        if affected > 0 {
            let val: i64 = self
                .conn
                .query_row(
                    "SELECT next_value - 1 FROM sequences WHERE project_id = ?1 AND kind = ?2",
                    params![project_id, kind],
                    |row| row.get(0),
                )
                .map_err(db_err)?;
            Ok(val as u32)
        } else {
            Err(CarryCtxError::database_error(
                "Failed to allocate progress display id",
            ))
        }
    }

    fn get_next_position(&self, project_id: &str, task_id: &str) -> Result<u32, CarryCtxError> {
        let max_pos: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(position), -1) FROM progress_items WHERE project_id = ?1 AND task_id = ?2",
                params![project_id, task_id],
                |row| row.get(0),
            )
            .map_err(db_err)?;
        Ok((max_pos + 1) as u32)
    }

    fn create(
        &self,
        input: &NewProgressItem,
        now: &str,
    ) -> Result<ProgressItemRecord, CarryCtxError> {
        let type_str = progress_type_to_sql(&input.item_type);
        self.conn
            .execute(
                "INSERT INTO progress_items (id, project_id, display_id, task_id, source_session_id, type, status, content, position, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'open', ?7, ?8, ?9, ?9)",
                params![
                    input.id, input.project_id, input.display_id, input.task_id,
                    input.source_session_id, type_str, input.content, input.position, now,
                ],
            )
            .map_err(db_err)?;
        self.find_by_id(&input.project_id, &input.id)
            .map(|opt| opt.expect("just inserted"))
    }

    fn find_by_id(
        &self,
        project_id: &str,
        id: &str,
    ) -> Result<Option<ProgressItemRecord>, CarryCtxError> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM progress_items WHERE project_id = ?1 AND id = ?2")
            .map_err(db_err)?;
        let mut rows = stmt
            .query_map(params![project_id, id], Self::row_to_item)
            .map_err(db_err)?;
        match rows.next() {
            Some(Ok(item)) => Ok(Some(item)),
            Some(Err(e)) => Err(db_err(e)),
            None => Ok(None),
        }
    }

    fn find_by_display_id(
        &self,
        project_id: &str,
        display_id: &str,
    ) -> Result<Option<ProgressItemRecord>, CarryCtxError> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM progress_items WHERE project_id = ?1 AND display_id = ?2")
            .map_err(db_err)?;
        let mut rows = stmt
            .query_map(params![project_id, display_id], Self::row_to_item)
            .map_err(db_err)?;
        match rows.next() {
            Some(Ok(item)) => Ok(Some(item)),
            Some(Err(e)) => Err(db_err(e)),
            None => Ok(None),
        }
    }

    fn list(&self, filter: &ProgressFilter) -> Result<Vec<ProgressItemRecord>, CarryCtxError> {
        let mut sql =
            "SELECT * FROM progress_items WHERE project_id = ?1 AND task_id = ?2".to_string();
        if !filter.include_removed {
            sql.push_str(" AND status != 'removed'");
        }
        sql.push_str(" ORDER BY position, id");
        let mut stmt = self.conn.prepare(&sql).map_err(db_err)?;
        let rows = stmt
            .query_map(
                params![filter.project_id, filter.task_id],
                Self::row_to_item,
            )
            .map_err(db_err)?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row.map_err(db_err)?);
        }
        Ok(items)
    }

    fn edit(
        &self,
        id: &str,
        project_id: &str,
        content: &str,
        now: &str,
    ) -> Result<ProgressItemRecord, CarryCtxError> {
        let affected = self
            .conn
            .execute(
                "UPDATE progress_items SET content = ?1, updated_at = ?2 WHERE id = ?3 AND project_id = ?4",
                params![content, now, id, project_id],
            )
            .map_err(db_err)?;
        if affected == 0 {
            return Err(CarryCtxError::resource_not_found(format!(
                "Progress item {id} not found in project {project_id}"
            )));
        }
        self.find_by_id(project_id, id)
            .map(|opt| opt.expect("just updated"))
    }

    fn update_status(
        &self,
        id: &str,
        project_id: &str,
        status: ProgressStatus,
        now: &str,
    ) -> Result<ProgressItemRecord, CarryCtxError> {
        let status_str = progress_status_to_sql(&status);
        let (completed_at, removed_at) = match status {
            ProgressStatus::Completed => (Some(now), None),
            ProgressStatus::Removed => (None, Some(now)),
            ProgressStatus::Open => (None::<&str>, None),
        };
        let affected = self
            .conn
            .execute(
                "UPDATE progress_items SET status = ?1, completed_at = ?2, removed_at = ?3, updated_at = ?4 WHERE id = ?5 AND project_id = ?6",
                params![status_str, completed_at, removed_at, now, id, project_id],
            )
            .map_err(db_err)?;
        if affected == 0 {
            return Err(CarryCtxError::resource_not_found(format!(
                "Progress item {id} not found in project {project_id}"
            )));
        }
        self.find_by_id(project_id, id)
            .map(|opt| opt.expect("just updated"))
    }

    fn reorder(
        &self,
        project_id: &str,
        task_id: &str,
        ordered_ids: &[String],
    ) -> Result<(), CarryCtxError> {
        if ordered_ids.is_empty() {
            return Ok(());
        }
        let mut in_list = String::new();
        let mut sql = String::from("UPDATE progress_items SET position = CASE id");
        for (i, _id) in ordered_ids.iter().enumerate() {
            sql.push_str(&format!(" WHEN ?{} THEN ?{}", i * 2 + 1, i * 2 + 2));
            if i > 0 {
                in_list.push_str(", ");
            }
            in_list.push_str(&format!("?{}", ordered_ids.len() * 2 + 1 + i));
        }
        sql.push_str(" END WHERE id IN (");
        sql.push_str(&in_list);
        sql.push_str(") AND project_id = ?");
        sql.push_str(&(ordered_ids.len() * 3 + 1).to_string());
        sql.push_str(" AND task_id = ?");
        sql.push_str(&(ordered_ids.len() * 3 + 2).to_string());

        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        for (i, id) in ordered_ids.iter().enumerate() {
            param_values.push(Box::new(id.clone()));
            param_values.push(Box::new(i as i64));
        }
        for id in ordered_ids.iter() {
            param_values.push(Box::new(id.clone()));
        }
        param_values.push(Box::new(project_id.to_string()));
        param_values.push(Box::new(task_id.to_string()));

        let mut stmt = self.conn.prepare(&sql).map_err(db_err)?;
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();
        stmt.execute(param_refs.as_slice()).map_err(db_err)?;
        Ok(())
    }
}

// ── Dependency Repository ──────────────────────────────────────────────

pub struct SqliteDependencyRepository<'a> {
    conn: &'a Connection,
}

impl<'a> SqliteDependencyRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
}

impl DependencyRepository for SqliteDependencyRepository<'_> {
    fn add(
        &self,
        project_id: &str,
        task_id: &str,
        prerequisite_id: &str,
        kind: DependencyKind,
    ) -> Result<(), CarryCtxError> {
        let id = ulid::Ulid::generate().to_string();
        let kind_str = dependency_kind_to_sql(&kind);
        let now = chrono::Utc::now().to_rfc3339();
        self.conn
            .execute(
                "INSERT INTO task_dependencies (id, project_id, task_id, prerequisite_task_id, kind, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, project_id, task_id, prerequisite_id, kind_str, now],
            )
            .map_err(|e| {
                if is_unique_violation(&e) {
                    CarryCtxError::state_conflict(
                        "This dependency already exists",
                    )
                    .with_source(e)
                } else if is_foreign_key_violation(&e) {
                    CarryCtxError::resource_not_found(
                        "Task or prerequisite not found",
                    )
                    .with_source(e)
                } else {
                    db_err(e)
                }
            })?;
        Ok(())
    }

    fn remove(
        &self,
        project_id: &str,
        task_id: &str,
        prerequisite_id: &str,
    ) -> Result<(), CarryCtxError> {
        let affected = self
            .conn
            .execute(
                "DELETE FROM task_dependencies WHERE project_id = ?1 AND task_id = ?2 AND prerequisite_task_id = ?3",
                params![project_id, task_id, prerequisite_id],
            )
            .map_err(db_err)?;
        if affected == 0 {
            return Err(CarryCtxError::resource_not_found("Dependency not found"));
        }
        Ok(())
    }

    fn list_for_task(
        &self,
        project_id: &str,
        task_id: &str,
    ) -> Result<Vec<DependencyEdge>, CarryCtxError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT task_id, prerequisite_task_id, kind FROM task_dependencies WHERE project_id = ?1 AND task_id = ?2",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![project_id, task_id], |row| {
                let kind_str: String = row.get(2)?;
                Ok(DependencyEdge {
                    task_id: row.get(0)?,
                    prerequisite_id: row.get(1)?,
                    kind: dependency_kind_from_sql(&kind_str).unwrap_or(DependencyKind::Strong),
                })
            })
            .map_err(db_err)?;
        let mut edges = Vec::new();
        for row in rows {
            edges.push(row.map_err(db_err)?);
        }
        Ok(edges)
    }

    fn list_all_for_project(&self, project_id: &str) -> Result<Vec<DependencyEdge>, CarryCtxError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT task_id, prerequisite_task_id, kind FROM task_dependencies WHERE project_id = ?1",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![project_id], |row| {
                let kind_str: String = row.get(2)?;
                Ok(DependencyEdge {
                    task_id: row.get(0)?,
                    prerequisite_id: row.get(1)?,
                    kind: dependency_kind_from_sql(&kind_str).unwrap_or(DependencyKind::Strong),
                })
            })
            .map_err(db_err)?;
        let mut edges = Vec::new();
        for row in rows {
            edges.push(row.map_err(db_err)?);
        }
        Ok(edges)
    }

    fn find_edge(
        &self,
        project_id: &str,
        task_id: &str,
        prerequisite_id: &str,
    ) -> Result<Option<DependencyEdge>, CarryCtxError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT task_id, prerequisite_task_id, kind FROM task_dependencies WHERE project_id = ?1 AND task_id = ?2 AND prerequisite_task_id = ?3",
            )
            .map_err(db_err)?;
        let mut rows = stmt
            .query_map(params![project_id, task_id, prerequisite_id], |row| {
                let kind_str: String = row.get(2)?;
                Ok(DependencyEdge {
                    task_id: row.get(0)?,
                    prerequisite_id: row.get(1)?,
                    kind: dependency_kind_from_sql(&kind_str).unwrap_or(DependencyKind::Strong),
                })
            })
            .map_err(db_err)?;
        match rows.next() {
            Some(Ok(edge)) => Ok(Some(edge)),
            Some(Err(e)) => Err(db_err(e)),
            None => Ok(None),
        }
    }
}

// ── Worktree Repository ───────────────────────────────────────────────

pub struct SqliteWorktreeRepository<'a> {
    conn: &'a Connection,
}

impl<'a> SqliteWorktreeRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    fn row_to_worktree(row: &Row) -> rusqlite::Result<WorktreeRecord> {
        Ok(WorktreeRecord {
            id: row.get("id")?,
            project_id: row.get("project_id")?,
            path: row.get("normalized_path")?,
            branch: row.get("branch")?,
            head: row.get("head")?,
            task_id: row.get("task_id")?,
            created_at: row.get("bound_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

impl WorktreeRepository for SqliteWorktreeRepository<'_> {
    fn upsert(&self, input: &NewWorktree, now: &str) -> Result<WorktreeRecord, CarryCtxError> {
        self.conn
            .execute(
                "INSERT INTO worktrees (id, project_id, task_id, normalized_path, git_common_dir, branch, head, bound_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, '', ?5, ?6, ?7, ?7)
                 ON CONFLICT(project_id, normalized_path) DO UPDATE SET
                   task_id = excluded.task_id,
                   branch = excluded.branch,
                   head = excluded.head,
                   updated_at = excluded.updated_at",
                params![
                    input.id, input.project_id, input.task_id, input.path,
                    input.branch, input.head, now,
                ],
            )
            .map_err(db_err)?;
        self.find_by_path(&input.project_id, &input.path)
            .map(|opt| opt.expect("just upserted"))
    }

    fn find_by_id(
        &self,
        project_id: &str,
        id: &str,
    ) -> Result<Option<WorktreeRecord>, CarryCtxError> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM worktrees WHERE project_id = ?1 AND id = ?2")
            .map_err(db_err)?;
        let mut rows = stmt
            .query_map(params![project_id, id], Self::row_to_worktree)
            .map_err(db_err)?;
        match rows.next() {
            Some(Ok(wt)) => Ok(Some(wt)),
            Some(Err(e)) => Err(db_err(e)),
            None => Ok(None),
        }
    }

    fn find_by_path(
        &self,
        project_id: &str,
        path: &str,
    ) -> Result<Option<WorktreeRecord>, CarryCtxError> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM worktrees WHERE project_id = ?1 AND normalized_path = ?2")
            .map_err(db_err)?;
        let mut rows = stmt
            .query_map(params![project_id, path], Self::row_to_worktree)
            .map_err(db_err)?;
        match rows.next() {
            Some(Ok(wt)) => Ok(Some(wt)),
            Some(Err(e)) => Err(db_err(e)),
            None => Ok(None),
        }
    }

    fn find_by_task_id(
        &self,
        project_id: &str,
        task_id: &str,
    ) -> Result<Option<WorktreeRecord>, CarryCtxError> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM worktrees WHERE project_id = ?1 AND task_id = ?2")
            .map_err(db_err)?;
        let mut rows = stmt
            .query_map(params![project_id, task_id], Self::row_to_worktree)
            .map_err(db_err)?;
        match rows.next() {
            Some(Ok(wt)) => Ok(Some(wt)),
            Some(Err(e)) => Err(db_err(e)),
            None => Ok(None),
        }
    }

    fn list(&self, project_id: &str) -> Result<Vec<WorktreeRecord>, CarryCtxError> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM worktrees WHERE project_id = ?1 ORDER BY bound_at DESC")
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![project_id], Self::row_to_worktree)
            .map_err(db_err)?;
        let mut trees = Vec::new();
        for row in rows {
            trees.push(row.map_err(db_err)?);
        }
        Ok(trees)
    }

    fn unbind_task(
        &self,
        id: &str,
        project_id: &str,
        now: &str,
    ) -> Result<WorktreeRecord, CarryCtxError> {
        let affected = self
            .conn
            .execute(
                "UPDATE worktrees SET task_id = NULL, updated_at = ?1 WHERE id = ?2 AND project_id = ?3",
                params![now, id, project_id],
            )
            .map_err(db_err)?;
        if affected == 0 {
            return Err(CarryCtxError::resource_not_found(format!(
                "Worktree {id} not found in project {project_id}"
            )));
        }
        self.find_by_id(project_id, id)
            .map(|opt| opt.expect("just updated"))
    }
}

// ── Checkpoint Repository ──────────────────────────────────────────────

pub struct SqliteCheckpointRepository<'a> {
    conn: &'a Connection,
}

impl<'a> SqliteCheckpointRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    fn row_to_checkpoint(row: &Row) -> rusqlite::Result<Checkpoint> {
        Ok(Checkpoint {
            id: row.get("id")?,
            project_id: row.get("project_id")?,
            task_id: row.get("task_id")?,
            session_id: row.get("session_id")?,
            agent_id: row.get("agent_id")?,
            worktree_id: row.get("worktree_id")?,
            branch: row.get("branch")?,
            head: row.get("head")?,
            dirty: row.get::<_, i64>("dirty")? != 0,
            staged_files: json_vec_or_default(row.get::<_, Option<String>>("staged_files_json")?),
            modified_files: json_vec_or_default(
                row.get::<_, Option<String>>("modified_files_json")?,
            ),
            deleted_files: json_vec_or_default(row.get::<_, Option<String>>("deleted_files_json")?),
            renamed_files: serde_json::from_str(
                &row.get::<_, String>("renamed_files_json")
                    .unwrap_or_else(|_| "[]".into()),
            )
            .unwrap_or_default(),
            untracked_files: json_vec_or_default(
                row.get::<_, Option<String>>("untracked_files_json")?,
            ),
            diff_files: row.get("diff_files")?,
            diff_insertions: row.get("diff_insertions")?,
            diff_deletions: row.get("diff_deletions")?,
            done: json_vec_or_default(row.get::<_, Option<String>>("done_items_json")?),
            remaining: json_vec_or_default(row.get::<_, Option<String>>("remaining_items_json")?),
            blockers: json_vec_or_default(row.get::<_, Option<String>>("blockers_json")?),
            risks: json_vec_or_default(row.get::<_, Option<String>>("risks_json")?),
            next_actions: json_vec_or_default(row.get::<_, Option<String>>("next_steps_json")?),
            notes: json_vec_or_default(row.get::<_, Option<String>>("notes_json")?),
            created_at: row.get("created_at")?,
        })
    }
}

impl CheckpointRepository for SqliteCheckpointRepository<'_> {
    fn create(&self, cp: &Checkpoint) -> Result<Checkpoint, CarryCtxError> {
        self.conn
            .execute(
                "INSERT INTO checkpoints (
                    id, project_id, task_id, session_id, worktree_id, agent_id,
                    branch, head, dirty,
                    staged_files_json, modified_files_json, deleted_files_json,
                    renamed_files_json, untracked_files_json,
                    diff_files, diff_insertions, diff_deletions,
                    done_items_json, remaining_items_json, blockers_json,
                    risks_json, next_steps_json, notes_json,
                    created_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                          ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                          ?18, ?19, ?20, ?21, ?22, ?23, ?24)",
                params![
                    cp.id,
                    cp.project_id,
                    cp.task_id,
                    cp.session_id,
                    cp.worktree_id,
                    cp.agent_id,
                    cp.branch,
                    cp.head,
                    cp.dirty as i64,
                    json_vec_to_string(&cp.staged_files),
                    json_vec_to_string(&cp.modified_files),
                    json_vec_to_string(&cp.deleted_files),
                    serde_json::to_string(&cp.renamed_files).unwrap_or_else(|_| "[]".into()),
                    json_vec_to_string(&cp.untracked_files),
                    cp.diff_files,
                    cp.diff_insertions,
                    cp.diff_deletions,
                    json_vec_to_string(&cp.done),
                    json_vec_to_string(&cp.remaining),
                    json_vec_to_string(&cp.blockers),
                    json_vec_to_string(&cp.risks),
                    json_vec_to_string(&cp.next_actions),
                    json_vec_to_string(&cp.notes),
                    cp.created_at,
                ],
            )
            .map_err(db_err)?;
        self.find_by_id(&cp.project_id, &cp.id)
            .map(|opt| opt.expect("just inserted"))
    }

    fn find_by_id(&self, project_id: &str, id: &str) -> Result<Option<Checkpoint>, CarryCtxError> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM checkpoints WHERE project_id = ?1 AND id = ?2")
            .map_err(db_err)?;
        let mut rows = stmt
            .query_map(params![project_id, id], Self::row_to_checkpoint)
            .map_err(db_err)?;
        match rows.next() {
            Some(Ok(cp)) => Ok(Some(cp)),
            Some(Err(e)) => Err(db_err(e)),
            None => Ok(None),
        }
    }

    fn find_latest_for_task(
        &self,
        project_id: &str,
        task_id: &str,
    ) -> Result<Option<Checkpoint>, CarryCtxError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT * FROM checkpoints WHERE project_id = ?1 AND task_id = ?2 ORDER BY created_at DESC LIMIT 1",
            )
            .map_err(db_err)?;
        let mut rows = stmt
            .query_map(params![project_id, task_id], Self::row_to_checkpoint)
            .map_err(db_err)?;
        match rows.next() {
            Some(Ok(cp)) => Ok(Some(cp)),
            Some(Err(e)) => Err(db_err(e)),
            None => Ok(None),
        }
    }

    fn list(
        &self,
        project_id: &str,
        task_id: Option<&str>,
    ) -> Result<Vec<Checkpoint>, CarryCtxError> {
        let mut sql = "SELECT * FROM checkpoints WHERE project_id = ?1".to_string();
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> =
            vec![Box::new(project_id.to_string())];
        if let Some(tid) = task_id {
            sql.push_str(" AND task_id = ?2");
            param_values.push(Box::new(tid.to_string()));
        }
        sql.push_str(" ORDER BY created_at DESC");
        let mut stmt = self.conn.prepare(&sql).map_err(db_err)?;
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();
        let rows = stmt
            .query_map(param_refs.as_slice(), Self::row_to_checkpoint)
            .map_err(db_err)?;
        let mut cps = Vec::new();
        for row in rows {
            cps.push(row.map_err(db_err)?);
        }
        Ok(cps)
    }

    fn correct(&self, correction: &CheckpointCorrection) -> Result<(), CarryCtxError> {
        self.conn
            .execute(
                "INSERT INTO checkpoint_corrections (id, checkpoint_id, project_id,
                    done_items_json, remaining_items_json, blockers_json,
                    risks_json, next_steps_json, notes_json,
                    corrected_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    correction.id,
                    correction.checkpoint_id,
                    "", // project_id — not directly available, but required as NOT NULL
                    correction.done.as_ref().map(|v| json_vec_to_string(v)),
                    correction.remaining.as_ref().map(|v| json_vec_to_string(v)),
                    correction.blockers.as_ref().map(|v| json_vec_to_string(v)),
                    correction.risks.as_ref().map(|v| json_vec_to_string(v)),
                    correction
                        .next_actions
                        .as_ref()
                        .map(|v| json_vec_to_string(v)),
                    correction.notes.as_ref().map(|v| json_vec_to_string(v)),
                    correction.created_at,
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }
}

// ── Event Repository ──────────────────────────────────────────────────

pub struct SqliteEventRepository<'a> {
    conn: &'a Connection,
}

impl<'a> SqliteEventRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
}

impl EventRepository for SqliteEventRepository<'_> {
    fn append(&self, event: &NewEvent) -> Result<EventRecord, CarryCtxError> {
        let payload_str = serde_json::to_string(&event.payload).unwrap_or_else(|_| "{}".into());
        self.conn
            .execute(
                "INSERT INTO events (id, project_id, type, aggregate_type, aggregate_id, payload_json, actor_agent_id, session_id, task_id, occurred_at)
                 VALUES (?1, ?2, ?3, ?3, ?1, ?4, ?5, ?6, ?7, ?8)",
                params![
                    event.id,
                    event.project_id,
                    event.event_type,
                    payload_str,
                    event.actor_agent_id,
                    event.session_id,
                    event.task_id,
                    event.occurred_at,
                ],
            )
            .map_err(|e| CarryCtxError::database_error(format!("EVENTS_APPEND_ERROR: {e:?}")))?;
        self.find_by_id(&event.project_id, &event.id)
            .map(|opt| opt.expect("just inserted"))
    }

    fn find_by_id(&self, project_id: &str, id: &str) -> Result<Option<EventRecord>, CarryCtxError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, project_id, type AS event_type, actor_agent_id, session_id, task_id, payload_json AS payload, occurred_at FROM events WHERE project_id = ?1 AND id = ?2")
            .map_err(db_err)?;
        let mut rows = stmt
            .query_map(params![project_id, id], |row| {
                Ok(EventRecord {
                    id: row.get("id")?,
                    project_id: row.get("project_id")?,
                    event_type: row.get("event_type")?,
                    actor_agent_id: row.get("actor_agent_id")?,
                    session_id: row.get("session_id")?,
                    task_id: row.get("task_id")?,
                    payload: row
                        .get::<_, String>("payload")
                        .map(|s| serde_json::from_str(&s).unwrap_or(serde_json::Value::Null))?,
                    occurred_at: row.get("occurred_at")?,
                })
            })
            .map_err(db_err)?;
        match rows.next() {
            Some(Ok(ev)) => Ok(Some(ev)),
            Some(Err(e)) => Err(db_err(e)),
            None => Ok(None),
        }
    }

    fn list(&self, filter: &EventFilter) -> Result<Vec<EventRecord>, CarryCtxError> {
        let mut sql = String::from(
            "SELECT id, project_id, type AS event_type, actor_agent_id, session_id, task_id, payload_json AS payload, occurred_at FROM events WHERE project_id = ?1",
        );
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> =
            vec![Box::new(filter.project_id.clone())];
        let mut idx = 2;

        if let Some(ref task_id) = filter.task_id {
            sql.push_str(&format!(" AND task_id = ?{idx}"));
            param_values.push(Box::new(task_id.clone()));
            idx += 1;
        }
        if let Some(ref agent_id) = filter.agent_id {
            sql.push_str(&format!(" AND actor_agent_id = ?{idx}"));
            param_values.push(Box::new(agent_id.clone()));
            idx += 1;
        }
        if let Some(ref session_id) = filter.session_id {
            sql.push_str(&format!(" AND session_id = ?{idx}"));
            param_values.push(Box::new(session_id.clone()));
            idx += 1;
        }
        if let Some(ref ev_type) = filter.event_type {
            sql.push_str(&format!(" AND type = ?{idx}"));
            param_values.push(Box::new(ev_type.clone()));
            idx += 1;
        }
        if let Some(ref since) = filter.since {
            sql.push_str(&format!(" AND occurred_at >= ?{idx}"));
            param_values.push(Box::new(since.clone()));
            idx += 1;
        }
        if let Some(ref until) = filter.until {
            sql.push_str(&format!(" AND occurred_at <= ?{idx}"));
            param_values.push(Box::new(until.clone()));
        }
        sql.push_str(" ORDER BY occurred_at DESC");
        if let Some(limit) = filter.limit {
            sql.push_str(&format!(" LIMIT {limit}"));
        }

        let mut stmt = self.conn.prepare(&sql).map_err(db_err)?;
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();
        let rows = stmt
            .query_map(param_refs.as_slice(), |row| {
                Ok(EventRecord {
                    id: row.get("id")?,
                    project_id: row.get("project_id")?,
                    event_type: row.get("event_type")?,
                    actor_agent_id: row.get("actor_agent_id")?,
                    session_id: row.get("session_id")?,
                    task_id: row.get("task_id")?,
                    payload: row
                        .get::<_, String>("payload")
                        .map(|s| serde_json::from_str(&s).unwrap_or(serde_json::Value::Null))?,
                    occurred_at: row.get("occurred_at")?,
                })
            })
            .map_err(db_err)?;
        let mut events = Vec::new();
        for row in rows {
            events.push(row.map_err(db_err)?);
        }
        Ok(events)
    }
}

pub struct SqliteScopeRepository<'a> {
    conn: &'a Connection,
}

impl<'a> SqliteScopeRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
}

impl ScopeRepository for SqliteScopeRepository<'_> {
    fn add(
        &self,
        project_id: &str,
        task_id: &str,
        pattern: &str,
        now: &str,
    ) -> Result<(), CarryCtxError> {
        let id = ulid::Ulid::generate().to_string();
        self.conn
            .execute(
                "INSERT INTO scopes (id, project_id, task_id, pattern, kind, created_at)
                 VALUES (?1, ?2, ?3, ?4, 'include', ?5)",
                params![id, project_id, task_id, pattern, now],
            )
            .map_err(|e| {
                if is_unique_violation(&e) {
                    CarryCtxError::state_conflict("Scope pattern already exists for this task")
                } else {
                    db_err(e)
                }
            })?;
        Ok(())
    }

    fn remove(&self, project_id: &str, task_id: &str, pattern: &str) -> Result<(), CarryCtxError> {
        let affected = self
            .conn
            .execute(
                "DELETE FROM scopes WHERE project_id = ?1 AND task_id = ?2 AND pattern = ?3",
                params![project_id, task_id, pattern],
            )
            .map_err(db_err)?;
        if affected == 0 {
            return Err(CarryCtxError::resource_not_found("Scope not found"));
        }
        Ok(())
    }

    fn list_for_task(
        &self,
        project_id: &str,
        task_id: &str,
    ) -> Result<Vec<TaskScope>, CarryCtxError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, task_id, pattern, created_at FROM scopes WHERE project_id = ?1 AND task_id = ?2")
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![project_id, task_id], |row| {
                Ok(TaskScope {
                    id: row.get("id")?,
                    task_id: row.get("task_id")?,
                    pattern: row.get("pattern")?,
                    created_at: row.get("created_at")?,
                })
            })
            .map_err(db_err)?;
        let mut scopes = Vec::new();
        for row in rows {
            scopes.push(row.map_err(db_err)?);
        }
        Ok(scopes)
    }

    fn list_active_scopes(&self, project_id: &str) -> Result<Vec<TaskScope>, CarryCtxError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT s.id, s.task_id, s.pattern, s.created_at
                 FROM scopes s
                 JOIN tasks t ON t.id = s.task_id
                 WHERE s.project_id = ?1 AND t.status NOT IN ('completed', 'cancelled')",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![project_id], |row| {
                Ok(TaskScope {
                    id: row.get("id")?,
                    task_id: row.get("task_id")?,
                    pattern: row.get("pattern")?,
                    created_at: row.get("created_at")?,
                })
            })
            .map_err(db_err)?;
        let mut scopes = Vec::new();
        for row in rows {
            scopes.push(row.map_err(db_err)?);
        }
        Ok(scopes)
    }
}

// ── Decision Repository ───────────────────────────────────────────────

pub struct SqliteDecisionRepository<'a> {
    conn: &'a Connection,
}

impl<'a> SqliteDecisionRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
}

impl DecisionRepository for SqliteDecisionRepository<'_> {
    fn create(&self, decision: &Decision) -> Result<Decision, CarryCtxError> {
        let context = decision.context.as_deref();
        let decision_body = decision.decision.as_deref();
        let consequences = decision.consequences.as_deref();
        let alternatives_str =
            serde_json::to_string(&decision.related_tasks).unwrap_or_else(|_| "[]".into());
        let tags_str =
            serde_json::to_string(&decision.related_paths).unwrap_or_else(|_| "[]".into());
        // Build a combined rationale from context/decision/consequences
        let rationale = [
            context.map(|c| format!("Context: {c}")),
            decision_body.map(|d| format!("Decision: {d}")),
            consequences.map(|c| format!("Consequences: {c}")),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n");

        self.conn
            .execute(
                "INSERT INTO decisions (id, project_id, task_id, session_id, display_id, title,
                    context, decision_body, consequences, rationale, alternatives_json, tags_json,
                    created_by_agent, created_by_session, superseded_by, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?16)",
                params![
                    decision.id,
                    decision.project_id,
                    decision.task_id,
                    decision.created_by_session,
                    decision.display_id,
                    decision.title,
                    context,
                    decision_body,
                    consequences,
                    rationale,
                    alternatives_str,
                    tags_str,
                    decision.created_by_agent,
                    decision.created_by_session,
                    decision.superseded_by,
                    decision.created_at,
                ],
            )
            .map_err(db_err)?;
        self.find_by_id(&decision.project_id, &decision.id)
            .map(|opt| opt.expect("just inserted"))
    }

    fn find_by_id(&self, project_id: &str, id: &str) -> Result<Option<Decision>, CarryCtxError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, project_id, task_id, display_id, title, context, decision_body, consequences,
                        alternatives_json, tags_json, created_by_agent, created_by_session,
                        superseded_by, created_at, updated_at
                 FROM decisions WHERE project_id = ?1 AND id = ?2",
            )
            .map_err(db_err)?;
        let mut rows = stmt
            .query_map(
                params![project_id, id],
                |row| -> rusqlite::Result<Decision> {
                    let alts: Vec<String> = row
                        .get::<_, String>("alternatives_json")
                        .map(|s| serde_json::from_str(&s).unwrap_or_default())
                        .unwrap_or_default();
                    let tags: Vec<String> = row
                        .get::<_, String>("tags_json")
                        .map(|s| serde_json::from_str(&s).unwrap_or_default())
                        .unwrap_or_default();
                    Ok(Decision {
                        id: row.get("id")?,
                        display_id: row.get("display_id")?,
                        project_id: row.get("project_id")?,
                        task_id: row.get("task_id")?,
                        title: row.get("title")?,
                        context: row.get("context")?,
                        decision: row.get("decision_body")?,
                        consequences: row.get("consequences")?,
                        related_tasks: alts,
                        related_paths: tags,
                        created_by_agent: row.get("created_by_agent")?,
                        created_by_session: row.get("created_by_session")?,
                        superseded_by: row.get("superseded_by")?,
                        created_at: row.get("created_at")?,
                        updated_at: row.get("updated_at")?,
                    })
                },
            )
            .map_err(db_err)?;
        match rows.next() {
            Some(Ok(d)) => Ok(Some(d)),
            Some(Err(e)) => Err(db_err(e)),
            None => Ok(None),
        }
    }

    fn find_by_display_id(
        &self,
        project_id: &str,
        display_id: &str,
    ) -> Result<Option<Decision>, CarryCtxError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, project_id, task_id, display_id, title, context, decision_body, consequences,
                        alternatives_json, tags_json, created_by_agent, created_by_session,
                        superseded_by, created_at, updated_at
                 FROM decisions WHERE project_id = ?1 AND display_id = ?2",
            )
            .map_err(db_err)?;
        let mut rows = stmt
            .query_map(
                params![project_id, display_id],
                |row| -> rusqlite::Result<Decision> {
                    let alts: Vec<String> = row
                        .get::<_, String>("alternatives_json")
                        .map(|s| serde_json::from_str(&s).unwrap_or_default())
                        .unwrap_or_default();
                    let tags: Vec<String> = row
                        .get::<_, String>("tags_json")
                        .map(|s| serde_json::from_str(&s).unwrap_or_default())
                        .unwrap_or_default();
                    Ok(Decision {
                        id: row.get("id")?,
                        display_id: row.get("display_id")?,
                        project_id: row.get("project_id")?,
                        task_id: row.get("task_id")?,
                        title: row.get("title")?,
                        context: row.get("context")?,
                        decision: row.get("decision_body")?,
                        consequences: row.get("consequences")?,
                        related_tasks: alts,
                        related_paths: tags,
                        created_by_agent: row.get("created_by_agent")?,
                        created_by_session: row.get("created_by_session")?,
                        superseded_by: row.get("superseded_by")?,
                        created_at: row.get("created_at")?,
                        updated_at: row.get("updated_at")?,
                    })
                },
            )
            .map_err(db_err)?;
        match rows.next() {
            Some(Ok(d)) => Ok(Some(d)),
            Some(Err(e)) => Err(db_err(e)),
            None => Ok(None),
        }
    }

    fn list(&self, project_id: &str) -> Result<Vec<Decision>, CarryCtxError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, project_id, task_id, display_id, title, context, decision_body, consequences,
                        alternatives_json, tags_json, created_by_agent, created_by_session,
                        superseded_by, created_at, updated_at
                 FROM decisions WHERE project_id = ?1 ORDER BY created_at DESC",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![project_id], |row| -> rusqlite::Result<Decision> {
                let alts: Vec<String> = row
                    .get::<_, String>("alternatives_json")
                    .map(|s| serde_json::from_str(&s).unwrap_or_default())
                    .unwrap_or_default();
                let tags: Vec<String> = row
                    .get::<_, String>("tags_json")
                    .map(|s| serde_json::from_str(&s).unwrap_or_default())
                    .unwrap_or_default();
                Ok(Decision {
                    id: row.get("id")?,
                    display_id: row.get("display_id")?,
                    project_id: row.get("project_id")?,
                    task_id: row.get("task_id")?,
                    title: row.get("title")?,
                    context: row.get("context")?,
                    decision: row.get("decision_body")?,
                    consequences: row.get("consequences")?,
                    related_tasks: alts,
                    related_paths: tags,
                    created_by_agent: row.get("created_by_agent")?,
                    created_by_session: row.get("created_by_session")?,
                    superseded_by: row.get("superseded_by")?,
                    created_at: row.get("created_at")?,
                    updated_at: row.get("updated_at")?,
                })
            })
            .map_err(db_err)?;
        let mut decisions = Vec::new();
        for row in rows {
            decisions.push(row.map_err(db_err)?);
        }
        Ok(decisions)
    }

    fn search(&self, project_id: &str, query: &str) -> Result<Vec<Decision>, CarryCtxError> {
        let pattern = format!("%{query}%");
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, project_id, task_id, display_id, title, context, decision_body, consequences,
                        alternatives_json, tags_json, created_by_agent, created_by_session,
                        superseded_by, created_at, updated_at
                 FROM decisions
                 WHERE project_id = ?1
                   AND (title LIKE ?2 OR context LIKE ?2 OR decision_body LIKE ?2 OR consequences LIKE ?2)
                 ORDER BY created_at DESC",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(
                params![project_id, pattern],
                |row| -> rusqlite::Result<Decision> {
                    let alts: Vec<String> = row
                        .get::<_, String>("alternatives_json")
                        .map(|s| serde_json::from_str(&s).unwrap_or_default())
                        .unwrap_or_default();
                    let tags: Vec<String> = row
                        .get::<_, String>("tags_json")
                        .map(|s| serde_json::from_str(&s).unwrap_or_default())
                        .unwrap_or_default();
                    Ok(Decision {
                        id: row.get("id")?,
                        display_id: row.get("display_id")?,
                        project_id: row.get("project_id")?,
                        task_id: row.get("task_id")?,
                        title: row.get("title")?,
                        context: row.get("context")?,
                        decision: row.get("decision_body")?,
                        consequences: row.get("consequences")?,
                        related_tasks: alts,
                        related_paths: tags,
                        created_by_agent: row.get("created_by_agent")?,
                        created_by_session: row.get("created_by_session")?,
                        superseded_by: row.get("superseded_by")?,
                        created_at: row.get("created_at")?,
                        updated_at: row.get("updated_at")?,
                    })
                },
            )
            .map_err(db_err)?;
        let mut decisions = Vec::new();
        for row in rows {
            decisions.push(row.map_err(db_err)?);
        }
        Ok(decisions)
    }

    fn supersede(
        &self,
        decision_id: &str,
        project_id: &str,
        superseded_by: &str,
        now: &str,
    ) -> Result<(), CarryCtxError> {
        let affected = self
            .conn
            .execute(
                "UPDATE decisions SET superseded_by = ?1, updated_at = ?2 WHERE id = ?3 AND project_id = ?4",
                params![superseded_by, now, decision_id, project_id],
            )
            .map_err(db_err)?;
        if affected == 0 {
            return Err(CarryCtxError::resource_not_found(format!(
                "Decision {decision_id} not found in project {project_id}"
            )));
        }
        Ok(())
    }
}

// ── Handoff Repository ────────────────────────────────────────────────

pub struct SqliteHandoffRepository<'a> {
    conn: &'a Connection,
}

impl<'a> SqliteHandoffRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
}

impl HandoffRepository for SqliteHandoffRepository<'_> {
    fn create(&self, handoff: &Handoff) -> Result<Handoff, CarryCtxError> {
        // Pack completed_work, remaining_work, blockers, risks, next_steps, changed_files into context_json
        let context = serde_json::json!({
            "completed_work": handoff.completed_work,
            "remaining_work": handoff.remaining_work,
            "blockers": handoff.blockers,
            "risks": handoff.risks,
            "next_steps": handoff.next_steps,
            "changed_files": handoff.changed_files,
        });
        let context_str = serde_json::to_string(&context).unwrap_or_else(|_| "{}".into());
        let state_str = handoff_status_to_sql(&handoff.status);

        self.conn
            .execute(
                "INSERT INTO handoffs (id, project_id, from_agent_id, to_agent_id, task_id, session_id,
                    state, display_id, summary, context_json, head, branch, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)",
                params![
                    handoff.id,
                    handoff.project_id,
                    handoff.source_agent_id,
                    handoff.target_agent_id,
                    handoff.task_id,
                    handoff.source_session_id,
                    state_str,
                    handoff.display_id,
                    handoff.summary.as_deref().unwrap_or(""),
                    context_str,
                    handoff.head,
                    handoff.branch,
                    handoff.created_at,
                ],
            )
            .map_err(db_err)?;
        self.find_by_id(&handoff.project_id, &handoff.id)
            .map(|opt| opt.expect("just inserted"))
    }

    fn find_by_id(&self, project_id: &str, id: &str) -> Result<Option<Handoff>, CarryCtxError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, project_id, from_agent_id, to_agent_id, task_id, session_id,
                        state, display_id, summary, context_json, head, branch,
                        created_at, updated_at
                 FROM handoffs WHERE project_id = ?1 AND id = ?2",
            )
            .map_err(db_err)?;
        let mut rows = stmt
            .query_map(
                params![project_id, id],
                |row| -> rusqlite::Result<Handoff> {
                    let state_str: String = row.get("state")?;
                    let context_str: String = row.get("context_json")?;
                    let ctx: serde_json::Value = serde_json::from_str(&context_str)
                        .unwrap_or(serde_json::Value::Object(Default::default()));
                    let extract = |field: &str| -> Vec<String> {
                        ctx.get(field)
                            .and_then(|v| serde_json::from_value(v.clone()).ok())
                            .unwrap_or_default()
                    };
                    let summary: Option<String> = row.get("summary")?;
                    Ok(Handoff {
                        id: row.get("id")?,
                        display_id: row.get("display_id")?,
                        project_id: row.get("project_id")?,
                        task_id: row.get("task_id")?,
                        source_agent_id: row.get("from_agent_id")?,
                        source_session_id: row.get("session_id")?,
                        target_agent_id: row.get("to_agent_id")?,
                        summary,
                        completed_work: extract("completed_work"),
                        remaining_work: extract("remaining_work"),
                        blockers: extract("blockers"),
                        risks: extract("risks"),
                        next_steps: extract("next_steps"),
                        changed_files: extract("changed_files"),
                        head: row.get("head")?,
                        branch: row.get("branch")?,
                        status: handoff_status_from_sql(&state_str).unwrap_or(HandoffStatus::Open),
                        created_at: row.get("created_at")?,
                        updated_at: row.get("updated_at")?,
                    })
                },
            )
            .map_err(db_err)?;
        match rows.next() {
            Some(Ok(h)) => Ok(Some(h)),
            Some(Err(e)) => Err(db_err(e)),
            None => Ok(None),
        }
    }

    fn find_by_display_id(
        &self,
        project_id: &str,
        display_id: &str,
    ) -> Result<Option<Handoff>, CarryCtxError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, project_id, from_agent_id, to_agent_id, task_id, session_id,
                        state, display_id, summary, context_json, head, branch,
                        created_at, updated_at
                 FROM handoffs WHERE project_id = ?1 AND display_id = ?2",
            )
            .map_err(db_err)?;
        let mut rows = stmt
            .query_map(
                params![project_id, display_id],
                |row| -> rusqlite::Result<Handoff> {
                    let state_str: String = row.get("state")?;
                    let context_str: String = row.get("context_json")?;
                    let ctx: serde_json::Value = serde_json::from_str(&context_str)
                        .unwrap_or(serde_json::Value::Object(Default::default()));
                    let extract = |field: &str| -> Vec<String> {
                        ctx.get(field)
                            .and_then(|v| serde_json::from_value(v.clone()).ok())
                            .unwrap_or_default()
                    };
                    Ok(Handoff {
                        id: row.get("id")?,
                        project_id: row.get("project_id")?,
                        task_id: row.get("task_id")?,
                        source_agent_id: row.get("from_agent_id")?,
                        source_session_id: row.get("session_id")?,
                        target_agent_id: row.get("to_agent_id")?,
                        summary: Some(row.get::<_, String>("summary")?).filter(|s| !s.is_empty()),
                        display_id: row.get("display_id")?,
                        completed_work: extract("completed_work"),
                        remaining_work: extract("remaining_work"),
                        blockers: extract("blockers"),
                        risks: extract("risks"),
                        next_steps: extract("next_steps"),
                        changed_files: extract("changed_files"),
                        head: row.get("head")?,
                        branch: row.get("branch")?,
                        status: handoff_status_from_sql(&state_str).unwrap_or(HandoffStatus::Open),
                        created_at: row.get("created_at")?,
                        updated_at: row.get("updated_at")?,
                    })
                },
            )
            .map_err(db_err)?;
        match rows.next() {
            Some(Ok(h)) => Ok(Some(h)),
            Some(Err(e)) => Err(db_err(e)),
            None => Ok(None),
        }
    }

    fn list(&self, project_id: &str) -> Result<Vec<Handoff>, CarryCtxError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, project_id, from_agent_id, to_agent_id, task_id, session_id,
                        state, display_id, summary, context_json, head, branch,
                        created_at, updated_at
                 FROM handoffs WHERE project_id = ?1 ORDER BY created_at DESC",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![project_id], |row| -> rusqlite::Result<Handoff> {
                let state_str: String = row.get("state")?;
                let context_str: String = row.get("context_json")?;
                let ctx: serde_json::Value = serde_json::from_str(&context_str)
                    .unwrap_or(serde_json::Value::Object(Default::default()));
                let extract = |field: &str| -> Vec<String> {
                    ctx.get(field)
                        .and_then(|v| serde_json::from_value(v.clone()).ok())
                        .unwrap_or_default()
                };
                let summary: Option<String> = row.get("summary")?;
                Ok(Handoff {
                    id: row.get("id")?,
                    display_id: row.get("display_id")?,
                    project_id: row.get("project_id")?,
                    task_id: row.get("task_id")?,
                    source_agent_id: row.get("from_agent_id")?,
                    source_session_id: row.get("session_id")?,
                    target_agent_id: row.get("to_agent_id")?,
                    summary,
                    completed_work: extract("completed_work"),
                    remaining_work: extract("remaining_work"),
                    blockers: extract("blockers"),
                    risks: extract("risks"),
                    next_steps: extract("next_steps"),
                    changed_files: extract("changed_files"),
                    head: row.get("head")?,
                    branch: row.get("branch")?,
                    status: handoff_status_from_sql(&state_str).unwrap_or(HandoffStatus::Open),
                    created_at: row.get("created_at")?,
                    updated_at: row.get("updated_at")?,
                })
            })
            .map_err(db_err)?;
        let mut handoffs = Vec::new();
        for row in rows {
            handoffs.push(row.map_err(db_err)?);
        }
        Ok(handoffs)
    }

    fn update_status(
        &self,
        id: &str,
        project_id: &str,
        status: HandoffStatus,
        now: &str,
    ) -> Result<(), CarryCtxError> {
        let state_str = handoff_status_to_sql(&status);
        let affected = self
            .conn
            .execute(
                "UPDATE handoffs SET state = ?1, updated_at = ?2 WHERE id = ?3 AND project_id = ?4",
                params![state_str, now, id, project_id],
            )
            .map_err(db_err)?;
        if affected == 0 {
            return Err(CarryCtxError::resource_not_found(format!(
                "Handoff {id} not found in project {project_id}"
            )));
        }
        Ok(())
    }
}

// ── Error helpers ──────────────────────────────────────────────────────

fn is_unique_violation(e: &rusqlite::Error) -> bool {
    matches!(e, rusqlite::Error::SqliteFailure(err, _) if err.code == rusqlite::ErrorCode::ConstraintViolation)
}

fn is_foreign_key_violation(e: &rusqlite::Error) -> bool {
    matches!(e, rusqlite::Error::SqliteFailure(err, _) if err.code == rusqlite::ErrorCode::ConstraintViolation)
}
