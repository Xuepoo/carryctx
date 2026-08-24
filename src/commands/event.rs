use crate::*;
use carryctx::adapter::unit_of_work::UnitOfWork;
use carryctx::application;
use carryctx::application::runtime::{InvocationContext, ProjectRuntime};
use carryctx::error::ExitCode;
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
        /// Resume listing after a previous page's opaque next_cursor token
        #[arg(long)]
        cursor: Option<String>,
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
    pre_opened: Option<ProjectRuntime>,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    // Reuse the dispatcher's pre-opened runtime when available; a second
    // open only happens (and reports) when that failed.
    let mut runtime = match pre_opened {
        Some(runtime) => runtime,
        None => open_runtime_or_report(ctx, "event")?,
    };
    let verbose = ctx.verbose || runtime.config.output.verbose;
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
            cursor,
        } => {
            // Resolve agent reference (name or ULID) to ULID for filtering.
            // The local --agent clashes with the global --agent (CARRYCTX_AGENT env),
            // so resolve it here to avoid filtering by raw agent name.
            // Resolve agent reference (name or ULID) to ULID for filtering.
            let resolved_agent_id = agent.as_deref().and_then(|a| {
                if a.is_empty() {
                    None
                } else {
                    resolve_agent_id(project_id, a, conn).ok()
                }
            });
            // Resolve task reference (display ID or ULID) to ULID for filtering.
            let resolved_task_id = task.as_deref().and_then(|t| {
                if t.is_empty() {
                    None
                } else {
                    resolve_task_id(project_id, t, conn).ok()
                }
            });
            let filter = EventFilter {
                project_id: project_id.to_string(),
                task_id: resolved_task_id,
                agent_id: resolved_agent_id,
                session_id: session.clone(),
                event_type: event_type.clone(),
                since: since.clone(),
                until: until.clone(),
                limit: *limit,
            };
            // Keyset pagination lives in the application layer: opaque
            // `(occurred_at, id)` cursor tokens keep bulk transitions that
            // share one timestamp from repeating across pages, and a full
            // page emits a real `next_cursor`.
            let uow = UnitOfWork::begin(conn).map_err(|e| e.exit_code)?;
            let page = resolve_or_render(
                "event.list",
                application::event::list_events(project_id, &filter, cursor.as_deref(), &uow),
                ctx,
                is_json,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )?;

            // Markdown format support
            if ctx.format == carryctx::application::runtime::OutputFormat::Markdown {
                let mut out = String::from("# Events\n\n");
                out.push_str("| Type | Agent | Occurred At |\n");
                out.push_str("|---|---|---|\n");
                for e in &page.events {
                    let agent = e
                        .actor_agent_id
                        .as_deref()
                        .map(|a| truncate_chars(a, 8))
                        .unwrap_or_else(|| "-".to_string());
                    out.push_str(&format!(
                        "| {} | {} | {} |\n",
                        e.event_type,
                        agent,
                        truncate_chars(&e.occurred_at, 19)
                    ));
                }
                if !ctx.quiet {
                    print!("{out}");
                }
                return Ok(ExitCode::Success);
            }

            let result =
                serde_json::json!({"events": page.events, "next_cursor": page.next_cursor});
            render_and_print_entity(
                "event.list",
                Ok(result),
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        EventCommand::Show { event_id } => {
            let uow = UnitOfWork::begin(conn).map_err(|e| e.exit_code)?;
            let result = application::event::show_event(project_id, event_id, &uow);
            render_and_print_entity(
                "event.show",
                result,
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
    }
}
