use crate::{check_dry_run_envelope, open_runtime_or_report, render_and_print};
use carryctx::application::runtime::{InvocationContext, ProjectRuntime};
use carryctx::domain::graph::{GraphEdge, GraphNode};
use carryctx::error::ExitCode;
use carryctx::output::{OutputSink, render_json};
use chrono::Utc;
use clap::{Args, Parser, Subcommand};
use serde_json::json;

#[derive(Parser, Debug)]
pub struct GraphArgs {
    #[command(subcommand)]
    pub command: GraphSubcommands,
}

#[derive(Subcommand, Debug)]
pub enum GraphSubcommands {
    /// List all edges connected to a specific node
    Edges(GraphEdgesArgs),
    /// Add a new node to the context graph
    AddNode(AddNodeArgs),
    /// Link two nodes with an edge
    Link(LinkArgs),
    /// Automatically extract depends_on edges from a file
    ExtractDeps(ExtractDepsArgs),
    /// Scan all git-tracked files and extract dependency edges into the graph
    Scan(ScanArgs),
    /// Export context graph to Mermaid, DOT, ASCII, or JSON format
    Export(ExportArgs),
}

#[derive(Args, Debug)]
pub struct GraphEdgesArgs {
    #[arg(help = "The ULID of the node")]
    pub id: String,
}

#[derive(Args, Debug)]
pub struct AddNodeArgs {
    #[arg(long, help = "Type of node (e.g., file, module, decision)")]
    pub node_type: String,
    #[arg(long, help = "Name of the node")]
    pub name: String,
    #[arg(long, help = "Description of the node")]
    pub description: Option<String>,
}

#[derive(Args, Debug)]
pub struct LinkArgs {
    #[arg(help = "Source node ULID")]
    pub source: String,
    #[arg(help = "Target node ULID")]
    pub target: String,
    pub relation: String,
}

#[derive(Args, Debug)]
pub struct ExtractDepsArgs {
    #[arg(help = "The file path to extract dependencies from")]
    pub file: String,
}

#[derive(Args, Debug)]
pub struct ScanArgs {
    /// Directory to scan (defaults to the repository root)
    #[arg(long, default_value = ".")]
    pub dir: String,

    /// Comma-separated list of file extensions to include
    #[arg(long, default_value = "rs,ts,js,tsx,jsx")]
    pub ext: String,

    /// Print what would be scanned without writing to the database
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args, Debug)]
pub struct ExportArgs {
    /// Format to export graph (mermaid, dot, ascii, json)
    #[arg(
        short = 't',
        long = "type",
        alias = "format",
        default_value = "mermaid"
    )]
    pub export_format: String,

    /// Output file path (.mmd, .dot, .png, .svg, .json, .txt)
    #[arg(short, long)]
    pub output: Option<String>,

    /// Filter graph nodes by type (e.g. file, task)
    #[arg(long)]
    pub node_type: Option<String>,

    /// Focus on a specific node (by name or ULID) and export its subgraph
    #[arg(long)]
    pub focus: Option<String>,

    /// Traversal depth when using --focus (default: 1)
    #[arg(long, default_value_t = 1)]
    pub depth: usize,

    /// Aggregate graph nodes into module-level clusters (e.g. src/commands, src/domain)
    #[arg(long)]
    pub compact: bool,

    /// Directly render output in ASCII diagram format
    #[arg(long)]
    pub ascii: bool,
}

/// Stable command label for a graph subcommand, matching the labels used by
/// the per-arm `render_json` calls.
fn graph_command_label(command: &GraphSubcommands) -> &'static str {
    match command {
        GraphSubcommands::Edges(_) => "graph.edges",
        GraphSubcommands::AddNode(_) => "graph.add-node",
        GraphSubcommands::Link(_) => "graph.link",
        GraphSubcommands::ExtractDeps(_) => "graph.extract-deps",
        GraphSubcommands::Scan(_) => "graph.scan",
        GraphSubcommands::Export(_) => "graph.export",
    }
}

pub fn handle_graph(
    args: &GraphArgs,
    pre_opened: Option<ProjectRuntime>,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    // The global --dry-run promise is "no database changes": mutating graph
    // subcommands must be gated before the runtime opens, while read-only
    // subcommands (edges/export) render normally.
    let mutating = matches!(
        &args.command,
        GraphSubcommands::AddNode(_)
            | GraphSubcommands::Link(_)
            | GraphSubcommands::ExtractDeps(_)
            | GraphSubcommands::Scan(_)
    );
    if mutating {
        if let Some(result) = check_dry_run_envelope(
            ctx,
            graph_command_label(&args.command),
            &format!("graph {:?}", args.command),
        ) {
            return result;
        }
    }

    // Reuse the dispatcher's pre-opened runtime when available; a second
    // open only happens (and reports) when that failed.
    let mut runtime = match pre_opened {
        Some(runtime) => runtime,
        None => open_runtime_or_report(ctx, "graph")?,
    };

    if mutating {
        let project_id = runtime.config.project.id.clone();
        let conn = runtime.database.connection_mut();
        return run_mutating_graph(&args.command, conn, &project_id, ctx, is_json);
    }

    let repo = carryctx::repository::graph::GraphRepository::new(runtime.database.connection());

    match &args.command {
        GraphSubcommands::Edges(cmd) => {
            let result = match repo.get_node(&cmd.id) {
                Ok(Some(_)) => repo.get_edges_for_node(&cmd.id),
                Ok(None) => Err(carryctx::error::CarryCtxError::resource_not_found(format!(
                    "'{}' is not a Context Graph node ID. Note: task/agent/session ULIDs are a separate ID space from graph nodes; use `carryctx task show <TASK_REF>` to see a task's dependencies instead.",
                    cmd.id
                ))),
                Err(e) => Err(e),
            };
            let (out, sink, code) = render_json("graph.edges", result.as_ref(), is_json);
            match sink {
                OutputSink::Stdout => println!("{out}"),
                OutputSink::Stderr => eprintln!("{out}"),
            }
            if code == ExitCode::Success {
                Ok(code)
            } else {
                Err(code)
            }
        }
        GraphSubcommands::Export(cmd) => {
            use carryctx::application::export_graph::{
                GraphExportFormat, export_graph, render_image_to_file,
            };
            use std::str::FromStr;

            let fmt_str = if cmd.ascii {
                "ascii"
            } else {
                cmd.export_format.as_str()
            };

            let result: Result<serde_json::Value, carryctx::error::CarryCtxError> = (|| {
                let parsed_format = GraphExportFormat::from_str(fmt_str)?;
                let content = export_graph(
                    &repo,
                    parsed_format,
                    cmd.node_type.as_deref(),
                    cmd.focus.as_deref(),
                    cmd.depth,
                    cmd.compact,
                )?;

                if let Some(out_path) = &cmd.output {
                    render_image_to_file(&content, parsed_format, out_path)?;
                    Ok(json!({
                        "status": "success",
                        "format": fmt_str,
                        "outputPath": out_path,
                    }))
                } else {
                    Ok(json!({
                        "status": "success",
                        "format": fmt_str,
                        "content": content,
                    }))
                }
            })(
            );

            match result {
                Ok(data) => {
                    if is_json {
                        let (out, _, code) = render_json("graph.export", Ok(data), true);
                        println!("{out}");
                        Ok(code)
                    } else if let Some(content) = data["content"].as_str() {
                        print!("{content}");
                        Ok(ExitCode::Success)
                    } else if let Some(path) = data["outputPath"].as_str() {
                        println!("Successfully exported graph to {path}");
                        Ok(ExitCode::Success)
                    } else {
                        Ok(ExitCode::Success)
                    }
                }
                Err(err) => render_and_print("graph.export", Err::<(), _>(err), is_json, ctx.quiet),
            }
        }
        GraphSubcommands::AddNode(_)
        | GraphSubcommands::Link(_)
        | GraphSubcommands::ExtractDeps(_)
        | GraphSubcommands::Scan(_) => {
            unreachable!("mutating graph subcommands are handled by run_mutating_graph")
        }
    }
}

/// Execute a mutating graph subcommand inside a UnitOfWork: the mutation rows
/// and their audit events (`graph.node_added`, `graph.edge_added`,
/// `graph.deps_extracted`, `graph.scanned`) commit in one transaction, or the
/// UnitOfWork Drop rolls both back together (issue #99).
fn run_mutating_graph(
    command: &GraphSubcommands,
    conn: &mut rusqlite::Connection,
    project_id: &str,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    use carryctx::adapter::sqlite_repos::SqliteEventRepository;

    /// Append an audit event describing a committed graph mutation.
    fn append_graph_event(
        event_repo: &SqliteEventRepository,
        project_id: &str,
        actor_agent_id: &Option<String>,
        session_id: Option<&str>,
        event_type: &str,
        payload: serde_json::Value,
        occurred_at: String,
    ) -> Result<(), carryctx::error::CarryCtxError> {
        use carryctx::repository::event::{EventRepository, NewEvent};
        event_repo
            .append(&NewEvent {
                id: ulid::Ulid::generate().to_string(),
                project_id: project_id.to_string(),
                event_type: event_type.into(),
                actor_agent_id: actor_agent_id.clone(),
                session_id: session_id.map(str::to_string),
                task_id: None,
                payload,
                occurred_at,
            })
            .map(|_| ())
    }

    /// Commit on success; on failure the UnitOfWork Drop rolls the mutation and
    /// any already-appended event rows back together.
    fn commit_graph_uow<T>(
        uow: Option<carryctx::adapter::unit_of_work::UnitOfWork>,
        result: Result<T, carryctx::error::CarryCtxError>,
    ) -> Result<T, carryctx::error::CarryCtxError> {
        match uow {
            Some(uow) => match result {
                Ok(value) => uow.commit().map(|()| value),
                Err(err) => Err(err),
            },
            None => result,
        }
    }

    let actor_agent_id = ctx.agent.clone();
    let mut uow =
        Some(carryctx::adapter::unit_of_work::UnitOfWork::begin(conn).map_err(|e| e.exit_code)?);

    match command {
        GraphSubcommands::AddNode(cmd) => {
            let compute = || -> Result<GraphNode, carryctx::error::CarryCtxError> {
                let repo = carryctx::repository::graph::GraphRepository::new(
                    uow.as_ref().expect("open").connection(),
                );
                let event_repo =
                    SqliteEventRepository::new(uow.as_ref().expect("open").connection());
                let id = ulid::Ulid::generate().to_string();
                let now = Utc::now().to_rfc3339();

                let node = GraphNode::new(
                    &id,
                    &cmd.node_type,
                    &cmd.name,
                    cmd.description.clone(),
                    json!({}),
                    now,
                );

                repo.insert_node(&node)?;
                append_graph_event(
                    &event_repo,
                    project_id,
                    &actor_agent_id,
                    ctx.session.as_deref(),
                    "graph.node_added",
                    json!({
                        "nodeId": node.id,
                        "nodeType": node.node_type,
                        "name": node.name,
                    }),
                    node.created_at.clone(),
                )?;
                Ok(node)
            };
            let computed = compute();
            let result = commit_graph_uow(uow.take(), computed);
            let (out, sink, code) = render_json("graph.add-node", result.as_ref(), is_json);
            match sink {
                OutputSink::Stdout => println!("{out}"),
                OutputSink::Stderr => eprintln!("{out}"),
            }
            if code == ExitCode::Success {
                Ok(code)
            } else {
                Err(code)
            }
        }
        GraphSubcommands::Link(cmd) => {
            let compute = || -> Result<GraphEdge, carryctx::error::CarryCtxError> {
                let repo = carryctx::repository::graph::GraphRepository::new(
                    uow.as_ref().expect("open").connection(),
                );
                let event_repo =
                    SqliteEventRepository::new(uow.as_ref().expect("open").connection());
                let now = Utc::now().to_rfc3339();
                let edge = GraphEdge::new(
                    &cmd.source,
                    &cmd.target,
                    &cmd.relation,
                    now,
                    ctx.agent.clone(),
                    json!({}),
                );

                repo.insert_edge(&edge)?;
                append_graph_event(
                    &event_repo,
                    project_id,
                    &actor_agent_id,
                    ctx.session.as_deref(),
                    "graph.edge_added",
                    json!({
                        "sourceId": edge.source_id,
                        "targetId": edge.target_id,
                        "relation": edge.relation_type,
                    }),
                    edge.created_at.clone(),
                )?;
                Ok(edge)
            };
            let computed = compute();
            let result = commit_graph_uow(uow.take(), computed);
            let (out, sink, code) = render_json("graph.link", result.as_ref(), is_json);
            match sink {
                OutputSink::Stdout => println!("{out}"),
                OutputSink::Stderr => eprintln!("{out}"),
            }
            if code == ExitCode::Success {
                Ok(code)
            } else {
                Err(code)
            }
        }
        GraphSubcommands::ExtractDeps(cmd) => {
            // The use case writes through a repository bound to the UoW
            // connection; a summary audit event covers the import and the
            // whole batch commits atomically.
            let compute = || -> Result<Vec<GraphEdge>, carryctx::error::CarryCtxError> {
                let repo = carryctx::repository::graph::GraphRepository::new(
                    uow.as_ref().expect("open").connection(),
                );
                let event_repo =
                    SqliteEventRepository::new(uow.as_ref().expect("open").connection());
                let created_edges = carryctx::application::extract_deps::extract_deps_for_file(
                    &cmd.file, &repo, ctx,
                )?;
                append_graph_event(
                    &event_repo,
                    project_id,
                    &actor_agent_id,
                    ctx.session.as_deref(),
                    "graph.deps_extracted",
                    json!({
                        "file": cmd.file,
                        "edgesCreated": created_edges.len(),
                    }),
                    Utc::now().to_rfc3339(),
                )?;
                Ok(created_edges)
            };
            let computed = compute();
            let result = commit_graph_uow(uow.take(), computed);
            let (out, sink, code) = render_json("graph.extract-deps", result.as_ref(), is_json);
            match sink {
                OutputSink::Stdout => println!("{out}"),
                OutputSink::Stderr => eprintln!("{out}"),
            }
            if code == ExitCode::Success {
                Ok(code)
            } else {
                Err(code)
            }
        }
        GraphSubcommands::Scan(cmd) => {
            use carryctx::application::scan_graph::{DEFAULT_EXTENSIONS, scan_project};
            use std::path::Path;

            // Parse extensions from comma-separated string
            let ext_owned: Vec<String> = cmd.ext.split(',').map(|s| s.trim().to_string()).collect();
            let extensions: Vec<&str> = ext_owned.iter().map(|s| s.as_str()).collect();

            // Fallback to defaults if empty
            let extensions: &[&str] = if extensions.is_empty() {
                DEFAULT_EXTENSIONS
            } else {
                &extensions
            };

            let dir = Path::new(&cmd.dir);
            let compute = || -> Result<serde_json::Value, carryctx::error::CarryCtxError> {
                let repo = carryctx::repository::graph::GraphRepository::new(
                    uow.as_ref().expect("open").connection(),
                );
                let event_repo =
                    SqliteEventRepository::new(uow.as_ref().expect("open").connection());

                scan_project(dir, extensions, cmd.dry_run, &repo, ctx).and_then(|r| {
                    let errors: Vec<serde_json::Value> = r
                        .errors
                        .iter()
                        .map(|e| json!({ "file": e.file, "error": e.message }))
                        .collect();
                    let summary = json!({
                        "dry_run": cmd.dry_run,
                        "extensions": extensions,
                        "scanned": r.scanned,
                        "skipped": r.skipped,
                        "nodes_created": r.nodes_created,
                        "edges_created": r.edges_created,
                        "error_count": errors.len(),
                        "errors": errors,
                    });
                    if !cmd.dry_run {
                        append_graph_event(
                            &event_repo,
                            project_id,
                            &actor_agent_id,
                            ctx.session.as_deref(),
                            "graph.scanned",
                            json!({
                                "dir": cmd.dir,
                                "nodesCreated": r.nodes_created,
                                "edgesCreated": r.edges_created,
                                "scanned": r.scanned,
                                "errorCount": errors.len(),
                            }),
                            Utc::now().to_rfc3339(),
                        )?;
                    }
                    Ok(summary)
                })
            };
            let computed = compute();
            let result = commit_graph_uow(uow.take(), computed);
            let (out, sink, code) = render_json("graph.scan", result.as_ref(), is_json);
            match sink {
                OutputSink::Stdout => println!("{out}"),
                OutputSink::Stderr => eprintln!("{out}"),
            }
            if code == ExitCode::Success {
                Ok(code)
            } else {
                Err(code)
            }
        }
        GraphSubcommands::Edges(_) | GraphSubcommands::Export(_) => {
            unreachable!("read-only graph subcommands are handled by the caller")
        }
    }
}
