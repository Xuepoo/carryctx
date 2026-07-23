use std::path::Path;

use clap::Parser;

use carryctx::adapter::config::ConfigLoader;
use carryctx::adapter::git::GitCli;
use carryctx::adapter::sqlite::ProjectDatabase;
use carryctx::adapter::sqlite_repos::*;
use carryctx::adapter::xdg::XdgPaths;
use carryctx::application::runtime::{InvocationContext, OutputFormat, ProjectRuntime};
use carryctx::domain::dependency::DependencyKind;
use carryctx::domain::task::{TaskPriority, TaskStatus};
use carryctx::error::{CarryCtxError, ExitCode};
use carryctx::output;
use carryctx::repository::*;

// ── Global CLI ───────────────────────────────────────────────────────────

/// Persistent project context for coding agents
///
/// CarryCtx is a local-first continuity manager that persists project state, task
/// assignments, session histories, and progress across disconnected Agent sessions
/// and multiple Git worktrees.
#[derive(Parser, Debug)]
#[command(name = "carryctx", version = env!("CARGO_PKG_VERSION"), about, long_about = None)]
pub struct Cli {
    /// Override the path to the project root directory. Defaults to traversing up to find `.git` or `.carryctx/`.
    #[arg(long, global = true)]
    pub project: Option<String>,

    /// Override the path to the carryctx configuration file (carryctx.toml).
    #[arg(long, global = true)]
    pub config: Option<String>,

    /// Profile name to load from the configuration file. Defaults to 'default'.
    #[arg(long, global = true)]
    pub profile: Option<String>,

    /// The name or ULID of the agent acting in this invocation. Required for writing state.
    #[arg(long, global = true, env = "CARRYCTX_AGENT")]
    pub agent: Option<String>,

    /// The ULID of the active session. If not provided, the global or worktree active session is used.
    #[arg(long, global = true, env = "CARRYCTX_SESSION")]
    pub session: Option<String>,

    /// The ULID of the task currently being worked on. Context defaults to this task if set.
    #[arg(long, global = true)]
    pub task: Option<String>,

    /// Output formatting style. 'text' for human readable, 'json' for parsable, 'markdown' for Agent reading.
    #[arg(long, global = true, value_parser = ["text", "json", "markdown"])]
    pub format: Option<String>,

    /// Alias for --format=json. Forces JSON output.
    #[arg(long, global = true)]
    pub json: bool,

    /// Disable ANSI color codes in output.
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Suppress all non-error output.
    #[arg(long, global = true)]
    pub quiet: bool,

    /// Enable verbose logging for debugging purposes.
    #[arg(long, global = true, conflicts_with = "quiet")]
    pub verbose: bool,

    /// Automatically answer 'yes' to all interactive prompts.
    #[arg(long, global = true)]
    pub yes: bool,

    /// Simulate the command without making any state or database changes.
    #[arg(long, global = true)]
    pub dry_run: bool,

    /// Disable all interactive prompts and fail if input is required.
    #[arg(long, global = true)]
    pub non_interactive: bool,

    /// Configuration compatibility behavior: 'error' (fail on unknown fields) or 'warn'.
    #[arg(long, global = true, value_parser = ["error", "warn"])]
    pub config_compat: Option<String>,

    /// The subcommand to execute.
    #[command(subcommand)]
    pub command: Option<Commands>,
}

pub mod commands;
pub use commands::*;

// ── Top-level commands ───────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub enum Commands {
    /// Initialize a new CarryCtx project in the current directory or the specified path
    Init(InitArgs),
    /// Show the current status of the project, including active tasks, progress, and sessions
    Status(StatusArgs),
    /// Resume an existing session or start a new session, picking up context from where you left off
    Resume(ResumeArgs),
    /// Dump the full or compact context of the active task and session for LLM consumption
    Context(ContextArgs),
    /// Manage checkpoints (snapshots of state) for safe rollback and error recovery
    Checkpoint(CheckpointArgs),
    /// Diagnose and automatically fix potential issues with the project's SQLite state database
    Doctor(DoctorArgs),
    /// Manage, list, and switch between coding agents within the project
    Agent(AgentArgs),
    /// Create, assign, review, and complete tasks that drive the project lifecycle
    Task(TaskArgs),
    /// Manage agent sessions, transitions, pausing, and resuming
    Session(SessionArgs),
    /// Add, update, or resolve progress events (todos, blockers, notes) attached to tasks
    Progress(ProgressArgs),
    /// Manage Model Context Protocol (MCP) server
    Mcp(McpArgs),
    /// Manage ecosystem presets
    Preset(PresetArgs),
    /// Manage Git worktrees tied to specific tasks for isolated parallel development
    Worktree(WorktreeArgs),
    /// Query the immutable event log for auditing and tracking historical changes
    Event(EventArgs),
    /// View and modify global or project-local configuration properties
    Config(ConfigArgs),
    /// Manage the local CarryCtx project, backups, migrations, and registrations
    Project(ProjectArgs),
    /// Record and search architectural and design decisions (ADRs) attached to the project
    Decision(DecisionArgs),
    /// Create and manage handoffs between different agents to collaborate on tasks
    Handoff(HandoffArgs),
    /// Install, manage, and verify executable skills that agents can invoke
    Skill(SkillArgs),
    /// Generate shell completion scripts for bash, zsh, fish, or powershell
    Completions(CompletionsArgs),
    /// Install and manage Git hooks that integrate with CarryCtx
    Hooks(HooksArgs),
    /// Sync state with remote storage
    Sync(SyncArgs),
    /// Agent performance analytics and statistics
    Stats(StatsArgs),
    /// Manage Context Graph nodes and edges for semantic queries
    Graph(GraphArgs),
}

// ═══════════════════════════════════════════════════════════════════════════
//  main()
// ═══════════════════════════════════════════════════════════════════════════

fn main() {
    let cli = Cli::parse();
    let result = run(cli);
    match result {
        Ok(exit_code) => std::process::exit(exit_code as i32),
        Err(code) => std::process::exit(code as i32),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  run()
// ═══════════════════════════════════════════════════════════════════════════

fn run(cli: Cli) -> Result<ExitCode, ExitCode> {
    let mut ctx = build_invocation_context(&cli)?;
    let is_json = matches!(ctx.format, OutputFormat::Json);

    // Opportunistically resolve CARRYCTX_AGENT name to ULID before dispatching
    if let Ok(runtime) = try_open_runtime(&ctx) {
        if let Some(agent_ref) = &ctx.agent {
            if !agent_ref.trim().is_empty() {
                if let Ok(ulid) = resolve_agent_id(
                    &runtime.config.project.id,
                    agent_ref,
                    runtime.database.connection(),
                ) {
                    ctx.agent = Some(ulid);
                }
            }
        }
    }

    match &cli.command {
        Some(Commands::Init(args)) => handle_init(args, &ctx),
        Some(Commands::Status(args)) => handle_status(args, &ctx, is_json),
        Some(Commands::Resume(args)) => handle_resume(args, &ctx, is_json),
        Some(Commands::Context(args)) => handle_context(args, &ctx, is_json),
        Some(Commands::Checkpoint(args)) => handle_checkpoint(args, &ctx, is_json),
        Some(Commands::Doctor(args)) => handle_doctor(args, &ctx, is_json),
        Some(Commands::Agent(args)) => handle_agent(args, &ctx, is_json),
        Some(Commands::Task(args)) => handle_task(args, &ctx, is_json),
        Some(Commands::Session(args)) => handle_session(args, &ctx, is_json),
        Some(Commands::Progress(args)) => handle_progress(args, &ctx, is_json),
        Some(Commands::Mcp(args)) => handle_mcp(args, &ctx),
        Some(Commands::Preset(args)) => handle_preset(args, &ctx, is_json),
        Some(Commands::Worktree(args)) => handle_worktree(args, &ctx, is_json),
        Some(Commands::Event(args)) => handle_event(args, &ctx, is_json),
        Some(Commands::Config(args)) => handle_config(args, &ctx, is_json),
        Some(Commands::Project(args)) => handle_project(args, &ctx, is_json),
        Some(Commands::Decision(args)) => handle_decision(args, &ctx, is_json),
        Some(Commands::Handoff(args)) => handle_handoff(args, &ctx, is_json),
        Some(Commands::Skill(args)) => handle_skill(args, &ctx, is_json),
        Some(Commands::Completions(args)) => handle_completions(args),
        Some(Commands::Hooks(args)) => handle_hooks(args, &ctx, is_json),
        Some(Commands::Sync(args)) => handle_sync(args, &ctx, is_json),
        Some(Commands::Stats(args)) => handle_stats(args, &ctx, is_json),
        Some(Commands::Graph(args)) => handle_graph(args, &ctx, is_json),
        None => {
            if !ctx.quiet {
                println!(
                    "CarryCtx v{} - Persistent project context for coding agents",
                    env!("CARGO_PKG_VERSION")
                );
                println!("Use --help for usage information.");
            }
            Ok(ExitCode::Success)
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Helpers
// ═══════════════════════════════════════════════════════════════════════════

pub fn build_invocation_context(cli: &Cli) -> Result<InvocationContext, ExitCode> {
    let cwd = std::env::current_dir().map_err(|e| {
        eprintln!("Failed to get current directory: {e}");
        ExitCode::General
    })?;
    InvocationContext::new(
        cwd,
        cli.project.clone(),
        cli.config.clone(),
        cli.profile.clone(),
        cli.agent.clone(),
        cli.session.clone(),
        cli.task.clone(),
        cli.format.clone(),
        cli.json,
        cli.no_color,
        cli.quiet,
        cli.verbose,
        cli.dry_run,
        cli.yes,
        !cli.non_interactive,
    )
    .map_err(|e| e.exit_code)
}

pub fn resolve_work_dir(ctx: &InvocationContext) -> &Path {
    ctx.project.as_deref().map(Path::new).unwrap_or(&ctx.cwd)
}

pub fn try_open_runtime(ctx: &InvocationContext) -> Result<ProjectRuntime, ExitCode> {
    let xdg = XdgPaths::new();
    let cfg_loader = ConfigLoader::new(xdg.clone());
    let work_dir = resolve_work_dir(ctx);
    let mut config = cfg_loader.load(Some(work_dir)).map_err(|e| e.exit_code)?;
    let git = GitCli::new();
    let git_project = git.discover(work_dir).map_err(|e| e.exit_code)?;
    let db_path = xdg.project_db(&git_project.git_common_dir);
    let database = ProjectDatabase::open(&db_path).map_err(|e| e.exit_code)?;

    // Fetch primary project identity from DB if initialized
    if let Ok(mut stmt) = database
        .connection()
        .prepare("SELECT id, name, task_prefix FROM projects LIMIT 1")
    {
        if let Ok(row) = stmt.query_row([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        }) {
            config.project.id = row.0;
            if !row.1.is_empty() {
                config.project.name = row.1;
            }
            if !row.2.is_empty() {
                config.project.task_prefix = row.2;
            }
        }
    }

    Ok(ProjectRuntime {
        git_project,
        database,
        config,
        xdg,
        db_path,
    })
}
pub fn render_and_print<T: serde::Serialize>(
    command: &str,
    result: Result<T, CarryCtxError>,
    is_json: bool,
    quiet: bool,
) -> Result<ExitCode, ExitCode> {
    let (output, sink, exit_code) = match &result {
        Ok(data) => output::render_json(command, Ok(data), is_json),
        Err(err) => output::render_json::<serde_json::Value>(command, Err(err), is_json),
    };
    if !quiet || matches!(sink, output::OutputSink::Stderr) {
        match sink {
            output::OutputSink::Stdout => println!("{output}"),
            output::OutputSink::Stderr => eprintln!("{output}"),
        }
    }
    match exit_code {
        ExitCode::Success => Ok(ExitCode::Success),
        other => Err(other),
    }
}

pub fn not_implemented(command: &str) -> ExitCode {
    eprintln!("{command}: not yet implemented");
    ExitCode::Unsupported
}

pub fn check_dry_run(
    ctx: &InvocationContext,
    description: &str,
) -> Option<Result<ExitCode, ExitCode>> {
    if ctx.dry_run {
        eprintln!("[dry-run] Would {description}");
        Some(Ok(ExitCode::Success))
    } else {
        None
    }
}

pub fn resolve_agent_id(
    project_id: &str,
    agent_ref: &str,
    conn: &rusqlite::Connection,
) -> Result<String, CarryCtxError> {
    let repo = SqliteAgentRepository::new(conn);
    if let Some(agent) = repo.find_by_name(project_id, agent_ref)? {
        return Ok(agent.id);
    }
    if let Some(agent) = repo.find_by_id(project_id, agent_ref)? {
        return Ok(agent.id);
    }
    Err(CarryCtxError::resource_not_found(format!(
        "Agent '{agent_ref}' not found."
    )))
}

pub fn resolve_task_id(
    project_id: &str,
    task_ref: &str,
    conn: &rusqlite::Connection,
) -> Result<String, CarryCtxError> {
    let repo = SqliteTaskRepository::new(conn);
    if let Some(task) = repo.find_by_display_id(project_id, task_ref)? {
        return Ok(task.id);
    }
    if let Some(task) = repo.find_by_id(project_id, task_ref)? {
        return Ok(task.id);
    }
    Err(CarryCtxError::resource_not_found(format!(
        "Task '{task_ref}' not found."
    )))
}

pub fn parse_task_status(s: &str) -> Result<TaskStatus, CarryCtxError> {
    match s {
        "planned" => Ok(TaskStatus::Planned),
        "ready" => Ok(TaskStatus::Ready),
        "in_progress" => Ok(TaskStatus::InProgress),
        "blocked" => Ok(TaskStatus::Blocked),
        "review" => Ok(TaskStatus::Review),
        "completed" => Ok(TaskStatus::Completed),
        "cancelled" => Ok(TaskStatus::Cancelled),
        other => Err(CarryCtxError::invalid_arguments(format!(
            "Unknown status: {other}"
        ))),
    }
}

pub fn parse_task_priority(s: &str) -> Result<TaskPriority, CarryCtxError> {
    match s {
        "low" => Ok(TaskPriority::Low),
        "normal" => Ok(TaskPriority::Normal),
        "high" => Ok(TaskPriority::High),
        "urgent" => Ok(TaskPriority::Urgent),
        other => Err(CarryCtxError::invalid_arguments(format!(
            "Unknown priority: {other}"
        ))),
    }
}

pub fn parse_dependency_kind(s: &str) -> Result<DependencyKind, CarryCtxError> {
    match s {
        "strong" => Ok(DependencyKind::Strong),
        "informational" | "info" => Ok(DependencyKind::Informational),
        other => Err(CarryCtxError::invalid_arguments(format!(
            "Unknown dependency kind: {other}"
        ))),
    }
}
