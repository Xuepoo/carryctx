use crate::domain::search::{SearchHit, SearchKind};
use crate::error::CarryCtxError;
use rusqlite::Connection;

fn db_err(e: rusqlite::Error) -> CarryCtxError {
    CarryCtxError::database_error(e.to_string())
}

/// Options narrowing a full-text search, shared across every entity kind.
#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    /// Restrict to one entity kind. `None` searches all four.
    pub kind: Option<SearchKind>,
    /// Restrict to tasks in this status (applies via the owning task, so it
    /// filters progress/checkpoint/decision hits too, not just task hits).
    pub status: Option<String>,
    /// Restrict to hits whose owning task's `owner_agent_id` matches this
    /// agent ULID.
    pub agent_id: Option<String>,
    pub limit: u32,
}

pub struct SearchRepository<'a> {
    conn: &'a Connection,
}

impl<'a> SearchRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Run the full-text search and return hits merged across every
    /// requested kind, sorted by BM25 score (ascending: SQLite FTS5 scores
    /// more relevant matches closer to zero and less relevant ones more
    /// negative) and capped at `options.limit` post-merge.
    pub fn search(
        &self,
        project_id: &str,
        query: &str,
        options: &SearchOptions,
    ) -> Result<Vec<SearchHit>, CarryCtxError> {
        let kinds: Vec<SearchKind> = match options.kind {
            Some(k) => vec![k],
            None => vec![
                SearchKind::Task,
                SearchKind::Progress,
                SearchKind::Checkpoint,
                SearchKind::Decision,
            ],
        };

        let mut hits = Vec::new();
        for kind in kinds {
            hits.extend(self.search_kind(project_id, query, kind, options)?);
        }
        hits.sort_by(|a, b| {
            a.score
                .partial_cmp(&b.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hits.truncate(options.limit.max(1) as usize);
        Ok(hits)
    }

    fn search_kind(
        &self,
        project_id: &str,
        query: &str,
        kind: SearchKind,
        options: &SearchOptions,
    ) -> Result<Vec<SearchHit>, CarryCtxError> {
        match kind {
            SearchKind::Task => self.search_tasks(project_id, query, options),
            SearchKind::Progress => self.search_progress(project_id, query, options),
            SearchKind::Checkpoint => self.search_checkpoints(project_id, query, options),
            SearchKind::Decision => self.search_decisions(project_id, query, options),
        }
    }

    fn status_and_agent_clause(options: &SearchOptions, start_idx: usize) -> String {
        let mut clause = String::new();
        let mut idx = start_idx;
        if options.status.is_some() {
            clause.push_str(&format!(" AND t.status = ?{idx}"));
            idx += 1;
        }
        if options.agent_id.is_some() {
            clause.push_str(&format!(" AND t.owner_agent_id = ?{idx}"));
        }
        clause
    }
    fn bind_status_and_agent<'p>(
        options: &'p SearchOptions,
        params: &mut Vec<&'p dyn rusqlite::types::ToSql>,
    ) {
        if let Some(status) = &options.status {
            params.push(status);
        }
        if let Some(agent_id) = &options.agent_id {
            params.push(agent_id);
        }
    }

    fn search_tasks(
        &self,
        project_id: &str,
        query: &str,
        options: &SearchOptions,
    ) -> Result<Vec<SearchHit>, CarryCtxError> {
        let extra_clause = Self::status_and_agent_clause(options, 4);
        let sql = format!(
            "SELECT t.id, t.display_id, t.status, w.branch, t.created_at,
                    bm25(tasks_fts) AS score,
                    snippet(tasks_fts, -1, '[', ']', '...', 12) AS snip
             FROM tasks_fts
             JOIN tasks t ON t.rowid = tasks_fts.rowid
             LEFT JOIN worktrees w ON w.task_id = t.id AND w.project_id = t.project_id
             WHERE t.project_id = ?1 AND tasks_fts MATCH ?2{extra_clause}
             ORDER BY score
             LIMIT ?3"
        );
        let mut stmt = self.conn.prepare(&sql).map_err(db_err)?;
        let limit = options.limit as i64;
        let mut param_values: Vec<&dyn rusqlite::types::ToSql> = vec![&project_id, &query, &limit];
        Self::bind_status_and_agent(options, &mut param_values);
        let rows = stmt
            .query_map(rusqlite::params_from_iter(param_values), |row| {
                let task_id: String = row.get("id")?;
                let task_display_id: String = row.get("display_id")?;
                Ok(SearchHit {
                    kind: SearchKind::Task,
                    id: task_id.clone(),
                    display_id: None,
                    task_id,
                    task_display_id,
                    task_status: row.get("status")?,
                    branch: row.get("branch")?,
                    snippet: row.get("snip")?,
                    score: row.get("score")?,
                    created_at: row.get("created_at")?,
                })
            })
            .map_err(db_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_err)
    }

    fn search_progress(
        &self,
        project_id: &str,
        query: &str,
        options: &SearchOptions,
    ) -> Result<Vec<SearchHit>, CarryCtxError> {
        let extra_clause = Self::status_and_agent_clause(options, 4);
        let sql = format!(
            "SELECT p.id, p.display_id, p.created_at,
                    t.id AS task_id, t.display_id AS task_display_id, t.status AS task_status,
                    w.branch,
                    bm25(progress_items_fts) AS score,
                    snippet(progress_items_fts, -1, '[', ']', '...', 12) AS snip
             FROM progress_items_fts
             JOIN progress_items p ON p.rowid = progress_items_fts.rowid
             JOIN tasks t ON t.id = p.task_id
             LEFT JOIN worktrees w ON w.task_id = t.id AND w.project_id = t.project_id
             WHERE p.project_id = ?1 AND progress_items_fts MATCH ?2{extra_clause}
             ORDER BY score
             LIMIT ?3"
        );
        let mut stmt = self.conn.prepare(&sql).map_err(db_err)?;
        let limit = options.limit as i64;
        let mut param_values: Vec<&dyn rusqlite::types::ToSql> = vec![&project_id, &query, &limit];
        Self::bind_status_and_agent(options, &mut param_values);
        let rows = stmt
            .query_map(rusqlite::params_from_iter(param_values), |row| {
                Ok(SearchHit {
                    kind: SearchKind::Progress,
                    id: row.get("id")?,
                    display_id: row.get("display_id")?,
                    task_id: row.get("task_id")?,
                    task_display_id: row.get("task_display_id")?,
                    task_status: row.get("task_status")?,
                    branch: row.get("branch")?,
                    snippet: row.get("snip")?,
                    score: row.get("score")?,
                    created_at: row.get("created_at")?,
                })
            })
            .map_err(db_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_err)
    }

    fn search_checkpoints(
        &self,
        project_id: &str,
        query: &str,
        options: &SearchOptions,
    ) -> Result<Vec<SearchHit>, CarryCtxError> {
        let extra_clause = Self::status_and_agent_clause(options, 4);
        // Checkpoints have their own `branch` column (captured at checkpoint
        // time) which is a more precise answer to "what branch was this on"
        // than the task's current worktree binding, so prefer it and fall
        // back to the worktree's branch only if the checkpoint didn't
        // record one (e.g. `--no-git`).
        let sql = format!(
            "SELECT c.id, c.created_at,
                    t.id AS task_id, t.display_id AS task_display_id, t.status AS task_status,
                    COALESCE(c.branch, w.branch) AS branch,
                    bm25(checkpoints_fts) AS score,
                    snippet(checkpoints_fts, -1, '[', ']', '...', 12) AS snip
             FROM checkpoints_fts
             JOIN checkpoints c ON c.rowid = checkpoints_fts.rowid
             JOIN tasks t ON t.id = c.task_id
             LEFT JOIN worktrees w ON w.task_id = t.id AND w.project_id = t.project_id
             WHERE c.project_id = ?1 AND checkpoints_fts MATCH ?2{extra_clause}
             ORDER BY score
             LIMIT ?3"
        );
        let mut stmt = self.conn.prepare(&sql).map_err(db_err)?;
        let limit = options.limit as i64;
        let mut param_values: Vec<&dyn rusqlite::types::ToSql> = vec![&project_id, &query, &limit];
        Self::bind_status_and_agent(options, &mut param_values);
        let rows = stmt
            .query_map(rusqlite::params_from_iter(param_values), |row| {
                Ok(SearchHit {
                    kind: SearchKind::Checkpoint,
                    id: row.get("id")?,
                    display_id: None,
                    task_id: row.get("task_id")?,
                    task_display_id: row.get("task_display_id")?,
                    task_status: row.get("task_status")?,
                    branch: row.get("branch")?,
                    snippet: row.get("snip")?,
                    score: row.get("score")?,
                    created_at: row.get("created_at")?,
                })
            })
            .map_err(db_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_err)
    }

    fn search_decisions(
        &self,
        project_id: &str,
        query: &str,
        options: &SearchOptions,
    ) -> Result<Vec<SearchHit>, CarryCtxError> {
        let extra_clause = Self::status_and_agent_clause(options, 4);
        let sql = format!(
            "SELECT d.id, d.display_id, d.created_at,
                    t.id AS task_id, t.display_id AS task_display_id, t.status AS task_status,
                    w.branch,
                    bm25(decisions_fts) AS score,
                    snippet(decisions_fts, -1, '[', ']', '...', 12) AS snip
             FROM decisions_fts
             JOIN decisions d ON d.rowid = decisions_fts.rowid
             JOIN tasks t ON t.id = d.task_id
             LEFT JOIN worktrees w ON w.task_id = t.id AND w.project_id = t.project_id
             WHERE d.project_id = ?1 AND decisions_fts MATCH ?2{extra_clause}
             ORDER BY score
             LIMIT ?3"
        );
        let mut stmt = self.conn.prepare(&sql).map_err(db_err)?;
        let limit = options.limit as i64;
        let mut param_values: Vec<&dyn rusqlite::types::ToSql> = vec![&project_id, &query, &limit];
        Self::bind_status_and_agent(options, &mut param_values);
        let rows = stmt
            .query_map(rusqlite::params_from_iter(param_values), |row| {
                Ok(SearchHit {
                    kind: SearchKind::Decision,
                    id: row.get("id")?,
                    display_id: row.get("display_id")?,
                    task_id: row.get("task_id")?,
                    task_display_id: row.get("task_display_id")?,
                    task_status: row.get("task_status")?,
                    branch: row.get("branch")?,
                    snippet: row.get("snip")?,
                    score: row.get("score")?,
                    created_at: row.get("created_at")?,
                })
            })
            .map_err(db_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_err)
    }
}
