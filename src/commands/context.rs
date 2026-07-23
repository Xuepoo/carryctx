use crate::*;
use carryctx::application::runtime::InvocationContext;
use carryctx::error::ExitCode;
use clap::Parser;

// ── Context ──────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub struct ContextArgs {
    /// Return a compact, token-efficient version of the context.
    #[arg(long)]
    pub compact: bool,

    /// Return the full, extensive project context, bypassing all default truncation limits.
    #[arg(long)]
    pub full: bool,

    /// Explicitly gather context for a specific task ULID.
    #[arg(long)]
    pub task: Option<String>,

    /// Include architectural decisions (ADRs) that are relevant to the current task.
    #[arg(long)]
    pub include_decisions: bool,

    /// Include recent event logs in the context output.
    #[arg(long)]
    pub include_events: bool,

    /// Include brief descriptions of related or blocking tasks.
    #[arg(long)]
    pub include_related_tasks: bool,

    /// Set a strict maximum limit on the number of event logs to include.
    #[arg(long)]
    pub max_events: Option<u64>,

    /// Only retrieve events that occurred since this timestamp or relative duration.
    #[arg(long)]
    pub since: Option<String>,

    /// Output context directly to the specified file path instead of stdout.
    #[arg(long)]
    pub output: Option<String>,
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: context
// ═══════════════════════════════════════════════════════════════════════════

pub fn handle_context(
    args: &ContextArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    let mut runtime = try_open_runtime(ctx)?;
    let project_id = &runtime.config.project.id;
    let conn = runtime.database.connection_mut();

    let task_repo = SqliteTaskRepository::new(conn);
    let event_repo = SqliteEventRepository::new(conn);
    let decision_repo = SqliteDecisionRepository::new(conn);
    let progress_repo = SqliteProgressRepository::new(conn);

    let current_task = args
        .task
        .as_ref()
        .and_then(|t| task_repo.find_by_display_id(project_id, t).ok().flatten())
        .or_else(|| {
            ctx.task
                .as_ref()
                .and_then(|t| task_repo.find_by_id(project_id, t).ok().flatten())
        });

    let events = if args.include_events || args.full {
        event_repo
            .list(&EventFilter {
                project_id: project_id.to_string(),
                task_id: current_task.as_ref().map(|t| t.id.clone()),
                agent_id: None,
                session_id: None,
                event_type: None,
                since: args.since.clone(),
                until: None,
                limit: args.max_events,
            })
            .ok()
            .unwrap_or_default()
    } else {
        vec![]
    };

    let decisions = if args.include_decisions || args.full {
        decision_repo.list(project_id).ok().unwrap_or_default()
    } else {
        vec![]
    };

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

    let graph_repo = carryctx::repository::graph::GraphRepository::new(conn);
    let mut context_graph_nodes = vec![];
    let mut context_graph_edges = vec![];

    if let Some(t) = &current_task {
        if let Ok(edges) = graph_repo.get_edges_for_node(&t.id) {
            context_graph_edges = edges.clone();
            for edge in edges {
                let other_id = if edge.source_id == t.id {
                    &edge.target_id
                } else {
                    &edge.source_id
                };
                if let Ok(Some(node)) = graph_repo.get_node(other_id) {
                    context_graph_nodes.push(node);
                }
            }
        }
    }

    let data = serde_json::json!({
        "projectId": project_id,
        "projectName": runtime.config.project.name,
        "branch": runtime.git_project.branch,
        "head": runtime.git_project.head,
        "currentTask": current_task,
        "events": events,
        "decisions": decisions,
        "progress": progress,
        "contextGraph": {
            "nodes": context_graph_nodes,
            "edges": context_graph_edges,
        }
    });

    let data_for_file = data.clone();
    let exit_code = render_and_print("context", Ok(data), is_json, ctx.quiet);

    if let Some(output_path) = &args.output {
        if let Ok(json) = serde_json::to_string_pretty(&data_for_file) {
            let _ = std::fs::write(output_path, &json);
        }
    }

    exit_code
}
