use crate::*;
use carryctx::application::runtime::{InvocationContext, ProjectRuntime};
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

    /// Add a Git worktrees table to the Markdown report. The JSON output
    /// always includes the full `worktrees` array regardless of this flag.
    #[arg(long)]
    pub worktrees: bool,
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: status
// ═══════════════════════════════════════════════════════════════════════════

pub fn handle_status(
    args: &StatusArgs,
    pre_opened: Option<ProjectRuntime>,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    // Reuse the dispatcher's pre-opened runtime when available; a second
    // open only happens (and reports) when that failed.
    let mut runtime = match pre_opened {
        Some(runtime) => runtime,
        None => open_runtime_or_report(ctx, "status")?,
    };
    let project_id = &runtime.config.project.id;
    let conn = runtime.database.connection_mut();

    let task_repo = SqliteTaskRepository::new(conn);
    let session_repo = SqliteSessionRepository::new(conn);
    let agent_repo = SqliteAgentRepository::new(conn);
    let worktree_repo = SqliteWorktreeRepository::new(conn);
    let all_sessions = session_repo.list(project_id).map_err(|e| e.exit_code)?;
    let active_sessions = all_sessions
        .into_iter()
        .filter(|s| s.state == carryctx::domain::session::SessionState::Active)
        .collect::<Vec<_>>();
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
    // The listing above is capped, so `len()` under-reports on big
    // projects; totals come from an exact COUNT(*) instead (CTX-0080).
    let total_tasks = task_repo.count_all(project_id).map_err(|e| e.exit_code)?;
    let worktrees = worktree_repo.list(project_id).map_err(|e| e.exit_code)?;

    // Check for Markdown format
    if ctx.format == carryctx::application::runtime::OutputFormat::Markdown {
        let branch = runtime.git_project.branch.as_deref().unwrap_or("unknown");
        let head = runtime.git_project.head.as_deref().unwrap_or("none");
        let mut md = format!(
            "# CarryCtx Status\n\n\
             - **Project**: {name}\n\
             - **Repository**: {root}\n\
             - **Branch**: {branch}\n\
             - **HEAD**: {head}\n\
             - **Active Sessions**: {sessions}\n\
             - **Active Agents**: {agents}\n\
             - **Total Tasks**: {tasks}\n\
             - **Worktrees**: {worktrees}\n",
            name = runtime.config.project.name,
            root = runtime.git_project.repository_root.display(),
            branch = branch,
            head = head,
            sessions = active_sessions.len(),
            agents = active_agents.len(),
            tasks = total_tasks,
            worktrees = worktrees.len(),
        );
        // `--worktrees` opts into a detailed table; the summary line above
        // always shows the count. (The JSON envelope always carries the
        // full worktrees array.)
        if args.worktrees {
            md.push_str("\n## Worktrees\n\n");
            if worktrees.is_empty() {
                md.push_str("No task-linked worktrees.\n");
            } else {
                md.push_str("| Path | Branch | Task |\n|---|---|---|\n");
                for wt in &worktrees {
                    let repo_root = runtime.git_project.repository_root.to_string_lossy();
                    let rel_path = wt.path.trim_start_matches(repo_root.as_ref());
                    md.push_str(&format!(
                        "| {} | {} | {} |\n",
                        truncate_chars(rel_path, 40),
                        truncate_chars(wt.branch.as_deref().unwrap_or("-"), 24),
                        truncate_chars(wt.task_id.as_deref().unwrap_or("-"), 12),
                    ));
                }
            }
        }
        if !ctx.quiet {
            print!("{md}");
        }
        return Ok(ExitCode::Success);
    }

    let data = serde_json::json!({
        "projectId": project_id,
        "projectName": runtime.config.project.name,
        "repositoryRoot": runtime.git_project.repository_root,
        "activeSessions": active_sessions,
        "activeAgents": active_agents,
        "totalTasks": total_tasks,
        "tasks": all_tasks,
        "worktrees": worktrees,
        "head": runtime.git_project.head,
        "branch": runtime.git_project.branch,
    });

    render_and_print_entity(
        "status",
        Ok(data),
        is_json,
        ctx.quiet,
        ctx.verbose || runtime.config.output.verbose,
        ctx.fields.as_deref(),
        Some(&runtime.config.output.fields),
    )
}
