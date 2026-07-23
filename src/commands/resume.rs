use crate::*;
use carryctx::application::runtime::InvocationContext;
use carryctx::error::ExitCode;
use clap::Parser;

// ── Resume ───────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub struct ResumeArgs {
    /// Target a specific task ULID to resume. Defaults to the task currently bound to the worktree.
    #[arg(long)]
    pub task: Option<String>,

    /// Target a specific session ULID to resume context from.
    #[arg(long)]
    pub session: Option<String>,

    /// Generate a compact summary of the resume context instead of full output.
    #[arg(long)]
    pub compact: bool,

    /// Output the complete context including extensive historical logs and file paths.
    #[arg(long)]
    pub full: bool,

    /// Automatically start a new session after outputting the context.
    #[arg(long)]
    pub start_session: bool,

    /// Include the uncommitted Git diff in the output context.
    #[arg(long)]
    pub include_diff: bool,

    /// Limit the number of historical events returned in the context.
    #[arg(long)]
    pub max_events: Option<u64>,
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: resume
// ═══════════════════════════════════════════════════════════════════════════

pub fn handle_resume(
    args: &ResumeArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    let mut runtime = try_open_runtime(ctx)?;
    let project_id = &runtime.config.project.id;
    let conn = runtime.database.connection_mut();

    let task_repo = SqliteTaskRepository::new(conn);
    let session_repo = SqliteSessionRepository::new(conn);
    let checkpoint_repo = SqliteCheckpointRepository::new(conn);
    let progress_repo = SqliteProgressRepository::new(conn);
    let event_repo = SqliteEventRepository::new(conn);

    let sessions = session_repo.list(project_id).map_err(|e| e.exit_code)?;
    let current_session = sessions
        .iter()
        .find(|s| matches!(s.state, carryctx::domain::session::SessionState::Active));

    let current_task = if let Some(task_ref) = &args.task {
        task_repo
            .find_by_display_id(project_id, task_ref)
            .map_err(|e| e.exit_code)?
            .or_else(|| task_repo.find_by_id(project_id, task_ref).ok().flatten())
    } else if let Some(session) = current_session {
        session
            .task_id
            .as_ref()
            .and_then(|tid| task_repo.find_by_id(project_id, tid).ok().flatten())
    } else {
        None
    };

    let latest_checkpoint = current_task.as_ref().and_then(|t| {
        checkpoint_repo
            .find_latest_for_task(project_id, &t.id)
            .ok()
            .flatten()
    });

    let progress = current_task.as_ref().map(|t| {
        progress_repo
            .list(&ProgressFilter {
                project_id: project_id.to_string(),
                task_id: t.id.clone(),
                include_removed: false,
            })
            .ok()
            .unwrap_or_default()
    });

    let recent_events = event_repo
        .list(&EventFilter {
            project_id: project_id.to_string(),
            task_id: current_task.as_ref().map(|t| t.id.clone()),
            agent_id: None,
            session_id: None,
            event_type: None,
            since: None,
            until: None,
            limit: args.max_events.or(Some(10)),
        })
        .map_err(|e| e.exit_code)?;

    let data = serde_json::json!({
        "projectId": project_id,
        "currentSession": current_session,
        "currentTask": current_task,
        "latestCheckpoint": latest_checkpoint,
        "progress": progress,
        "recentEvents": recent_events,
        "branch": runtime.git_project.branch,
        "head": runtime.git_project.head,
    });

    render_and_print("resume", Ok(data), is_json, ctx.quiet)
}
