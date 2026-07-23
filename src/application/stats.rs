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

#[derive(Debug, serde::Serialize)]
pub struct ProjectStats {
    pub tasks_total: u64,
    pub tasks_planned: u64,
    pub tasks_ready: u64,
    pub tasks_in_progress: u64,
    pub tasks_completed: u64,
    pub tasks_cancelled: u64,
    pub graph_nodes_total: u64,
    pub graph_edges_total: u64,
    pub checkpoints_total: u64,
    pub sessions_total: u64,
    pub total_seconds: u64,
    pub agent_stats: Vec<AgentStat>,
}

pub fn compute_stats(
    project_path: &Path,
    agent_filter: Option<&str>,
) -> Result<ProjectStats, CarryCtxError> {
    let git = GitCli::new();
    let gp = git.discover(project_path)?;
    let xdg = XdgPaths::new();
    let db_path = xdg.project_db(&gp.git_common_dir);

    let db = ProjectDatabase::open_readonly(&db_path)?;
    let conn = db.connection();

    // 1. Task counts by status
    let mut tasks_total = 0u64;
    let mut tasks_planned = 0u64;
    let mut tasks_ready = 0u64;
    let mut tasks_in_progress = 0u64;
    let mut tasks_completed = 0u64;
    let mut tasks_cancelled = 0u64;

    if let Ok(mut stmt) = conn.prepare("SELECT status, COUNT(*) FROM tasks GROUP BY status") {
        if let Ok(mut rows) = stmt.query([]) {
            while let Ok(Some(row)) = rows.next() {
                let status: String = row.get(0).unwrap_or_default();
                let count: i64 = row.get(1).unwrap_or(0);
                let ucount = count as u64;
                tasks_total += ucount;
                match status.as_str() {
                    "planned" => tasks_planned += ucount,
                    "ready" => tasks_ready += ucount,
                    "in_progress" => tasks_in_progress += ucount,
                    "completed" => tasks_completed += ucount,
                    "cancelled" => tasks_cancelled += ucount,
                    _ => {}
                }
            }
        }
    }

    // 2. Graph node & edge counts
    let graph_nodes_total: u64 = conn
        .query_row("SELECT COUNT(*) FROM graph_nodes", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap_or(0) as u64;
    let graph_edges_total: u64 = conn
        .query_row("SELECT COUNT(*) FROM graph_edges", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap_or(0) as u64;

    // 3. Checkpoints & sessions total
    let checkpoints_total: u64 = conn
        .query_row("SELECT COUNT(*) FROM checkpoints", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap_or(0) as u64;
    let sessions_total: u64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get::<_, i64>(0))
        .unwrap_or(0) as u64;

    // 4. Per-agent stats
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

    let mut total_seconds = 0u64;

    while let Some(row) = rows.next().unwrap_or(None) {
        let name: String = row.get(0).unwrap_or_default();
        if name.is_empty() {
            continue;
        }

        let started_at: Option<String> = row.get(1).ok();
        let ended_at: Option<String> = row.get(2).ok();
        let agent_checkpoints: i64 = row.get(3).unwrap_or(0);
        let agent_tasks_completed: i64 = row.get(4).unwrap_or(0);
        let agent_blockers: i64 = row.get(5).unwrap_or(0);

        let diff_sec = if let Some(start_str) = started_at {
            let start = DateTime::parse_from_rfc3339(&start_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            let end = if let Some(ref end_str) = ended_at {
                DateTime::parse_from_rfc3339(end_str)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now())
            } else {
                Utc::now()
            };
            end.signed_duration_since(start).num_seconds().max(0) as u64
        } else {
            0
        };

        total_seconds += diff_sec;

        let stat = stats_map.entry(name.clone()).or_insert(AgentStat {
            agent_name: name,
            total_sessions: 0,
            total_seconds: 0,
            total_checkpoints: agent_checkpoints as u64,
            tasks_completed: agent_tasks_completed as u64,
            blockers_reported: agent_blockers as u64,
        });

        stat.total_sessions += 1;
        stat.total_seconds += diff_sec;
    }

    let mut agent_stats: Vec<AgentStat> = stats_map.into_values().collect();
    agent_stats.sort_by_key(|b| std::cmp::Reverse(b.total_seconds));

    Ok(ProjectStats {
        tasks_total,
        tasks_planned,
        tasks_ready,
        tasks_in_progress,
        tasks_completed,
        tasks_cancelled,
        graph_nodes_total,
        graph_edges_total,
        checkpoints_total,
        sessions_total,
        total_seconds,
        agent_stats,
    })
}

pub fn render_stats_markdown(stats: &ProjectStats) -> String {
    let mut out = String::new();
    out.push_str("# CarryCtx Project Statistics\n\n");
    out.push_str("## Overview\n");
    out.push_str(&format!(
        "- **Total Tasks**: {} (Completed: {}, In Progress: {}, Ready: {}, Planned: {})\n",
        stats.tasks_total,
        stats.tasks_completed,
        stats.tasks_in_progress,
        stats.tasks_ready,
        stats.tasks_planned
    ));
    out.push_str(&format!(
        "- **Code Graph**: {} Nodes, {} Edges\n",
        stats.graph_nodes_total, stats.graph_edges_total
    ));
    out.push_str(&format!(
        "- **Sessions & Checkpoints**: {} Sessions, {} Checkpoints\n",
        stats.sessions_total, stats.checkpoints_total
    ));

    let hours = stats.total_seconds / 3600;
    let minutes = (stats.total_seconds % 3600) / 60;
    out.push_str(&format!(
        "- **Total Agent Work Time**: {}h {}m\n\n",
        hours, minutes
    ));

    out.push_str("## Agent Performance\n");
    out.push_str("| Agent Name | Sessions | Time Spent | Checkpoints | Tasks Done | Blockers |\n");
    out.push_str("| :--- | :--- | :--- | :--- | :--- | :--- |\n");

    for stat in &stats.agent_stats {
        let h = stat.total_seconds / 3600;
        let m = (stat.total_seconds % 3600) / 60;
        out.push_str(&format!(
            "| {} | {} | {}h {}m | {} | {} | {} |\n",
            stat.agent_name,
            stat.total_sessions,
            h,
            m,
            stat.total_checkpoints,
            stat.tasks_completed,
            stat.blockers_reported
        ));
    }

    out
}

pub fn export_stats_csv(stats: &ProjectStats) -> String {
    let mut out = String::from(
        "agent_name,sessions,total_seconds,checkpoints,tasks_completed,blockers_reported\n",
    );
    for stat in &stats.agent_stats {
        out.push_str(&format!(
            "{},{},{},{},{},{}\n",
            stat.agent_name,
            stat.total_sessions,
            stat.total_seconds,
            stat.total_checkpoints,
            stat.tasks_completed,
            stat.blockers_reported
        ));
    }
    out
}
