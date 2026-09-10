use crate::adapter::sqlite_repos::{
    SqliteCheckpointRepository, SqliteEventRepository, SqliteProgressRepository,
    SqliteSessionRepository,
};
use crate::application::runtime::{InvocationContext, ProjectRuntime};
use crate::cli::{open_runtime_or_report, render_and_print_with_warnings};
use crate::error::ExitCode;
use crate::repository::event::EventFilter;
use crate::repository::progress::ProgressFilter;
use crate::repository::{
    CheckpointRepository, EventRepository, ProgressRepository, SessionRepository,
};
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
    pre_opened: Option<ProjectRuntime>,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    // Reuse the dispatcher's pre-opened runtime when available; a second
    // open only happens (and reports) when that failed.
    let mut runtime = match pre_opened {
        Some(runtime) => runtime,
        None => open_runtime_or_report(ctx, "resume")?,
    };
    let project_id = &runtime.config.project.id;
    let conn = runtime.database.connection_mut();

    // Resolve current task: explicit --task, current active session, current
    // worktree, or (as a last resort) the agent's single in-progress task.
    let current_task = {
        let uow = crate::adapter::unit_of_work::UnitOfWork::begin(conn).map_err(|e| e.exit_code)?;
        let resolver = crate::application::runtime::CurrentEntityResolver::new(project_id, &uow);
        let cwd = ctx.cwd.to_string_lossy();

        let agent_id = resolver
            .resolve_agent(
                ctx.agent.as_deref(),
                None,
                None,
                runtime.config.agent.default_name.as_deref(),
                runtime.config.agent.default_name.as_deref(),
            )
            .ok()
            .map(|a| a.id);

        let resolved = resolver
            .resolve_task(
                args.task.as_deref().or(ctx.task.as_deref()),
                Some(&cwd),
                agent_id.as_deref(),
            )
            .ok()
            .flatten();

        uow.commit()
            .map_err(|e| crate::error::CarryCtxError::database_error(e.to_string()).exit_code)?;
        resolved
    };

    let session_repo = SqliteSessionRepository::new(conn);
    let checkpoint_repo = SqliteCheckpointRepository::new(conn);
    let progress_repo = SqliteProgressRepository::new(conn);
    let event_repo = SqliteEventRepository::new(conn);

    let sessions = session_repo.list(project_id).map_err(|e| e.exit_code)?;
    let current_session = sessions
        .iter()
        .find(|s| matches!(s.state, crate::domain::session::SessionState::Active));

    // Issue #105: checkpoint/progress query failures used to collapse into
    // confident empty output; surface them as warnings instead.
    let mut warnings: Vec<String> = Vec::new();

    let latest_checkpoint = current_task.as_ref().and_then(|t| {
        match checkpoint_repo.find_latest_for_task(project_id, &t.id) {
            Ok(Some(checkpoint)) => Some(checkpoint),
            Ok(None) => None,
            Err(e) => {
                warnings.push(format!("checkpoint query failed: {}", e.message));
                None
            }
        }
    });

    let progress = current_task.as_ref().map(|t| {
        match progress_repo.list(&ProgressFilter {
            project_id: project_id.to_string(),
            task_id: t.id.clone(),
            include_removed: false,
        }) {
            Ok(items) => items,
            Err(e) => {
                warnings.push(format!("progress query failed: {}", e.message));
                vec![]
            }
        }
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

    render_and_print_with_warnings("resume", Ok(data), is_json, ctx.quiet, warnings)
}
