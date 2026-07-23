use crate::*;
use carryctx::application::runtime::InvocationContext;
use carryctx::domain::agent::AgentStatus;
use carryctx::error::ExitCode;
use clap::Parser;

// ── Status ───────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub struct StatusArgs {
    /// Show only items assigned to the current agent.
    #[arg(long)]
    pub mine: bool,

    /// Show all items across the entire project regardless of status or assignment.
    #[arg(long)]
    pub all: bool,

    /// Print output in a compact format without detailed descriptions.
    #[arg(long)]
    pub compact: bool,

    /// Include active and recent agent sessions in the status report.
    #[arg(long)]
    pub sessions: bool,

    /// Include active and pending tasks in the status report.
    #[arg(long)]
    pub tasks: bool,

    /// Include current Git worktrees linked to tasks.
    #[arg(long)]
    pub worktrees: bool,

    /// Only show events/status changes that occurred since a specific timestamp or duration (e.g., '24h', '2023-01-01').
    #[arg(long)]
    pub since: Option<String>,
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: status
// ═══════════════════════════════════════════════════════════════════════════

pub fn handle_status(
    _args: &StatusArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    let mut runtime = try_open_runtime(ctx)?;
    let project_id = &runtime.config.project.id;
    let conn = runtime.database.connection_mut();

    let task_repo = SqliteTaskRepository::new(conn);
    let session_repo = SqliteSessionRepository::new(conn);
    let agent_repo = SqliteAgentRepository::new(conn);
    let worktree_repo = SqliteWorktreeRepository::new(conn);

    let active_sessions = session_repo.list(project_id).map_err(|e| e.exit_code)?;
    let active_agents = agent_repo
        .list(&AgentFilter {
            project_id: project_id.to_string(),
            status: Some(AgentStatus::Active),
        })
        .map_err(|e| e.exit_code)?;

    let task_filter = TaskFilter {
        project_id: project_id.to_string(),
        status: None,
        owner_agent_id: None,
        ready: false,
        blocked: false,
        mine: None,
    };
    let all_tasks = task_repo.list(&task_filter).map_err(|e| e.exit_code)?;
    let worktrees = worktree_repo.list(project_id).map_err(|e| e.exit_code)?;

    let data = serde_json::json!({
        "projectId": project_id,
        "projectName": runtime.config.project.name,
        "repositoryRoot": runtime.git_project.repository_root,
        "activeSessions": active_sessions.len(),
        "activeAgents": active_agents.len(),
        "totalTasks": all_tasks.len(),
        "worktrees": worktrees.len(),
        "head": runtime.git_project.head,
        "branch": runtime.git_project.branch,
    });

    render_and_print("status", Ok(data), is_json, ctx.quiet)
}
