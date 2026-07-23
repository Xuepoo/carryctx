use crate::*;
use carryctx::adapter::unit_of_work::UnitOfWork;
use carryctx::application;
use carryctx::application::runtime::InvocationContext;
use carryctx::error::{CarryCtxError, ExitCode};
use clap::Parser;

// ── Event ────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub enum EventCommand {
    /// List events matching the specified filters
    List {
        /// Filter by associated task ULID
        #[arg(long)]
        task: Option<String>,
        /// Filter by associated agent ULID
        #[arg(long)]
        agent: Option<String>,
        /// Filter by associated session ULID
        #[arg(long)]
        session: Option<String>,
        /// Filter by event type (e.g., TaskTransition, SessionStarted)
        #[arg(long)]
        event_type: Option<String>,
        /// Only show events after this timestamp or relative duration
        #[arg(long)]
        since: Option<String>,
        /// Only show events before this timestamp or relative duration
        #[arg(long)]
        until: Option<String>,
        /// Limit the number of returned events
        #[arg(long)]
        limit: Option<u64>,
    },
    /// Show full raw JSON details for a specific event ULID
    Show { event_id: String },
}

#[derive(Parser, Debug)]
pub struct EventArgs {
    /// Event subcommand to execute
    #[command(subcommand)]
    pub command: EventCommand,
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: event
// ═══════════════════════════════════════════════════════════════════════════

pub fn handle_event(
    args: &EventArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    let mut runtime = try_open_runtime(ctx)?;
    let project_id = &runtime.config.project.id;
    let conn = runtime.database.connection_mut();

    match &args.command {
        EventCommand::List {
            task,
            agent,
            session,
            event_type,
            since,
            until,
            limit,
        } => {
            // Resolve agent reference (name or ULID) to ULID for filtering.
            // The local --agent clashes with the global --agent (CARRYCTX_AGENT env),
            // so resolve it here to avoid filtering by raw agent name.
            let resolved_agent_id = agent.as_deref().and_then(|a| {
                if a.is_empty() {
                    None
                } else {
                    resolve_agent_id(project_id, a, conn).ok()
                }
            });
            let filter = EventFilter {
                project_id: project_id.to_string(),
                task_id: task.clone(),
                agent_id: resolved_agent_id,
                session_id: session.clone(),
                event_type: event_type.clone(),
                since: since.clone(),
                until: until.clone(),
                limit: *limit,
            };
            let repo = carryctx::adapter::sqlite_repos::SqliteEventRepository::new(conn);
            let events = repo.list(&filter).map_err(|e| e.exit_code)?;
            let result = serde_json::json!({"events": events, "next_cursor": null});
            render_and_print("event.list", Ok(result), is_json, ctx.quiet)
        }
        EventCommand::Show { event_id } => {
            let tx = conn
                .transaction()
                .map_err(|e| CarryCtxError::database_error(format!("{e}")).exit_code)?;
            let uow = UnitOfWork::new(tx);
            let result = application::event::show_event(project_id, event_id, &uow);
            render_and_print("event.show", result, is_json, ctx.quiet)
        }
    }
}
