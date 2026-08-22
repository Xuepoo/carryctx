use crate::*;
use carryctx::application::runtime::InvocationContext;
use carryctx::error::ExitCode;
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
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    let mut runtime = try_open_runtime(ctx)?;
    let project_id = &runtime.config.project.id;
    let conn = runtime.database.connection_mut();

    // Resolve current task
    let current_task = {
        let uow =
            carryctx::adapter::unit_of_work::UnitOfWork::begin(conn).map_err(|e| e.exit_code)?;
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
        let event_limit = args
            .max_events
            .or_else(|| (args.compact && !args.full).then_some(runtime.config.context.max_events));
        let event_since = args.since.clone().or_else(|| {
            (args.compact && !args.full).then_some(runtime.config.context.lookback.clone())
        });
        event_repo
            .list(&EventFilter {
                project_id: project_id.to_string(),
                task_id: current_task.as_ref().map(|t| t.id.clone()),
                agent_id: None,
                session_id: None,
                event_type: None,
                since: event_since,
                until: None,
                limit: event_limit,
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

/// Keep the actionable progress needed to resume while dropping historical
/// records and storage-only fields from the default agent context.
fn compact_progress(items: Vec<carryctx::repository::progress::ProgressItemRecord>) -> Value {
    let records = items
        .into_iter()
        .filter(|item| {
            matches!(
                item.status,
                carryctx::domain::progress::ProgressStatus::Open
            )
        })
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

fn compact_task(task: carryctx::repository::task::TaskRecord) -> Value {
    serde_json::json!({
        "display_id": task.display_id,
        "title": task.title,
        "description": task.description.map(|description| truncate(&description, 500)),
        "status": task.status,
        "priority": task.priority,
    })
}

fn compact_event(event: carryctx::repository::event::EventRecord) -> Value {
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
    use carryctx::domain::progress::{ProgressStatus, ProgressType};
    use carryctx::repository::event::EventRecord;
    use carryctx::repository::progress::ProgressItemRecord;

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
}
