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
            let result = repo.get_edges_for_node(&cmd.id);
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
    }
}
