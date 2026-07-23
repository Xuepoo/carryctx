use crate::adapter::git::GitCli;
use crate::adapter::sqlite::ProjectDatabase;
use crate::adapter::xdg::XdgPaths;
use crate::error::CarryCtxError;
use chrono::{DateTime, Utc};
use std::path::Path;

#[derive(Debug, serde::Serialize)]
pub struct AgentStat {
    pub agent_name: String,
    pub total_sessions: u64,
    pub total_seconds: u64,
    pub total_checkpoints: u64,
    pub tasks_completed: u64,
    pub blockers_reported: u64,
}

pub fn compute_stats(
    project_path: &Path,
    agent_filter: Option<&str>,
) -> Result<Vec<AgentStat>, CarryCtxError> {
    let git = GitCli::new();
    let gp = git.discover(project_path)?;
    let xdg = XdgPaths::new();
    let db_path = xdg.project_db(&gp.git_common_dir);

    let db = ProjectDatabase::open_readonly(&db_path)?;
    let conn = db.connection();

    let mut sql = "
        SELECT 
            a.name,
            s.started_at,
            s.ended_at,
            (SELECT COUNT(*) FROM checkpoints c JOIN sessions cs ON c.session_id = cs.id WHERE cs.agent_id = a.id) as total_checkpoints,
            (SELECT COUNT(*) FROM tasks t WHERE t.owner_agent_id = a.id AND t.status = 'completed') as tasks_completed,
            (SELECT COUNT(*) FROM progress_items p JOIN sessions ps ON p.source_session_id = ps.id WHERE ps.agent_id = a.id AND p.type = 'blocker') as blockers_reported
        FROM agents a
        LEFT JOIN sessions s ON s.agent_id = a.id
    "
    .to_string();

    if agent_filter.is_some() {
        sql.push_str(" WHERE a.name = ?1");
    }

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| CarryCtxError::database_error(format!("Failed to prepare statement: {e}")))?;

    let mut rows = if let Some(agent) = agent_filter {
        stmt.query([agent])
    } else {
        stmt.query([])
    }
    .map_err(|e| CarryCtxError::database_error(format!("Failed to query sessions: {e}")))?;

    let mut stats_map: std::collections::HashMap<String, AgentStat> =
        std::collections::HashMap::new();

    while let Some(row) = rows.next().unwrap_or(None) {
        let name: String = row.get(0).unwrap_or_default();
        if name.is_empty() {
            continue;
        }

        let started_at: Option<String> = row.get(1).ok();
        let ended_at: Option<String> = row.get(2).ok();
        let total_checkpoints: i64 = row.get(3).unwrap_or(0);
        let tasks_completed: i64 = row.get(4).unwrap_or(0);
        let blockers_reported: i64 = row.get(5).unwrap_or(0);

        let mut diff_sec = 0u64;
        let mut session_count = 0u64;

        if let (Some(start_str), Some(end_str)) = (started_at, ended_at) {
            let start =
                DateTime::parse_from_rfc3339(&start_str).unwrap_or_else(|_| Utc::now().into());
            let end = DateTime::parse_from_rfc3339(&end_str).unwrap_or(start);
            diff_sec = end.signed_duration_since(start).num_seconds().max(0) as u64;
            session_count = 1;
        }

        let stat = stats_map.entry(name.clone()).or_insert(AgentStat {
            agent_name: name,
            total_sessions: 0,
            total_seconds: 0,
            total_checkpoints: total_checkpoints as u64,
            tasks_completed: tasks_completed as u64,
            blockers_reported: blockers_reported as u64,
        });

        stat.total_sessions += session_count;
        stat.total_seconds += diff_sec;
    }

    let mut result: Vec<AgentStat> = stats_map.into_values().collect();
    result.sort_by_key(|b| std::cmp::Reverse(b.total_seconds));

    Ok(result)
}
