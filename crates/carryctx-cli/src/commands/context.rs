use crate::adapter::sqlite_repos::{
    SqliteAgentRepository, SqliteDecisionRepository, SqliteDependencyRepository,
    SqliteEventRepository, SqliteProgressRepository, SqliteTaskRepository,
};
use crate::application::runtime::{InvocationContext, OutputFormat, ProjectRuntime};
use crate::cli::{open_runtime_or_report, render_and_print, render_and_print_with_warnings};
use crate::commands::print_markdown_result;
use crate::error::ExitCode;
use crate::repository::event::EventFilter;
use crate::repository::progress::ProgressFilter;
use crate::repository::{
    AgentRepository, DecisionRepository, DependencyRepository, EventRepository, ProgressRepository,
    TaskRepository,
};
use clap::Parser;
use serde_json::Value;

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
    pre_opened: Option<ProjectRuntime>,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    // Reuse the dispatcher's pre-opened runtime when available; a second
    // open only happens (and reports) when that failed.
    let mut runtime = match pre_opened {
        Some(runtime) => runtime,
        None => open_runtime_or_report(ctx, "context")?,
    };
    let project_id = &runtime.config.project.id;
    let conn = runtime.database.connection_mut();

    // Resolve current task
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

    let event_repo = SqliteEventRepository::new(conn);
    let decision_repo = SqliteDecisionRepository::new(conn);
    let progress_repo = SqliteProgressRepository::new(conn);
    let graph_repo = crate::repository::graph::GraphRepository::new(conn);

    // Issue #105: secondary query failures used to be collapsed into
    // confident empty output (`.ok().unwrap_or_default()`); they must surface
    // as warnings instead.
    let mut warnings: Vec<String> = Vec::new();

    let events = if args.include_events || args.full {
        let event_limit = args
            .max_events
            .or_else(|| (args.compact && !args.full).then_some(runtime.config.context.max_events));
        let event_since = args.since.clone().or_else(|| {
            (args.compact && !args.full).then_some(runtime.config.context.lookback.clone())
        });
        match event_repo.list(&EventFilter {
            project_id: project_id.to_string(),
            task_id: current_task.as_ref().map(|t| t.id.clone()),
            agent_id: None,
            session_id: None,
            event_type: None,
            since: event_since,
            until: None,
            limit: event_limit,
        }) {
            Ok(events) => events,
            Err(e) => {
                warnings.push(format!("events query failed: {}", e.message));
                vec![]
            }
        }
    } else {
        vec![]
    };

    let decisions = if args.include_decisions || args.full {
        match decision_repo.list(project_id) {
            Ok(decisions) => decisions,
            Err(e) => {
                warnings.push(format!("decisions query failed: {}", e.message));
                vec![]
            }
        }
    } else {
        vec![]
    };

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
    let progress = progress.map(|items| {
        if args.compact && !args.full {
            compact_progress(items)
        } else {
            serde_json::to_value(items).unwrap_or_default()
        }
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
                            .any(|n: &crate::domain::graph::GraphNode| n.id == node.id)
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
                    .any(|n: &crate::domain::graph::GraphNode| n.id == file_node.id)
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
                                .any(|n: &crate::domain::graph::GraphNode| n.id == node.id)
                            {
                                context_graph_nodes.push(node);
                            }
                        }
                        // Deduplicate edges
                        let already = context_graph_edges.iter().any(
                            |e: &crate::domain::graph::GraphEdge| {
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

    let related_tasks = if (args.include_related_tasks || args.full)
        && let Some(t) = current_task.as_ref()
    {
        let dep_repo = SqliteDependencyRepository::new(conn);
        let task_repo = SqliteTaskRepository::new(conn);
        let agent_repo = SqliteAgentRepository::new(conn);

        let outgoing = dep_repo
            .list_for_task(project_id, &t.id)
            .unwrap_or_default();
        let all_edges = dep_repo
            .list_all_for_project(project_id)
            .unwrap_or_default();
        let incoming: Vec<&crate::domain::dependency::DependencyEdge> = all_edges
            .iter()
            .filter(|e| e.prerequisite_id == t.id)
            .collect();

        let mut depends_on = Vec::new();
        for edge in &outgoing {
            if let Ok(Some(prereq)) = task_repo.find_by_id(project_id, &edge.prerequisite_id) {
                let owner = prereq.owner_agent_id.as_deref().and_then(|id| {
                    agent_repo
                        .find_by_id(project_id, id)
                        .ok()
                        .flatten()
                        .map(|a| a.name)
                        .or_else(|| Some(id.to_string()))
                });
                if args.compact && !args.full {
                    depends_on.push(serde_json::json!({
                        "display_id": prereq.display_id,
                        "title": truncate(&prereq.title, 100),
                        "status": prereq.status,
                        "owner": owner,
                    }));
                } else {
                    depends_on.push(serde_json::json!({
                        "id": prereq.id,
                        "display_id": prereq.display_id,
                        "title": prereq.title,
                        "status": prereq.status,
                        "owner": owner,
                        "kind": edge.kind,
                    }));
                }
            }
        }

        let mut blocks = Vec::new();
        for edge in &incoming {
            if let Ok(Some(dep)) = task_repo.find_by_id(project_id, &edge.task_id) {
                let owner = dep.owner_agent_id.as_deref().and_then(|id| {
                    agent_repo
                        .find_by_id(project_id, id)
                        .ok()
                        .flatten()
                        .map(|a| a.name)
                        .or_else(|| Some(id.to_string()))
                });
                if args.compact && !args.full {
                    blocks.push(serde_json::json!({
                        "display_id": dep.display_id,
                        "title": truncate(&dep.title, 100),
                        "status": dep.status,
                        "owner": owner,
                    }));
                } else {
                    blocks.push(serde_json::json!({
                        "id": dep.id,
                        "display_id": dep.display_id,
                        "title": dep.title,
                        "status": dep.status,
                        "owner": owner,
                        "kind": edge.kind,
                    }));
                }
            }
        }

        Some(serde_json::json!({
            "depends_on": depends_on,
            "blocks": blocks,
        }))
    } else {
        None
    };

    let current_task = current_task.map(|task| {
        if args.compact && !args.full {
            compact_task(task)
        } else {
            serde_json::to_value(task).unwrap_or_default()
        }
    });
    let events = if args.compact && !args.full {
        Value::Array(events.into_iter().map(compact_event).collect())
    } else {
        serde_json::to_value(events).unwrap_or_default()
    };

    let mut data = serde_json::json!({
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
    if let Some(ref rt) = related_tasks {
        data["related_tasks"] = rt.clone();
        data["relatedTasks"] = rt.clone();
    }

    // Persist to --output before rendering stdout: a failed write must fail
    // the command with the standard error path instead of exiting 0 after an
    // already-printed success document (issue #105).
    if let Some(output_path) = &args.output {
        let write_result = serde_json::to_string_pretty(&data)
            .map_err(|e| crate::error::CarryCtxError::io_error(format!("{e}")))
            .and_then(|json| {
                std::fs::write(output_path, &json).map_err(|e| {
                    crate::error::CarryCtxError::io_error(format!(
                        "Failed to write context output to {output_path}: {e}"
                    ))
                })
            });
        if let Err(err) = write_result {
            // JSON mode: standard error envelope on stderr. Text mode:
            // readable Error [CODE] line (same contract as runtime-open
            // failures).
            if is_json {
                return render_and_print::<serde_json::Value>("context", Err(err), true, ctx.quiet);
            }
            if !ctx.quiet {
                eprintln!("Error [{}]: {}", err.code, err.message);
            }
            return Err(err.exit_code);
        }
    }

    if ctx.format == OutputFormat::Markdown {
        return print_markdown_result("context", Ok(&data), render_context_markdown, ctx);
    }

    render_and_print_with_warnings("context", Ok(data), is_json, ctx.quiet, warnings)
}

fn render_context_markdown(val: &serde_json::Value) -> String {
    let project_name = val
        .get("projectName")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let project_id = val.get("projectId").and_then(|v| v.as_str()).unwrap_or("");
    let branch = val.get("branch").and_then(|v| v.as_str()).unwrap_or("");
    let head = val.get("head").and_then(|v| v.as_str()).unwrap_or("");

    let mut out = format!("# Context: {project_name} ({project_id})\n\n");
    out.push_str(&format!("- **Branch**: {branch}\n"));
    out.push_str(&format!("- **Head**: {head}\n\n"));

    // Current Task
    out.push_str("## Current Task\n\n");
    if let Some(task) = val.get("currentTask").filter(|v| !v.is_null()) {
        let display_id = task
            .get("display_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let title = task.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let status = task.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let priority = task.get("priority").and_then(|v| v.as_str()).unwrap_or("");
        out.push_str(&format!(
            "- **{display_id}**: {title} (Status: `{status}`, Priority: `{priority}`)\n"
        ));
        if let Some(desc) = task
            .get("description")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            out.push_str(&format!("\n{desc}\n"));
        }
    } else {
        out.push_str("No active task.\n");
    }
    out.push('\n');

    // Related Tasks
    if let Some(rt) = val.get("related_tasks").filter(|v| !v.is_null()) {
        out.push_str("## Related Tasks\n\n");

        out.push_str("### Depends On (Prerequisites)\n");
        if let Some(depends) = rt
            .get("depends_on")
            .and_then(|v| v.as_array())
            .filter(|a| !a.is_empty())
        {
            for dep in depends {
                let id = dep.get("display_id").and_then(|v| v.as_str()).unwrap_or("");
                let title = dep.get("title").and_then(|v| v.as_str()).unwrap_or("");
                let status = dep.get("status").and_then(|v| v.as_str()).unwrap_or("");
                let owner = dep
                    .get("owner")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unassigned");
                out.push_str(&format!(
                    "- **{id}**: {title} (`{status}`, owner: {owner})\n"
                ));
            }
        } else {
            out.push_str("- None\n");
        }
        out.push('\n');

        out.push_str("### Blocks (Dependents)\n");
        if let Some(blocks) = rt
            .get("blocks")
            .and_then(|v| v.as_array())
            .filter(|a| !a.is_empty())
        {
            for dep in blocks {
                let id = dep.get("display_id").and_then(|v| v.as_str()).unwrap_or("");
                let title = dep.get("title").and_then(|v| v.as_str()).unwrap_or("");
                let status = dep.get("status").and_then(|v| v.as_str()).unwrap_or("");
                let owner = dep
                    .get("owner")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unassigned");
                out.push_str(&format!(
                    "- **{id}**: {title} (`{status}`, owner: {owner})\n"
                ));
            }
        } else {
            out.push_str("- None\n");
        }
        out.push('\n');
    }

    // Progress
    if let Some(progress) = val
        .get("progress")
        .and_then(|v| v.as_array())
        .filter(|a| !a.is_empty())
    {
        out.push_str("## Progress\n\n");
        for item in progress {
            let display_id = item
                .get("display_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let content = item.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let status = item.get("status").and_then(|v| v.as_str()).unwrap_or("");
            let marker = if status == "completed" { "[x]" } else { "[ ]" };
            out.push_str(&format!("- {marker} **{display_id}**: {content}\n"));
        }
        out.push('\n');
    }

    // Decisions
    if let Some(decisions) = val
        .get("decisions")
        .and_then(|v| v.as_array())
        .filter(|a| !a.is_empty())
    {
        out.push_str("## Decisions\n\n");
        for d in decisions {
            let display_id = d.get("display_id").and_then(|v| v.as_str()).unwrap_or("");
            let title = d.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let status = d.get("status").and_then(|v| v.as_str()).unwrap_or("");
            out.push_str(&format!("- **{display_id}**: {title} (`{status}`)\n"));
        }
        out.push('\n');
    }

    // Events
    if let Some(events) = val
        .get("events")
        .and_then(|v| v.as_array())
        .filter(|a| !a.is_empty())
    {
        out.push_str("## Recent Events\n\n");
        for ev in events {
            let event_type = ev.get("event_type").and_then(|v| v.as_str()).unwrap_or("");
            let occurred_at = ev.get("occurred_at").and_then(|v| v.as_str()).unwrap_or("");
            out.push_str(&format!(
                "- `{event_type}` at {occurred_at}: {}\n",
                ev.get("payload").unwrap_or(&serde_json::Value::Null)
            ));
        }
        out.push('\n');
    }

    // Context Graph
    if let Some(graph) = val.get("contextGraph").filter(|v| !v.is_null()) {
        let node_count = graph.get("nodeCount").and_then(|v| v.as_u64()).unwrap_or(0);
        let edge_count = graph.get("edgeCount").and_then(|v| v.as_u64()).unwrap_or(0);
        if node_count > 0 || edge_count > 0 {
            out.push_str("## Context Graph\n\n");
            out.push_str(&format!("- Nodes: {node_count}, Edges: {edge_count}\n"));
        }
    }

    out
}

/// Keep the actionable progress needed to resume while dropping historical
/// records and storage-only fields from the default agent context.
fn compact_progress(items: Vec<crate::repository::progress::ProgressItemRecord>) -> Value {
    let records = items
        .into_iter()
        .filter(|item| matches!(item.status, crate::domain::progress::ProgressStatus::Open))
        .map(|item| {
            serde_json::json!({
                "display_id": item.display_id,
                "item_type": item.item_type,
                "status": item.status,
                "content": item.content,
                "position": item.position,
            })
        })
        .collect();
    Value::Array(records)
}

fn compact_task(task: crate::repository::task::TaskRecord) -> Value {
    serde_json::json!({
        "display_id": task.display_id,
        "title": task.title,
        "description": task.description.map(|description| truncate(&description, 500)),
        "status": task.status,
        "priority": task.priority,
    })
}

fn compact_event(event: crate::repository::event::EventRecord) -> Value {
    serde_json::json!({
        "event_type": event.event_type,
        "payload": compact_payload(event.payload),
        "occurred_at": event.occurred_at,
    })
}

fn compact_payload(payload: Value) -> Value {
    match payload {
        Value::String(value) => Value::String(truncate(&value, 500)),
        Value::Array(values) => Value::Array(values.into_iter().map(compact_payload).collect()),
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, compact_payload(value)))
                .collect(),
        ),
        value => value,
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::{compact_event, compact_progress, truncate};
    use crate::domain::progress::{ProgressStatus, ProgressType};
    use crate::repository::event::EventRecord;
    use crate::repository::progress::ProgressItemRecord;

    fn item(id: &str, status: ProgressStatus, content: &str) -> ProgressItemRecord {
        ProgressItemRecord {
            id: format!("id-{id}"),
            display_id: id.to_string(),
            project_id: "project".to_string(),
            task_id: "task".to_string(),
            source_session_id: Some("session".to_string()),
            item_type: ProgressType::Todo,
            status,
            content: content.to_string(),
            position: 1,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            completed_at: None,
            removed_at: None,
        }
    }

    #[test]
    fn compact_progress_keeps_open_resume_items_and_required_fields() {
        let value = compact_progress(vec![
            item("ITEM-0001", ProgressStatus::Completed, "finished"),
            item("ITEM-0002", ProgressStatus::Open, "continue this"),
        ]);

        assert_eq!(value.as_array().unwrap().len(), 1);
        let record = &value[0];
        assert_eq!(record["display_id"], "ITEM-0002");
        assert_eq!(record["content"], "continue this");
        assert_eq!(record["status"], "open");
        assert!(record.get("updated_at").is_none());
        assert!(record.get("project_id").is_none());
    }

    #[test]
    fn compact_progress_is_smaller_for_historical_items() {
        let items = (0..100)
            .map(|index| {
                item(
                    &format!("ITEM-{index:04}"),
                    ProgressStatus::Completed,
                    "a completed item with storage metadata that compact mode should omit",
                )
            })
            .collect::<Vec<_>>();
        let full = serde_json::to_vec(&items).unwrap();
        let compact_value = compact_progress(items);
        let compact = serde_json::to_vec(&compact_value).unwrap();

        assert!(compact.len() < full.len() / 10);
    }

    #[test]
    fn compact_event_keeps_resume_fields_and_drops_storage_identity() {
        let event = EventRecord {
            id: "event-id".to_string(),
            project_id: "project-id".to_string(),
            event_type: "progress.created".to_string(),
            actor_agent_id: Some("agent-id".to_string()),
            session_id: Some("session-id".to_string()),
            task_id: Some("task-id".to_string()),
            payload: serde_json::json!({"content": "continue"}),
            occurred_at: "2026-01-01T00:00:00Z".to_string(),
        };

        let value = compact_event(event);
        assert_eq!(value["event_type"], "progress.created");
        assert_eq!(value["payload"]["content"], "continue");
        assert!(value.get("id").is_none());
        assert!(value.get("task_id").is_none());
    }

    #[test]
    fn truncate_adds_marker_only_when_needed() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("abcdefghij", 10), "abcdefghij");
        assert_eq!(truncate("abcdefghijk", 10), "abcdefghij...");
    }

    #[test]
    fn render_context_markdown_includes_related_tasks_and_details() {
        let val = serde_json::json!({
            "projectName": "test-proj",
            "projectId": "01ABCD",
            "branch": "main",
            "head": "deadbeef",
            "currentTask": {
                "display_id": "CTX-0002",
                "title": "Middle Task",
                "status": "in_progress",
                "priority": "high",
                "description": "Fix the issue"
            },
            "related_tasks": {
                "depends_on": [
                    {
                        "display_id": "CTX-0001",
                        "title": "Prereq Task",
                        "status": "completed",
                        "owner": "alice"
                    }
                ],
                "blocks": [
                    {
                        "display_id": "CTX-0003",
                        "title": "Downstream Task",
                        "status": "ready",
                        "owner": "unassigned"
                    }
                ]
            },
            "progress": [
                {
                    "display_id": "ITEM-0001",
                    "content": "step 1 done",
                    "status": "completed"
                },
                {
                    "display_id": "ITEM-0002",
                    "content": "step 2 in progress",
                    "status": "open"
                }
            ],
            "decisions": [
                {
                    "display_id": "ADR-0001",
                    "title": "Use SQLite",
                    "status": "accepted"
                }
            ],
            "contextGraph": {
                "nodeCount": 2,
                "edgeCount": 1
            }
        });

        let md = super::render_context_markdown(&val);
        assert!(md.contains("# Context: test-proj (01ABCD)"));
        assert!(
            md.contains("- **CTX-0002**: Middle Task (Status: `in_progress`, Priority: `high`)")
        );
        assert!(md.contains("## Related Tasks"));
        assert!(md.contains("### Depends On (Prerequisites)"));
        assert!(md.contains("- **CTX-0001**: Prereq Task (`completed`, owner: alice)"));
        assert!(md.contains("### Blocks (Dependents)"));
        assert!(md.contains("- **CTX-0003**: Downstream Task (`ready`, owner: unassigned)"));
        assert!(md.contains("## Progress"));
        assert!(md.contains("- [x] **ITEM-0001**: step 1 done"));
        assert!(md.contains("- [ ] **ITEM-0002**: step 2 in progress"));
        assert!(md.contains("## Decisions"));
        assert!(md.contains("- **ADR-0001**: Use SQLite (`accepted`)"));
        assert!(md.contains("## Context Graph"));
        assert!(md.contains("- Nodes: 2, Edges: 1"));
    }

    #[test]
    fn render_context_markdown_handles_empty_tasks_and_no_related_tasks() {
        let val = serde_json::json!({
            "projectName": "empty-proj",
            "projectId": "01EMPTY",
            "branch": "main",
            "head": "000000",
            "currentTask": null,
            "contextGraph": {
                "nodeCount": 0,
                "edgeCount": 0
            }
        });

        let md = super::render_context_markdown(&val);
        assert!(md.contains("# Context: empty-proj (01EMPTY)"));
        assert!(md.contains("No active task."));
        assert!(!md.contains("## Related Tasks"));
        assert!(!md.contains("## Progress"));
        assert!(!md.contains("## Decisions"));
        assert!(!md.contains("## Context Graph"));
    }
}
