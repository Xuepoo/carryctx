use crate::render_and_print;
use crate::try_open_runtime;
use carryctx::application::runtime::InvocationContext;
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

pub fn handle_graph(
    args: &GraphArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    let runtime = try_open_runtime(ctx)?;

    let conn = runtime.database.connection();
    let repo = carryctx::repository::GraphRepository::new(conn);

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
            let (out, sink, code) = render_json("graph edges", result.as_ref(), is_json);
            match sink {
                OutputSink::Stdout => println!("{}", out),
                OutputSink::Stderr => eprintln!("{}", out),
            }
            if code == ExitCode::Success {
                Ok(code)
            } else {
                Err(code)
            }
        }
        GraphSubcommands::AddNode(cmd) => {
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

            let result = repo.insert_node(&node).map(|_| node);
            let (out, sink, code) = render_json("graph add-node", result.as_ref(), is_json);
            match sink {
                OutputSink::Stdout => println!("{}", out),
                OutputSink::Stderr => eprintln!("{}", out),
            }
            if code == ExitCode::Success {
                Ok(code)
            } else {
                Err(code)
            }
        }
        GraphSubcommands::Link(cmd) => {
            let now = Utc::now().to_rfc3339();
            let edge = GraphEdge::new(
                &cmd.source,
                &cmd.target,
                &cmd.relation,
                now,
                ctx.agent.clone(),
                json!({}),
            );

            let result = repo.insert_edge(&edge).map(|_| edge);
            let (out, sink, code) = render_json("graph link", result.as_ref(), is_json);
            match sink {
                OutputSink::Stdout => println!("{}", out),
                OutputSink::Stderr => eprintln!("{}", out),
            }
            if code == ExitCode::Success {
                Ok(code)
            } else {
                Err(code)
            }
        }
        GraphSubcommands::ExtractDeps(cmd) => {
            let result =
                carryctx::application::extract_deps::extract_deps_for_file(&cmd.file, &repo, ctx);
            let (out, sink, code) = render_json("graph extract-deps", result.as_ref(), is_json);
            match sink {
                OutputSink::Stdout => println!("{}", out),
                OutputSink::Stderr => eprintln!("{}", out),
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
            let scan_result = scan_project(dir, extensions, cmd.dry_run, &repo, ctx);

            let result = scan_result.map(|r| {
                let errors: Vec<serde_json::Value> = r
                    .errors
                    .iter()
                    .map(|e| json!({ "file": e.file, "error": e.message }))
                    .collect();
                json!({
                    "dryRun": cmd.dry_run,
                    "extensions": extensions,
                    "scanned": r.scanned,
                    "skipped": r.skipped,
                    "nodesCreated": r.nodes_created,
                    "edgesCreated": r.edges_created,
                    "errorCount": errors.len(),
                    "errors": errors,
                })
            });

            let (out, sink, code) = render_json("graph scan", result.as_ref(), is_json);
            match sink {
                OutputSink::Stdout => println!("{}", out),
                OutputSink::Stderr => eprintln!("{}", out),
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
                        let (out, _, code) = render_json("graph export", Ok(data), true);
                        println!("{}", out);
                        Ok(code)
                    } else if let Some(content) = data["content"].as_str() {
                        print!("{}", content);
                        Ok(ExitCode::Success)
                    } else if let Some(path) = data["outputPath"].as_str() {
                        println!("Successfully exported graph to {}", path);
                        Ok(ExitCode::Success)
                    } else {
                        Ok(ExitCode::Success)
                    }
                }
                Err(err) => render_and_print("graph.export", Err::<(), _>(err), is_json, ctx.quiet),
            }
        }
    }
}
