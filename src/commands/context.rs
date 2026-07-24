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

    /// Include the context graph (file dependency nodes and edges).
    #[arg(long)]
    pub include_graph: bool,

    /// Set a strict maximum limit on the number of event logs to include.
    #[arg(long)]
    pub max_events: Option<u64>,

    /// Only retrieve events that occurred since this timestamp or relative duration.
    #[arg(long)]
    pub since: Option<String>,

    /// Restrict graph output to a specific file path node and its neighbours.
    #[arg(long)]
    pub file: Option<String>,

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

    // Resolve current task
    let current_task = {
        let tx = conn
            .transaction()
            .map_err(|e| carryctx::error::CarryCtxError::database_error(e.to_string()).exit_code)?;
        let uow = carryctx::adapter::unit_of_work::UnitOfWork::new(tx);
        let resolver = carryctx::application::runtime::CurrentEntityResolver::new(project_id, &uow);
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
            .map_err(|e| carryctx::error::CarryCtxError::database_error(e.to_string()).exit_code)?;
        resolved
    };

    let event_repo = SqliteEventRepository::new(conn);
    let decision_repo = SqliteDecisionRepository::new(conn);
    let progress_repo = SqliteProgressRepository::new(conn);
    let graph_repo = carryctx::repository::graph::GraphRepository::new(conn);
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

    // ── Context Graph assembly ─────────────────────────────────────────────
    // Include graph when: --include-graph, --full, or --file is specified.
    let include_graph = args.include_graph || args.full || args.file.is_some();

    let mut context_graph_nodes = vec![];
    let mut context_graph_edges = vec![];

    if include_graph {
        // 1. Task-level: edges directly on the current task node
        if let Some(t) = &current_task {
            if let Ok(edges) = graph_repo.get_edges_for_node(&t.id) {
                for edge in &edges {
                    let other_id = if edge.source_id == t.id {
                        &edge.target_id
                    } else {
                        &edge.source_id
                    };
                    if let Ok(Some(node)) = graph_repo.get_node(other_id) {
                        if !context_graph_nodes
                            .iter()
                            .any(|n: &carryctx::domain::graph::GraphNode| n.id == node.id)
                        {
                            context_graph_nodes.push(node);
                        }
                    }
                }
                context_graph_edges.extend(edges);
            }
        }

        // 2. File-level: if --file is given, show that node and all its neighbours
        if let Some(file_path) = &args.file {
            if let Ok(Some(file_node)) = graph_repo.get_node_by_name_and_type(file_path, "file") {
                if !context_graph_nodes
                    .iter()
                    .any(|n: &carryctx::domain::graph::GraphNode| n.id == file_node.id)
                {
                    context_graph_nodes.push(file_node.clone());
                }
                if let Ok(edges) = graph_repo.get_edges_for_node(&file_node.id) {
                    for edge in &edges {
                        let other_id = if edge.source_id == file_node.id {
                            &edge.target_id
                        } else {
                            &edge.source_id
                        };
                        if let Ok(Some(node)) = graph_repo.get_node(other_id) {
                            if !context_graph_nodes
                                .iter()
                                .any(|n: &carryctx::domain::graph::GraphNode| n.id == node.id)
                            {
                                context_graph_nodes.push(node);
                            }
                        }
                        // Deduplicate edges
                        let already = context_graph_edges.iter().any(
                            |e: &carryctx::domain::graph::GraphEdge| {
                                e.source_id == edge.source_id
                                    && e.target_id == edge.target_id
                                    && e.relation_type == edge.relation_type
                            },
                        );
                        if !already {
                            context_graph_edges.push(edge.clone());
                        }
                    }
                }
            }
        }
    }

    let graph_summary = serde_json::json!({
        "nodeCount": context_graph_nodes.len(),
        "edgeCount": context_graph_edges.len(),
        "nodes": if !args.compact { serde_json::to_value(&context_graph_nodes).unwrap_or_default() }
                 else { serde_json::Value::Array(
                     context_graph_nodes.iter().map(|n| serde_json::json!({"id": n.id, "type": n.node_type, "name": n.name})).collect()
                 )},
        "edges": if !args.compact { serde_json::to_value(&context_graph_edges).unwrap_or_default() }
                 else { serde_json::Value::Array(
                     context_graph_edges.iter().map(|e| serde_json::json!({"src": e.source_id, "dst": e.target_id, "rel": e.relation_type})).collect()
                 )},
    });

    let data = serde_json::json!({
        "projectId": project_id,
        "projectName": runtime.config.project.name,
        "branch": runtime.git_project.branch,
        "head": runtime.git_project.head,
        "currentTask": current_task,
        "events": events,
        "decisions": decisions,
        "progress": progress,
        "contextGraph": graph_summary,
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
