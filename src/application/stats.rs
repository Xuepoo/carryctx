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
        SELECT a.name, s.started_at, s.ended_at
        FROM sessions s
        JOIN agents a ON s.agent_id = a.id
        WHERE s.ended_at IS NOT NULL
    "
    .to_string();

    if agent_filter.is_some() {
        sql.push_str(" AND a.name = ?1");
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
        let started_at: String = row.get(1).unwrap_or_default();
        let ended_at: String = row.get(2).unwrap_or_default();

        let start = DateTime::parse_from_rfc3339(&started_at).unwrap_or(Utc::now().into());
        let end = DateTime::parse_from_rfc3339(&ended_at).unwrap_or(start);

        let diff = end.signed_duration_since(start).num_seconds().max(0) as u64;

        let stat = stats_map.entry(name.clone()).or_insert(AgentStat {
            agent_name: name,
            total_sessions: 0,
            total_seconds: 0,
        });

        stat.total_sessions += 1;
        stat.total_seconds += diff;
    }

    let mut result: Vec<AgentStat> = stats_map.into_values().collect();
    result.sort_by_key(|b| std::cmp::Reverse(b.total_seconds));

    Ok(result)
}
