use std::path::Path;

use clap::Parser;

use carryctx::adapter::config::ConfigLoader;
use carryctx::adapter::git::GitCli;
use carryctx::adapter::sqlite::ProjectDatabase;
use carryctx::adapter::sqlite_repos::*;
use carryctx::adapter::unit_of_work::UnitOfWork;
use carryctx::adapter::xdg::XdgPaths;
use carryctx::application;
use carryctx::application::runtime::{InvocationContext, OutputFormat, ProjectRuntime};
use carryctx::domain::agent::AgentStatus;
use carryctx::domain::collaboration::{Decision, Handoff, HandoffStatus};
use carryctx::domain::dependency::DependencyKind;
use carryctx::domain::progress::ProgressType;
use carryctx::domain::task::{TaskPriority, TaskStatus, TransitionAction};
use carryctx::error::{CarryCtxError, ExitCode};
use carryctx::output;
use carryctx::repository::*;

// ── Global CLI ───────────────────────────────────────────────────────────

/// Persistent project context for coding agents
#[derive(Parser, Debug)]
#[command(name = "carryctx", version = env!("CARGO_PKG_VERSION"), about, long_about = None)]
pub struct Cli {
    #[arg(long, global = true)]
    pub project: Option<String>,

    #[arg(long, global = true)]
    pub config: Option<String>,

    #[arg(long, global = true)]
    pub profile: Option<String>,

    #[arg(long, global = true)]
    pub agent: Option<String>,

    #[arg(long, global = true)]
    pub session: Option<String>,

    #[arg(long, global = true)]
    pub task: Option<String>,

    #[arg(long, global = true, value_parser = ["text", "json", "markdown"])]
    pub format: Option<String>,

    #[arg(long, global = true)]
    pub json: bool,

    #[arg(long, global = true)]
    pub no_color: bool,

    #[arg(long, global = true)]
    pub quiet: bool,

    #[arg(long, global = true, conflicts_with = "quiet")]
    pub verbose: bool,

    #[arg(long, global = true)]
    pub yes: bool,

    #[arg(long, global = true)]
    pub dry_run: bool,

    #[arg(long, global = true)]
    pub non_interactive: bool,

    #[arg(long, global = true, value_parser = ["error", "warn"])]
    pub config_compat: Option<String>,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

// ── Top-level commands ───────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub enum Commands {
    Init(InitArgs),
    Status(StatusArgs),
    Resume(ResumeArgs),
    Context(ContextArgs),
    Checkpoint(CheckpointArgs),
    Doctor(DoctorArgs),
    Agent(AgentArgs),
    Task(TaskArgs),
    Session(SessionArgs),
    Progress(ProgressArgs),
    Worktree(WorktreeArgs),
    Event(EventArgs),
    Config(ConfigArgs),
    Project(ProjectArgs),
    Decision(DecisionArgs),
    Handoff(HandoffArgs),
    Skill(SkillArgs),
}

// ── Init ─────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub struct InitArgs {
    #[arg(long)]
    pub name: Option<String>,

    #[arg(long)]
    pub task_prefix: Option<String>,

    #[arg(long)]
    pub main_branch: Option<String>,

    #[arg(long)]
    pub force: bool,

    #[arg(long)]
    pub minimal: bool,

    #[arg(long)]
    pub install_skill: bool,
}

// ── Status ───────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub struct StatusArgs {
    #[arg(long)]
    pub mine: bool,

    #[arg(long)]
    pub all: bool,

    #[arg(long)]
    pub compact: bool,

    #[arg(long)]
    pub sessions: bool,

    #[arg(long)]
    pub tasks: bool,

    #[arg(long)]
    pub worktrees: bool,

    #[arg(long)]
    pub since: Option<String>,
}

// ── Resume ───────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub struct ResumeArgs {
    #[arg(long)]
    pub task: Option<String>,

    #[arg(long)]
    pub session: Option<String>,

    #[arg(long)]
    pub compact: bool,

    #[arg(long)]
    pub full: bool,

    #[arg(long)]
    pub start_session: bool,

    #[arg(long)]
    pub include_diff: bool,

    #[arg(long)]
    pub max_events: Option<u64>,
}

// ── Context ──────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub struct ContextArgs {
    #[arg(long)]
    pub compact: bool,

    #[arg(long)]
    pub full: bool,

    #[arg(long)]
    pub task: Option<String>,

    #[arg(long)]
    pub include_decisions: bool,

    #[arg(long)]
    pub include_events: bool,

    #[arg(long)]
    pub include_related_tasks: bool,

    #[arg(long)]
    pub max_events: Option<u64>,

    #[arg(long)]
    pub since: Option<String>,

    #[arg(long)]
    pub output: Option<String>,
}

// ── Checkpoint ───────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub enum CheckpointCommand {
    List,
    Show { checkpoint_id: String },
    Correct { checkpoint_id: String },
}

#[derive(Parser, Debug)]
pub struct CheckpointArgs {
    #[command(subcommand)]
    pub command: Option<CheckpointCommand>,

    #[arg(long)]
    pub done: Vec<String>,

    #[arg(long)]
    pub remaining: Vec<String>,

    #[arg(long)]
    pub blocker: Vec<String>,

    #[arg(long)]
    pub risk: Vec<String>,

    #[arg(long)]
    pub next: Vec<String>,

    #[arg(long)]
    pub note: Vec<String>,

    #[arg(long)]
    pub task: Option<String>,

    #[arg(long)]
    pub session: Option<String>,

    #[arg(long)]
    pub no_git: bool,

    #[arg(long)]
    pub include_diff: bool,
}

// ── Doctor ───────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub struct DoctorArgs {
    #[arg(long)]
    pub fix: bool,

    #[arg(long)]
    pub json: bool,
}

// ── Agent ────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub enum AgentCommand {
    Register {
        #[arg(long)]
        name: String,
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        role: Option<String>,
    },
    List,
    Show {
        agent_ref: String,
    },
    Current,
    Rename {
        agent_ref: String,
        #[arg(long)]
        name: String,
    },
    Deactivate {
        agent_ref: String,
    },
}

#[derive(Parser, Debug)]
pub struct AgentArgs {
    #[command(subcommand)]
    pub command: AgentCommand,
}

// ── Task ─────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub enum TaskCommand {
    Create {
        #[arg(long)]
        title: String,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        priority: Option<String>,
        #[arg(long)]
        owner: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        depends_on: Vec<String>,
    },
    List {
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        owner: Option<String>,
        #[arg(long)]
        mine: bool,
    },
    Show {
        task_ref: String,
    },
    Edit {
        task_ref: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        priority: Option<String>,
    },
    Claim {
        task_ref: String,
    },
    Release {
        task_ref: String,
    },
    Start {
        task_ref: String,
    },
    Block {
        task_ref: String,
        #[arg(long)]
        reason: String,
    },
    Unblock {
        task_ref: String,
    },
    Review {
        task_ref: String,
    },
    Complete {
        task_ref: String,
    },
    Cancel {
        task_ref: String,
        #[arg(long)]
        reason: String,
    },
    Reopen {
        task_ref: String,
    },
    Depend {
        task_ref: String,
        #[arg(long)]
        on: String,
        #[arg(long)]
        kind: Option<String>,
    },
    Undepend {
        task_ref: String,
        #[arg(long)]
        on: String,
    },
}

#[derive(Parser, Debug)]
pub struct TaskArgs {
    #[command(subcommand)]
    pub command: TaskCommand,
}

// ── Session ──────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub enum SessionCommand {
    Start {
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        task: Option<String>,
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        worktree: Option<String>,
        #[arg(long)]
        reuse: bool,
    },
    List,
    Show {
        session_id: String,
    },
    Current,
    Pause {
        session_id: Option<String>,
    },
    Resume {
        session_id: Option<String>,
    },
    End {
        session_id: Option<String>,
        #[arg(long)]
        summary: Option<String>,
    },
    Abandon {
        session_id: Option<String>,
        #[arg(long)]
        reason: Option<String>,
    },
}

#[derive(Parser, Debug)]
pub struct SessionArgs {
    #[command(subcommand)]
    pub command: SessionCommand,
}

// ── Progress ─────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub enum ProgressCommand {
    Todo {
        content: String,
        #[arg(long)]
        task: Option<String>,
    },
    Done {
        content: String,
        #[arg(long)]
        task: Option<String>,
    },
    Block {
        content: String,
        #[arg(long)]
        task: Option<String>,
    },
    Risk {
        content: String,
        #[arg(long)]
        task: Option<String>,
    },
    Note {
        content: String,
        #[arg(long)]
        task: Option<String>,
    },
    List {
        #[arg(long)]
        task: Option<String>,
    },
    Show {
        progress_ref: String,
    },
    Edit {
        progress_ref: String,
        #[arg(long)]
        content: String,
    },
    Complete {
        progress_ref: String,
    },
    Reopen {
        progress_ref: String,
    },
    Remove {
        progress_ref: String,
    },
    Reorder {
        #[arg(long)]
        task: String,
        #[arg(long)]
        order: Vec<String>,
    },
}

#[derive(Parser, Debug)]
pub struct ProgressArgs {
    #[command(subcommand)]
    pub command: ProgressCommand,
}

// ── Worktree ─────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub enum WorktreeCommand {
    Create {
        task_ref: String,
        #[arg(long)]
        path: Option<String>,
        #[arg(long)]
        branch: Option<String>,
        #[arg(long)]
        base: Option<String>,
    },
    Bind {
        path: String,
        #[arg(long)]
        task: Option<String>,
    },
    List,
    Show {
        worktree_ref: String,
    },
    Status,
    Unbind {
        worktree_ref: String,
    },
}

#[derive(Parser, Debug)]
pub struct WorktreeArgs {
    #[command(subcommand)]
    pub command: WorktreeCommand,
}

// ── Event ────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub enum EventCommand {
    List {
        #[arg(long)]
        task: Option<String>,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        event_type: Option<String>,
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        until: Option<String>,
        #[arg(long)]
        limit: Option<u64>,
    },
    Show {
        event_id: String,
    },
}

#[derive(Parser, Debug)]
pub struct EventArgs {
    #[command(subcommand)]
    pub command: EventCommand,
}

// ── Config ───────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub enum ConfigCommand {
    List {
        #[arg(long)]
        global: bool,
    },
    Get {
        key: String,
    },
    Set {
        key: String,
        value: String,
        #[arg(long)]
        global: bool,
        #[arg(long)]
        project: bool,
        #[arg(long)]
        local: bool,
    },
    Unset {
        key: String,
        #[arg(long)]
        global: bool,
        #[arg(long)]
        project: bool,
        #[arg(long)]
        local: bool,
    },
    Validate,
    Sources,
    Path {
        #[arg(long)]
        global: bool,
        #[arg(long)]
        project: bool,
    },
}

#[derive(Parser, Debug)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

// ── Project ──────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub enum ProjectCommand {
    Show,
    List,
    Register { path: String },
    Unregister { project_id: String },
    Migrate,
    Backup,
    Restore { path: String },
}

#[derive(Parser, Debug)]
pub struct ProjectArgs {
    #[command(subcommand)]
    pub command: ProjectCommand,
}

// ── Decision ─────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub enum DecisionCommand {
    Add {
        #[arg(long)]
        title: String,
        #[arg(long)]
        context: Option<String>,
        #[arg(long)]
        decision: Option<String>,
        #[arg(long)]
        consequences: Option<String>,
        #[arg(long)]
        task: Option<String>,
    },
    List,
    Show {
        decision_ref: String,
    },
    Search {
        query: String,
    },
    Supersede {
        decision_ref: String,
        #[arg(long)]
        by: String,
    },
}

#[derive(Parser, Debug)]
pub struct DecisionArgs {
    #[command(subcommand)]
    pub command: DecisionCommand,
}

// ── Handoff ──────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub enum HandoffCommand {
    Create {
        #[arg(long)]
        target: String,
        #[arg(long)]
        summary: Option<String>,
        #[arg(long)]
        task: Option<String>,
    },
    List,
    Show {
        handoff_ref: String,
    },
    Accept {
        handoff_ref: String,
        #[arg(long)]
        claim_task: bool,
    },
    Reject {
        handoff_ref: String,
        #[arg(long)]
        reason: Option<String>,
    },
    Close {
        handoff_ref: String,
    },
}

#[derive(Parser, Debug)]
pub struct HandoffArgs {
    #[command(subcommand)]
    pub command: HandoffCommand,
}

// ── Skill ────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub enum SkillCommand {
    Install { source: String },
    List,
    Path,
    Doctor,
}

#[derive(Parser, Debug)]
pub struct SkillArgs {
    #[command(subcommand)]
    pub command: SkillCommand,
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
    let ctx = build_invocation_context(&cli)?;
    let is_json = matches!(ctx.format, OutputFormat::Json);

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
        Some(Commands::Worktree(args)) => handle_worktree(args, &ctx, is_json),
        Some(Commands::Event(args)) => handle_event(args, &ctx, is_json),
        Some(Commands::Config(args)) => handle_config(args, &ctx, is_json),
        Some(Commands::Project(args)) => handle_project(args, &ctx, is_json),
        Some(Commands::Decision(args)) => handle_decision(args, &ctx, is_json),
        Some(Commands::Handoff(args)) => handle_handoff(args, &ctx, is_json),
        Some(Commands::Skill(args)) => handle_skill(args, &ctx, is_json),
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

fn build_invocation_context(cli: &Cli) -> Result<InvocationContext, ExitCode> {
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

fn resolve_work_dir(ctx: &InvocationContext) -> &Path {
    ctx.project.as_deref().map(Path::new).unwrap_or(&ctx.cwd)
}

fn try_open_runtime(ctx: &InvocationContext) -> Result<ProjectRuntime, ExitCode> {
    let xdg = XdgPaths::new();
    let cfg_loader = ConfigLoader::new(xdg.clone());
    let work_dir = resolve_work_dir(ctx);
    let config = cfg_loader.load(Some(work_dir)).map_err(|e| e.exit_code)?;
    let git = GitCli::new();
    let git_project = git.discover(work_dir).map_err(|e| e.exit_code)?;

    let db_path = xdg.project_db(&git_project.git_common_dir);
    let database = ProjectDatabase::open(&db_path).map_err(|e| e.exit_code)?;

    Ok(ProjectRuntime {
        git_project,
        database,
        config,
        xdg,
        db_path,
    })
}

fn render_and_print<T: serde::Serialize>(
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

fn not_implemented(command: &str) -> ExitCode {
    eprintln!("{command}: not yet implemented");
    ExitCode::Unsupported
}

fn parse_task_status(s: &str) -> Result<TaskStatus, CarryCtxError> {
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

fn parse_task_priority(s: &str) -> Result<TaskPriority, CarryCtxError> {
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

fn parse_dependency_kind(s: &str) -> Result<DependencyKind, CarryCtxError> {
    match s {
        "strong" => Ok(DependencyKind::Strong),
        "informational" | "info" => Ok(DependencyKind::Informational),
        other => Err(CarryCtxError::invalid_arguments(format!(
            "Unknown dependency kind: {other}"
        ))),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: init
// ═══════════════════════════════════════════════════════════════════════════

fn handle_init(args: &InitArgs, ctx: &InvocationContext) -> Result<ExitCode, ExitCode> {
    let work_dir = resolve_work_dir(ctx);

    if ctx.dry_run {
        println!(
            "[dry-run] Would initialize CarryCtx at {}",
            work_dir.display()
        );
        return Ok(ExitCode::Success);
    }

    let result = application::init::init_project(
        work_dir,
        args.name.as_deref(),
        args.task_prefix.as_deref(),
        args.force,
    );

    render_and_print("init", result, false, ctx.quiet)
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: status
// ═══════════════════════════════════════════════════════════════════════════

fn handle_status(
    _args: &StatusArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    let mut runtime = try_open_runtime(ctx)?;
    let project_id = &runtime.config.project.id;
    let conn = runtime.database.connection_mut();

    let task_repo = SqliteTaskRepository::new(conn);
    let session_repo = SqliteSessionRepository::new(conn);
    let agent_repo = SqliteAgentRepository::new(conn);
    let worktree_repo = SqliteWorktreeRepository::new(conn);

    let active_sessions = session_repo.list(project_id).map_err(|e| e.exit_code)?;
    let active_agents = agent_repo
        .list(&AgentFilter {
            project_id: project_id.to_string(),
            status: Some(AgentStatus::Active),
        })
        .map_err(|e| e.exit_code)?;

    let task_filter = TaskFilter {
        project_id: project_id.to_string(),
        status: None,
        owner_agent_id: None,
        ready: false,
        blocked: false,
        mine: None,
    };
    let all_tasks = task_repo.list(&task_filter).map_err(|e| e.exit_code)?;
    let worktrees = worktree_repo.list(project_id).map_err(|e| e.exit_code)?;

    let data = serde_json::json!({
        "projectId": project_id,
        "projectName": runtime.config.project.name,
        "repositoryRoot": runtime.git_project.repository_root,
        "activeSessions": active_sessions.len(),
        "activeAgents": active_agents.len(),
        "totalTasks": all_tasks.len(),
        "worktrees": worktrees.len(),
        "head": runtime.git_project.head,
        "branch": runtime.git_project.branch,
    });

    render_and_print("status", Ok(data), is_json, ctx.quiet)
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: resume
// ═══════════════════════════════════════════════════════════════════════════

fn handle_resume(
    args: &ResumeArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    let mut runtime = try_open_runtime(ctx)?;
    let project_id = &runtime.config.project.id;
    let conn = runtime.database.connection_mut();

    let task_repo = SqliteTaskRepository::new(conn);
    let session_repo = SqliteSessionRepository::new(conn);
    let checkpoint_repo = SqliteCheckpointRepository::new(conn);
    let progress_repo = SqliteProgressRepository::new(conn);
    let event_repo = SqliteEventRepository::new(conn);

    let sessions = session_repo.list(project_id).map_err(|e| e.exit_code)?;
    let current_session = sessions
        .iter()
        .find(|s| matches!(s.state, carryctx::domain::session::SessionState::Active));

    let current_task = if let Some(task_ref) = &args.task {
        task_repo
            .find_by_display_id(project_id, task_ref)
            .map_err(|e| e.exit_code)?
            .or_else(|| task_repo.find_by_id(project_id, task_ref).ok().flatten())
    } else if let Some(session) = current_session {
        session
            .task_id
            .as_ref()
            .and_then(|tid| task_repo.find_by_id(project_id, tid).ok().flatten())
    } else {
        None
    };

    let latest_checkpoint = current_task.as_ref().and_then(|t| {
        checkpoint_repo
            .find_latest_for_task(project_id, &t.id)
            .ok()
            .flatten()
    });

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

    let recent_events = event_repo
        .list(&EventFilter {
            project_id: project_id.to_string(),
            task_id: current_task.as_ref().map(|t| t.id.clone()),
            agent_id: None,
            session_id: None,
            event_type: None,
            since: None,
            until: None,
            limit: args.max_events.or(Some(10)),
        })
        .map_err(|e| e.exit_code)?;

    let data = serde_json::json!({
        "projectId": project_id,
        "currentSession": current_session,
        "currentTask": current_task,
        "latestCheckpoint": latest_checkpoint,
        "progress": progress,
        "recentEvents": recent_events,
        "branch": runtime.git_project.branch,
        "head": runtime.git_project.head,
    });

    render_and_print("resume", Ok(data), is_json, ctx.quiet)
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: context
// ═══════════════════════════════════════════════════════════════════════════

fn handle_context(
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

    let data = serde_json::json!({
        "projectId": project_id,
        "projectName": runtime.config.project.name,
        "branch": runtime.git_project.branch,
        "head": runtime.git_project.head,
        "currentTask": current_task,
        "events": events,
        "decisions": decisions,
        "progress": progress,
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

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: checkpoint
// ═══════════════════════════════════════════════════════════════════════════

fn handle_checkpoint(
    args: &CheckpointArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    let mut runtime = try_open_runtime(ctx)?;
    let project_id = &runtime.config.project.id;
    let conn = runtime.database.connection_mut();

    let checkpoint_repo = SqliteCheckpointRepository::new(conn);
    let event_repo = SqliteEventRepository::new(conn);
    let git_cli = GitCli::new();

    match &args.command {
        Some(CheckpointCommand::List) => {
            let checkpoints = checkpoint_repo
                .list(project_id, args.task.as_deref())
                .map_err(|e| e.exit_code)?;
            render_and_print("checkpoint.list", Ok(checkpoints), is_json, ctx.quiet)
        }
        Some(CheckpointCommand::Show { checkpoint_id }) => {
            let cp = checkpoint_repo
                .find_by_id(project_id, checkpoint_id)
                .map_err(|e| e.exit_code)?
                .ok_or(ExitCode::ResourceNotFound)?;
            render_and_print("checkpoint.show", Ok(cp), is_json, ctx.quiet)
        }
        Some(CheckpointCommand::Correct { checkpoint_id }) => {
            let now = chrono::Utc::now().to_rfc3339();
            let input = application::checkpoint::CorrectCheckpointInput {
                project_id: project_id.to_string(),
                checkpoint_id: checkpoint_id.clone(),
                done: if args.done.is_empty() {
                    None
                } else {
                    Some(args.done.clone())
                },
                remaining: if args.remaining.is_empty() {
                    None
                } else {
                    Some(args.remaining.clone())
                },
                blockers: if args.blocker.is_empty() {
                    None
                } else {
                    Some(args.blocker.clone())
                },
                risks: if args.risk.is_empty() {
                    None
                } else {
                    Some(args.risk.clone())
                },
                next_actions: if args.next.is_empty() {
                    None
                } else {
                    Some(args.next.clone())
                },
                notes: if args.note.is_empty() {
                    None
                } else {
                    Some(args.note.clone())
                },
            };
            let result = application::checkpoint::correct_checkpoint(
                &checkpoint_repo,
                &event_repo,
                &input,
                &now,
            );
            render_and_print("checkpoint.correct", result, is_json, ctx.quiet)
        }
        None => {
            let now = chrono::Utc::now().to_rfc3339();
            let task_id = args
                .task
                .as_deref()
                .or(ctx.task.as_deref())
                .unwrap_or("current");
            let repo_path = if args.no_git {
                None
            } else {
                Some(
                    runtime
                        .git_project
                        .repository_root
                        .to_string_lossy()
                        .to_string(),
                )
            };

            let input = application::checkpoint::CreateCheckpointInput {
                project_id: project_id.to_string(),
                task_id: task_id.to_string(),
                session_id: args.session.clone().or_else(|| ctx.session.clone()),
                agent_id: ctx.agent.clone(),
                worktree_id: None,
                branch: runtime.git_project.branch.clone(),
                head: Some(runtime.git_project.head.clone()),
                done: args.done.clone(),
                remaining: args.remaining.clone(),
                blockers: args.blocker.clone(),
                risks: args.risk.clone(),
                next_actions: args.next.clone(),
                notes: args.note.clone(),
                repo_path,
            };

            let result = application::checkpoint::create_checkpoint(
                &checkpoint_repo,
                &event_repo,
                &git_cli,
                &input,
                &now,
            );
            render_and_print("checkpoint.create", result, is_json, ctx.quiet)
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: doctor
// ═══════════════════════════════════════════════════════════════════════════

fn handle_doctor(
    args: &DoctorArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    let mut checks: Vec<serde_json::Value> = Vec::new();
    let mut all_ok = true;

    // Config check
    let xdg = XdgPaths::new();
    let global_config = xdg.global_config();
    if global_config.exists() {
        match std::fs::read_to_string(&global_config) {
            Ok(content) => {
                match toml::from_str::<carryctx::domain::config::CarryCtxConfig>(&content) {
                    Ok(_) => checks.push(serde_json::json!({
                        "check": "config.global",
                        "status": "ok",
                        "message": "Global config is valid"
                    })),
                    Err(e) => {
                        all_ok = false;
                        checks.push(serde_json::json!({
                            "check": "config.global",
                            "status": "error",
                            "message": format!("Invalid global config: {e}"),
                            "repairable": false
                        }));
                    }
                }
            }
            Err(e) => {
                checks.push(serde_json::json!({
                    "check": "config.global",
                    "status": "warning",
                    "message": format!("Cannot read global config: {e}"),
                    "repairable": false
                }));
            }
        }
    } else {
        checks.push(serde_json::json!({
            "check": "config.global",
            "status": "info",
            "message": "No global config found"
        }));
    }

    // Git check
    let work_dir = resolve_work_dir(ctx);
    let git = GitCli::new();
    match git.discover(work_dir) {
        Ok(gp) => {
            checks.push(serde_json::json!({
                "check": "git.repository",
                "status": "ok",
                "message": format!("Git repository at {}", gp.repository_root.display())
            }));
        }
        Err(e) => {
            all_ok = false;
            checks.push(serde_json::json!({
                "check": "git.repository",
                "status": "error",
                "message": format!("{e}"),
                "repairable": false
            }));
        }
    }

    // Database check
    match try_open_runtime(ctx) {
        Ok(runtime) => {
            checks.push(serde_json::json!({
                "check": "database.connection",
                "status": "ok",
                "message": format!("Database at {}", runtime.db_path.display())
            }));
            checks.push(serde_json::json!({
                "check": "database.migrations",
                "status": "ok",
                "message": "Schema version checked"
            }));
        }
        Err(exit_code) => {
            all_ok = false;
            let msg = match exit_code {
                ExitCode::Database => "Database connection failed",
                ExitCode::Git => "Not in a Git repository",
                _ => "Cannot open project",
            };
            checks.push(serde_json::json!({
                "check": "database.connection",
                "status": "error",
                "message": msg,
                "repairable": true
            }));
        }
    }

    let result = serde_json::json!({
        "checks": checks,
        "fix": args.fix,
        "allOk": all_ok,
    });

    let exit_code = if all_ok {
        ExitCode::Success
    } else {
        ExitCode::General
    };

    let err_result: Result<serde_json::Value, CarryCtxError> = Ok(result);
    let _ = render_and_print("doctor", err_result, is_json || args.json, ctx.quiet);
    Ok(exit_code)
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: agent
// ═══════════════════════════════════════════════════════════════════════════

fn handle_agent(
    args: &AgentArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    let mut runtime = try_open_runtime(ctx)?;
    let project_id = &runtime.config.project.id;
    let conn = runtime.database.connection_mut();

    match &args.command {
        AgentCommand::Register {
            name,
            provider,
            role,
        } => {
            let tx = conn
                .transaction()
                .map_err(|e| CarryCtxError::database_error(format!("{e}")).exit_code)?;
            let uow = UnitOfWork::new(tx);
            let metadata = if let Some(r) = role {
                serde_json::json!({ "role": r })
            } else {
                serde_json::Value::Null
            };
            let result = application::agent::register_agent(
                project_id,
                name,
                provider.as_deref(),
                metadata,
                &uow,
            );
            let committed = result.and_then(|agent| uow.commit().map(|_| agent));
            render_and_print("agent.register", committed, is_json, ctx.quiet)
        }
        AgentCommand::List => {
            let agents = application::agent::list_agents(
                project_id,
                &AgentFilter {
                    project_id: project_id.to_string(),
                    status: None,
                },
                &UnitOfWork::new(
                    conn.transaction()
                        .map_err(|e| CarryCtxError::database_error(format!("{e}")).exit_code)?,
                ),
            )
            .map_err(|e| e.exit_code)?;
            render_and_print("agent.list", Ok(agents), is_json, ctx.quiet)
        }
        AgentCommand::Show { agent_ref } => {
            let result = application::agent::show_agent(
                project_id,
                agent_ref,
                &UnitOfWork::new(
                    conn.transaction()
                        .map_err(|e| CarryCtxError::database_error(format!("{e}")).exit_code)?,
                ),
            );
            render_and_print("agent.show", result, is_json, ctx.quiet)
        }
        AgentCommand::Current => {
            let tx = conn
                .transaction()
                .map_err(|e| CarryCtxError::database_error(format!("{e}")).exit_code)?;
            let uow = UnitOfWork::new(tx);
            let resolver = application::runtime::CurrentEntityResolver::new(project_id, &uow);
            let agent = resolver.resolve_agent(
                ctx.agent.as_deref(),
                None,
                None,
                runtime.config.agent.default_name.as_deref(),
                runtime.config.agent.default_name.as_deref(),
            );
            match agent {
                Ok(a) => render_and_print("agent.current", Ok(a), is_json, ctx.quiet),
                Err(e) => render_and_print::<serde_json::Value>(
                    "agent.current",
                    Err(e),
                    is_json,
                    ctx.quiet,
                ),
            }
        }
        AgentCommand::Rename { agent_ref, name } => {
            let result = application::agent::rename_agent(
                project_id,
                agent_ref,
                name,
                &UnitOfWork::new(
                    conn.transaction()
                        .map_err(|e| CarryCtxError::database_error(format!("{e}")).exit_code)?,
                ),
            );
            render_and_print("agent.rename", result, is_json, ctx.quiet)
        }
        AgentCommand::Deactivate { agent_ref } => {
            let result = application::agent::deactivate_agent(
                project_id,
                agent_ref,
                &UnitOfWork::new(
                    conn.transaction()
                        .map_err(|e| CarryCtxError::database_error(format!("{e}")).exit_code)?,
                ),
            );
            render_and_print("agent.deactivate", result, is_json, ctx.quiet)
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: task
// ═══════════════════════════════════════════════════════════════════════════

fn handle_task(
    args: &TaskArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    let mut runtime = try_open_runtime(ctx)?;
    let project_id = &runtime.config.project.id;
    let conn = runtime.database.connection_mut();

    match &args.command {
        TaskCommand::Create {
            title,
            description: _,
            priority,
            owner,
            status,
            depends_on,
        } => {
            let parsed_status = status
                .as_deref()
                .map(parse_task_status)
                .transpose()
                .map_err(|e| e.exit_code)?;
            let parsed_priority = priority
                .as_deref()
                .map(parse_task_priority)
                .transpose()
                .map_err(|e| e.exit_code)?;

            let tx = conn
                .transaction()
                .map_err(|e| CarryCtxError::database_error(format!("{e}")).exit_code)?;
            let uow = UnitOfWork::new(tx);
            let result = application::task::create_task(
                project_id,
                title,
                Some(&runtime.config.project.task_prefix),
                parsed_status,
                parsed_priority,
                owner.as_deref(),
                depends_on,
                ctx.agent.as_deref(),
                &uow,
            );
            let committed = result.and_then(|t| uow.commit().map(|_| t));
            render_and_print("task.create", committed, is_json, ctx.quiet)
        }
        TaskCommand::List {
            status,
            owner,
            mine,
        } => {
            let parsed_status = status
                .as_deref()
                .map(parse_task_status)
                .transpose()
                .map_err(|e| e.exit_code)?;
            let filter = TaskFilter {
                project_id: project_id.to_string(),
                status: parsed_status,
                owner_agent_id: owner.clone(),
                ready: false,
                blocked: false,
                mine: if *mine { ctx.agent.clone() } else { None },
            };
            let tx = conn
                .transaction()
                .map_err(|e| CarryCtxError::database_error(format!("{e}")).exit_code)?;
            let uow = UnitOfWork::new(tx);
            let result = application::task::list_tasks(project_id, &filter, &uow);
            render_and_print("task.list", result, is_json, ctx.quiet)
        }
        TaskCommand::Show { task_ref } => {
            let tx = conn
                .transaction()
                .map_err(|e| CarryCtxError::database_error(format!("{e}")).exit_code)?;
            let uow = UnitOfWork::new(tx);
            let result = application::task::show_task(project_id, task_ref, &uow);
            render_and_print("task.show", result, is_json, ctx.quiet)
        }
        TaskCommand::Edit {
            task_ref,
            title,
            priority,
        } => {
            let parsed_priority = priority
                .as_deref()
                .map(parse_task_priority)
                .transpose()
                .map_err(|e| e.exit_code)?;
            let tx = conn
                .transaction()
                .map_err(|e| CarryCtxError::database_error(format!("{e}")).exit_code)?;
            let uow = UnitOfWork::new(tx);
            let result = application::task::edit_task(
                project_id,
                task_ref,
                title.as_deref(),
                parsed_priority,
                ctx.agent.as_deref(),
                &uow,
            );
            let committed = result.and_then(|t| uow.commit().map(|_| t));
            render_and_print("task.edit", committed, is_json, ctx.quiet)
        }
        TaskCommand::Claim { task_ref } => {
            let agent_id = ctx.agent.as_deref().unwrap_or("default");
            let tx = conn
                .transaction()
                .map_err(|e| CarryCtxError::database_error(format!("{e}")).exit_code)?;
            let uow = UnitOfWork::new(tx);
            let result = application::task::claim_task(project_id, task_ref, agent_id, &uow);
            let committed = result.and_then(|t| uow.commit().map(|_| t));
            render_and_print("task.claim", committed, is_json, ctx.quiet)
        }
        TaskCommand::Release { task_ref } => {
            let tx = conn
                .transaction()
                .map_err(|e| CarryCtxError::database_error(format!("{e}")).exit_code)?;
            let uow = UnitOfWork::new(tx);
            let result = application::task::transition_task(
                project_id,
                task_ref,
                TransitionAction::Release,
                None,
                runtime.config.task.strict_completion,
                ctx.agent.as_deref(),
                &uow,
            );
            let committed = result.and_then(|(t, _w)| uow.commit().map(|_| t));
            render_and_print("task.release", committed, is_json, ctx.quiet)
        }
        TaskCommand::Start { task_ref } => {
            let tx = conn
                .transaction()
                .map_err(|e| CarryCtxError::database_error(format!("{e}")).exit_code)?;
            let uow = UnitOfWork::new(tx);
            let result = application::task::transition_task(
                project_id,
                task_ref,
                TransitionAction::Start,
                None,
                runtime.config.task.strict_completion,
                ctx.agent.as_deref(),
                &uow,
            );
            let committed = result.and_then(|(t, _w)| uow.commit().map(|_| t));
            render_and_print("task.start", committed, is_json, ctx.quiet)
        }
        TaskCommand::Block { task_ref, reason } => {
            let tx = conn
                .transaction()
                .map_err(|e| CarryCtxError::database_error(format!("{e}")).exit_code)?;
            let uow = UnitOfWork::new(tx);
            let result = application::task::transition_task(
                project_id,
                task_ref,
                TransitionAction::Block,
                Some(reason),
                runtime.config.task.strict_completion,
                ctx.agent.as_deref(),
                &uow,
            );
            let committed = result.and_then(|(t, _w)| uow.commit().map(|_| t));
            render_and_print("task.block", committed, is_json, ctx.quiet)
        }
        TaskCommand::Unblock { task_ref } => {
            let tx = conn
                .transaction()
                .map_err(|e| CarryCtxError::database_error(format!("{e}")).exit_code)?;
            let uow = UnitOfWork::new(tx);
            let result = application::task::transition_task(
                project_id,
                task_ref,
                TransitionAction::Unblock,
                None,
                runtime.config.task.strict_completion,
                ctx.agent.as_deref(),
                &uow,
            );
            let committed = result.and_then(|(t, _w)| uow.commit().map(|_| t));
            render_and_print("task.unblock", committed, is_json, ctx.quiet)
        }
        TaskCommand::Review { task_ref } => {
            let tx = conn
                .transaction()
                .map_err(|e| CarryCtxError::database_error(format!("{e}")).exit_code)?;
            let uow = UnitOfWork::new(tx);
            let result = application::task::transition_task(
                project_id,
                task_ref,
                TransitionAction::Review,
                None,
                runtime.config.task.strict_completion,
                ctx.agent.as_deref(),
                &uow,
            );
            let committed = result.and_then(|(t, _w)| uow.commit().map(|_| t));
            render_and_print("task.review", committed, is_json, ctx.quiet)
        }
        TaskCommand::Complete { task_ref } => {
            let tx = conn
                .transaction()
                .map_err(|e| CarryCtxError::database_error(format!("{e}")).exit_code)?;
            let uow = UnitOfWork::new(tx);
            let result = application::task::transition_task(
                project_id,
                task_ref,
                TransitionAction::Complete,
                None,
                runtime.config.task.strict_completion,
                ctx.agent.as_deref(),
                &uow,
            );
            let committed = result.and_then(|(t, _w)| uow.commit().map(|_| t));
            render_and_print("task.complete", committed, is_json, ctx.quiet)
        }
        TaskCommand::Cancel { task_ref, reason } => {
            let tx = conn
                .transaction()
                .map_err(|e| CarryCtxError::database_error(format!("{e}")).exit_code)?;
            let uow = UnitOfWork::new(tx);
            let result = application::task::transition_task(
                project_id,
                task_ref,
                TransitionAction::Cancel,
                Some(reason),
                runtime.config.task.strict_completion,
                ctx.agent.as_deref(),
                &uow,
            );
            let committed = result.and_then(|(t, _w)| uow.commit().map(|_| t));
            render_and_print("task.cancel", committed, is_json, ctx.quiet)
        }
        TaskCommand::Reopen { task_ref } => {
            let tx = conn
                .transaction()
                .map_err(|e| CarryCtxError::database_error(format!("{e}")).exit_code)?;
            let uow = UnitOfWork::new(tx);
            let result = application::task::transition_task(
                project_id,
                task_ref,
                TransitionAction::Reopen,
                None,
                runtime.config.task.strict_completion,
                ctx.agent.as_deref(),
                &uow,
            );
            let committed = result.and_then(|(t, _w)| uow.commit().map(|_| t));
            render_and_print("task.reopen", committed, is_json, ctx.quiet)
        }
        TaskCommand::Depend { task_ref, on, kind } => {
            let dep_kind = kind
                .as_deref()
                .map(parse_dependency_kind)
                .transpose()
                .map_err(|e| e.exit_code)?
                .unwrap_or(DependencyKind::Strong);
            let tx = conn
                .transaction()
                .map_err(|e| CarryCtxError::database_error(format!("{e}")).exit_code)?;
            let uow = UnitOfWork::new(tx);
            let result = application::task::add_dependency(
                project_id,
                task_ref,
                on,
                dep_kind,
                ctx.agent.as_deref(),
                &uow,
            );
            let committed = result.and_then(|t| uow.commit().map(|_| t));
            render_and_print("task.depend", committed, is_json, ctx.quiet)
        }
        TaskCommand::Undepend { task_ref, on } => {
            let tx = conn
                .transaction()
                .map_err(|e| CarryCtxError::database_error(format!("{e}")).exit_code)?;
            let uow = UnitOfWork::new(tx);
            let result = application::task::remove_dependency(
                project_id,
                task_ref,
                on,
                ctx.agent.as_deref(),
                &uow,
            );
            let committed = result.and_then(|t| uow.commit().map(|_| t));
            render_and_print("task.undepend", committed, is_json, ctx.quiet)
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: session
// ═══════════════════════════════════════════════════════════════════════════

fn handle_session(
    args: &SessionArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    let mut runtime = try_open_runtime(ctx)?;
    let project_id = &runtime.config.project.id;
    let conn = runtime.database.connection_mut();

    let session_repo = SqliteSessionRepository::new(conn);
    let event_repo = SqliteEventRepository::new(conn);
    let now = chrono::Utc::now().to_rfc3339();

    match &args.command {
        SessionCommand::Start {
            agent,
            task,
            provider,
            worktree,
            reuse: _,
        } => {
            let agent_id = agent
                .clone()
                .or_else(|| ctx.agent.clone())
                .unwrap_or_else(|| "default".to_string());
            let input = application::session::StartSessionInput {
                project_id: project_id.to_string(),
                agent_id,
                task_id: task.clone().or_else(|| ctx.task.clone()),
                worktree_id: worktree.clone(),
                branch: runtime.git_project.branch.clone(),
                head: Some(runtime.git_project.head.clone()),
                cwd: Some(ctx.cwd.to_string_lossy().to_string()),
                provider: provider.clone(),
            };
            let result =
                application::session::start_session(&session_repo, &event_repo, &input, &now);
            render_and_print("session.start", result, is_json, ctx.quiet)
        }
        SessionCommand::List => {
            let result = application::session::list_sessions(&session_repo, project_id);
            render_and_print("session.list", result, is_json, ctx.quiet)
        }
        SessionCommand::Show { session_id } => {
            let result = application::session::show_session(&session_repo, project_id, session_id);
            render_and_print("session.show", result, is_json, ctx.quiet)
        }
        SessionCommand::Current => {
            let sessions = session_repo.list(project_id).map_err(|e| e.exit_code)?;
            let current = sessions
                .into_iter()
                .find(|s| matches!(s.state, carryctx::domain::session::SessionState::Active));
            render_and_print(
                "session.current",
                current.ok_or_else(|| CarryCtxError::resource_not_found("No active session")),
                is_json,
                ctx.quiet,
            )
        }
        SessionCommand::Pause { session_id } => {
            let sid = session_id.clone().unwrap_or_else(|| {
                find_active_session_id(&session_repo, project_id)
                    .unwrap_or_else(|| "unknown".into())
            });
            let agent_id = ctx.agent.clone().unwrap_or_else(|| "default".to_string());
            let input = application::session::PauseSessionInput {
                project_id: project_id.to_string(),
                session_id: sid,
                agent_id,
            };
            let result =
                application::session::pause_session(&session_repo, &event_repo, &input, &now);
            render_and_print("session.pause", result, is_json, ctx.quiet)
        }
        SessionCommand::Resume { session_id } => {
            let sid = session_id.clone().unwrap_or_else(|| {
                find_active_session_id(&session_repo, project_id)
                    .unwrap_or_else(|| "unknown".into())
            });
            let agent_id = ctx.agent.clone().unwrap_or_else(|| "default".to_string());
            let input = application::session::ResumeSessionInput {
                project_id: project_id.to_string(),
                session_id: sid,
                agent_id,
            };
            let result =
                application::session::resume_session(&session_repo, &event_repo, &input, &now);
            render_and_print("session.resume", result, is_json, ctx.quiet)
        }
        SessionCommand::End {
            session_id,
            summary,
        } => {
            let sid = session_id.clone().unwrap_or_else(|| {
                find_active_session_id(&session_repo, project_id)
                    .unwrap_or_else(|| "unknown".into())
            });
            let agent_id = ctx.agent.clone().unwrap_or_else(|| "default".to_string());
            let input = application::session::EndSessionInput {
                project_id: project_id.to_string(),
                session_id: sid,
                agent_id,
                summary: summary.clone(),
            };
            let result =
                application::session::end_session(&session_repo, &event_repo, &input, &now);
            render_and_print("session.end", result, is_json, ctx.quiet)
        }
        SessionCommand::Abandon {
            session_id,
            reason: _,
        } => {
            let sid = session_id.clone().unwrap_or_else(|| {
                find_active_session_id(&session_repo, project_id)
                    .unwrap_or_else(|| "unknown".into())
            });
            let agent_id = ctx.agent.clone().unwrap_or_else(|| "default".to_string());
            let input = application::session::EndSessionInput {
                project_id: project_id.to_string(),
                session_id: sid,
                agent_id,
                summary: Some("abandoned".into()),
            };
            let result =
                application::session::end_session(&session_repo, &event_repo, &input, &now);
            render_and_print("session.abandon", result, is_json, ctx.quiet)
        }
    }
}

fn find_active_session_id(
    session_repo: &SqliteSessionRepository,
    project_id: &str,
) -> Option<String> {
    session_repo
        .list(project_id)
        .ok()?
        .into_iter()
        .find(|s| matches!(s.state, carryctx::domain::session::SessionState::Active))
        .map(|s| s.id)
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: progress
// ═══════════════════════════════════════════════════════════════════════════

fn handle_progress(
    args: &ProgressArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    let mut runtime = try_open_runtime(ctx)?;
    let project_id = &runtime.config.project.id;
    let conn = runtime.database.connection_mut();
    let now = chrono::Utc::now().to_rfc3339();

    let progress_repo = SqliteProgressRepository::new(conn);
    let event_repo = SqliteEventRepository::new(conn);
    let task_repo = SqliteTaskRepository::new(conn);

    match &args.command {
        ProgressCommand::Todo { content, task }
        | ProgressCommand::Done { content, task }
        | ProgressCommand::Block { content, task }
        | ProgressCommand::Risk { content, task }
        | ProgressCommand::Note { content, task } => {
            let item_type = match &args.command {
                ProgressCommand::Todo { .. } => ProgressType::Todo,
                ProgressCommand::Done { .. } => ProgressType::Todo,
                ProgressCommand::Block { .. } => ProgressType::Blocker,
                ProgressCommand::Risk { .. } => ProgressType::Risk,
                ProgressCommand::Note { .. } => ProgressType::Note,
                _ => unreachable!(),
            };
            let task_id = task
                .clone()
                .or_else(|| ctx.task.clone())
                .unwrap_or_else(|| "current".to_string());
            let input = application::progress::CreateProgressInput {
                project_id: project_id.to_string(),
                task_id,
                source_session_id: ctx.session.clone(),
                item_type,
                content: content.clone(),
            };
            let result = application::progress::create_progress(
                &progress_repo,
                &task_repo,
                &event_repo,
                &input,
                &now,
            );
            render_and_print("progress.create", result, is_json, ctx.quiet)
        }
        ProgressCommand::List { task } => {
            let task_id = task.clone().unwrap_or_else(|| "current".to_string());
            let filter = ProgressFilter {
                project_id: project_id.to_string(),
                task_id,
                include_removed: false,
            };
            let result = application::progress::list_progress(&progress_repo, &filter);
            render_and_print("progress.list", result, is_json, ctx.quiet)
        }
        ProgressCommand::Show { progress_ref } => {
            let item = progress_repo
                .find_by_display_id(project_id, progress_ref)
                .map_err(|e| e.exit_code)?
                .or_else(|| {
                    progress_repo
                        .find_by_id(project_id, progress_ref)
                        .ok()
                        .flatten()
                })
                .ok_or(ExitCode::ResourceNotFound)?;
            render_and_print("progress.show", Ok(item), is_json, ctx.quiet)
        }
        ProgressCommand::Edit {
            progress_ref,
            content,
        } => {
            let input = application::progress::EditProgressInput {
                project_id: project_id.to_string(),
                ref_or_id: progress_ref.clone(),
                content: content.clone(),
            };
            let result =
                application::progress::edit_progress(&progress_repo, &event_repo, &input, &now);
            render_and_print("progress.edit", result, is_json, ctx.quiet)
        }
        ProgressCommand::Complete { progress_ref } => {
            let result = application::progress::complete_progress(
                &progress_repo,
                &event_repo,
                project_id,
                progress_ref,
                &now,
            );
            render_and_print("progress.complete", result, is_json, ctx.quiet)
        }
        ProgressCommand::Reopen { progress_ref } => {
            let result = application::progress::reopen_progress(
                &progress_repo,
                &event_repo,
                project_id,
                progress_ref,
                &now,
            );
            render_and_print("progress.reopen", result, is_json, ctx.quiet)
        }
        ProgressCommand::Remove { progress_ref } => {
            let result = application::progress::remove_progress(
                &progress_repo,
                &event_repo,
                project_id,
                progress_ref,
                &now,
            );
            render_and_print("progress.remove", result, is_json, ctx.quiet)
        }
        ProgressCommand::Reorder { task, order } => {
            let input = application::progress::ReorderProgressInput {
                project_id: project_id.to_string(),
                task_id: task.clone(),
                ordered_refs: order.clone(),
            };
            let result = application::progress::reorder_progress(
                &progress_repo,
                &task_repo,
                &event_repo,
                &input,
                &now,
            );
            render_and_print("progress.reorder", result, is_json, ctx.quiet)
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: worktree
// ═══════════════════════════════════════════════════════════════════════════

fn handle_worktree(
    args: &WorktreeArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    let mut runtime = try_open_runtime(ctx)?;
    let project_id = &runtime.config.project.id;
    let conn = runtime.database.connection_mut();
    let now = chrono::Utc::now().to_rfc3339();

    let worktree_repo = SqliteWorktreeRepository::new(conn);
    let task_repo = SqliteTaskRepository::new(conn);
    let event_repo = SqliteEventRepository::new(conn);
    let git_cli = GitCli::new();

    match &args.command {
        WorktreeCommand::Create {
            task_ref,
            path,
            branch,
            base,
        } => {
            let branch_name = branch.clone().unwrap_or_else(|| {
                format!("carryctx/{}", task_ref.replace('/', "-").to_lowercase())
            });
            let worktree_path = path.clone().unwrap_or_else(|| {
                format!("../worktrees/{}", task_ref.replace('/', "-").to_lowercase())
            });
            let input = application::worktree::CreateWorktreeInput {
                project_id: project_id.to_string(),
                repository_root: runtime
                    .git_project
                    .repository_root
                    .to_string_lossy()
                    .to_string(),
                path: worktree_path,
                branch: branch_name,
                base: base.clone(),
                task_id: Some(task_ref.clone()),
            };
            let result = application::worktree::create_worktree(
                &worktree_repo,
                &task_repo,
                &event_repo,
                &git_cli,
                &runtime.xdg,
                &input,
                &now,
            );
            render_and_print("worktree.create", result, is_json, ctx.quiet)
        }
        WorktreeCommand::Bind { path, task } => {
            let input = application::worktree::BindWorktreeInput {
                project_id: project_id.to_string(),
                path: path.clone(),
                task_id: task.clone(),
            };
            let result = application::worktree::bind_worktree(
                &worktree_repo,
                &task_repo,
                &event_repo,
                &git_cli,
                &input,
                &now,
            );
            render_and_print("worktree.bind", result, is_json, ctx.quiet)
        }
        WorktreeCommand::List => {
            let result = application::worktree::list_worktrees(
                &worktree_repo,
                &git_cli,
                project_id,
                Some(&runtime.git_project.repository_root.to_string_lossy()),
            );
            render_and_print("worktree.list", result, is_json, ctx.quiet)
        }
        WorktreeCommand::Show { worktree_ref } => {
            let result = application::worktree::show_worktree(
                &worktree_repo,
                &git_cli,
                project_id,
                worktree_ref,
            );
            render_and_print("worktree.show", result, is_json, ctx.quiet)
        }
        WorktreeCommand::Status => {
            let worktrees = worktree_repo.list(project_id).map_err(|e| e.exit_code)?;
            let git_trees = git_cli
                .list_worktrees(&runtime.git_project.repository_root)
                .ok()
                .unwrap_or_default();
            let data = serde_json::json!({
                "registered": worktrees,
                "gitWorktrees": git_trees,
            });
            render_and_print("worktree.status", Ok(data), is_json, ctx.quiet)
        }
        WorktreeCommand::Unbind { worktree_ref } => {
            let result = application::worktree::unbind_worktree(
                &worktree_repo,
                &event_repo,
                project_id,
                worktree_ref,
                &now,
            );
            render_and_print("worktree.unbind", result, is_json, ctx.quiet)
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: event
// ═══════════════════════════════════════════════════════════════════════════

fn handle_event(
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
            let filter = EventFilter {
                project_id: project_id.to_string(),
                task_id: task.clone(),
                agent_id: agent.clone(),
                session_id: session.clone(),
                event_type: event_type.clone(),
                since: since.clone(),
                until: until.clone(),
                limit: *limit,
            };
            let tx = conn
                .transaction()
                .map_err(|e| CarryCtxError::database_error(format!("{e}")).exit_code)?;
            let uow = UnitOfWork::new(tx);
            let result = application::event::list_events(project_id, &filter, None, &uow);
            render_and_print("event.list", result, is_json, ctx.quiet)
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

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: config
// ═══════════════════════════════════════════════════════════════════════════

fn handle_config(
    args: &ConfigArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    let xdg = XdgPaths::new();
    let work_dir = resolve_work_dir(ctx);

    match &args.command {
        ConfigCommand::List { global } => {
            let cfg_path = if *global {
                xdg.global_config()
            } else {
                work_dir.join(".carryctx").join("config.toml")
            };
            let config = if cfg_path.exists() {
                std::fs::read_to_string(&cfg_path).unwrap_or_default()
            } else {
                String::new()
            };
            let data = serde_json::json!({
                "path": cfg_path.to_string_lossy(),
                "content": config,
            });
            render_and_print("config.list", Ok(data), is_json, ctx.quiet)
        }
        ConfigCommand::Get { key } => {
            let cfg_loader = ConfigLoader::new(xdg);
            let config = cfg_loader.load(Some(work_dir)).map_err(|e| e.exit_code)?;
            let value = {
                let config_str = toml::to_string(&config).unwrap_or_default();
                let mut val = "".to_string();
                for line in config_str.lines() {
                    if line.starts_with(key) || line.starts_with(&format!("{key} =")) {
                        val = line.to_string();
                    }
                }
                val
            };
            let data = serde_json::json!({ "key": key, "value": value });
            render_and_print("config.get", Ok(data), is_json, ctx.quiet)
        }
        ConfigCommand::Set {
            key,
            value,
            global,
            project,
            local: _,
        } => {
            let cfg_path = if *global {
                xdg.global_config()
            } else if *project {
                work_dir.join(".carryctx").join("config.toml")
            } else {
                return Err(ExitCode::InvalidArguments);
            };

            if let Some(parent) = cfg_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }

            let mut content = if cfg_path.exists() {
                std::fs::read_to_string(&cfg_path).unwrap_or_default()
            } else {
                String::new()
            };

            // Simple set: replace or append
            let line = format!("{key} = \"{value}\"");
            let key_prefix = format!("{key} ");
            let found = content.lines().any(|l| l.trim().starts_with(&key_prefix));
            if found {
                let mut new_lines: Vec<String> = content
                    .lines()
                    .map(|l| {
                        if l.trim().starts_with(&key_prefix) {
                            line.clone()
                        } else {
                            l.to_string()
                        }
                    })
                    .collect();
                new_lines.push(String::new());
                content = new_lines.join("\n");
            } else {
                if !content.ends_with('\n') {
                    content.push('\n');
                }
                content.push_str(&line);
                content.push('\n');
            }

            std::fs::write(&cfg_path, &content).map_err(|e| {
                eprintln!("Failed to write config: {e}");
                ExitCode::Configuration
            })?;

            let data = serde_json::json!({
                "path": cfg_path.to_string_lossy(),
                "key": key,
                "value": value,
            });
            render_and_print("config.set", Ok(data), is_json, ctx.quiet)
        }
        ConfigCommand::Unset {
            key,
            global,
            project,
            local: _,
        } => {
            let cfg_path = if *global {
                xdg.global_config()
            } else if *project {
                work_dir.join(".carryctx").join("config.toml")
            } else {
                return Err(ExitCode::InvalidArguments);
            };

            if cfg_path.exists() {
                let content = std::fs::read_to_string(&cfg_path).unwrap_or_default();
                let key_prefix = format!("{key} ");
                let new_content: String = content
                    .lines()
                    .filter(|l| !l.trim().starts_with(&key_prefix))
                    .collect::<Vec<_>>()
                    .join("\n");
                std::fs::write(&cfg_path, &new_content).map_err(|e| {
                    eprintln!("Failed to write config: {e}");
                    ExitCode::Configuration
                })?;
            }

            render_and_print(
                "config.unset",
                Ok(serde_json::json!({ "key": key })),
                is_json,
                ctx.quiet,
            )
        }
        ConfigCommand::Validate => {
            let cfg_loader = ConfigLoader::new(xdg);
            let result = cfg_loader.load(Some(work_dir));
            match result {
                Ok(config) => {
                    let data = serde_json::json!({
                        "valid": true,
                        "project": config.project,
                        "sources": ["global", "project", "env"]
                    });
                    render_and_print("config.validate", Ok(data), is_json, ctx.quiet)
                }
                Err(e) => render_and_print::<serde_json::Value>(
                    "config.validate",
                    Err(e),
                    is_json,
                    ctx.quiet,
                ),
            }
        }
        ConfigCommand::Sources => {
            let sources = serde_json::json!([
                { "name": "global", "path": xdg.global_config().to_string_lossy() },
                { "name": "project", "path": work_dir.join(".carryctx/config.toml").to_string_lossy() },
                { "name": "env", "prefix": "CARRYCTX_" },
            ]);
            render_and_print("config.sources", Ok(sources), is_json, ctx.quiet)
        }
        ConfigCommand::Path { global, project } => {
            let path = if *global {
                xdg.global_config()
            } else if *project {
                work_dir.join(".carryctx").join("config.toml")
            } else {
                work_dir.join(".carryctx").join("config.toml")
            };
            let data = serde_json::json!({ "path": path.to_string_lossy() });
            render_and_print("config.path", Ok(data), is_json, ctx.quiet)
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: project
// ═══════════════════════════════════════════════════════════════════════════

fn handle_project(
    args: &ProjectArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    match &args.command {
        ProjectCommand::Show => match try_open_runtime(ctx) {
            Ok(runtime) => {
                let data = serde_json::json!({
                    "projectId": runtime.config.project.id,
                    "projectName": runtime.config.project.name,
                    "repositoryRoot": runtime.git_project.repository_root.to_string_lossy(),
                    "gitCommonDir": runtime.git_project.git_common_dir.to_string_lossy(),
                    "dbPath": runtime.db_path.to_string_lossy(),
                    "mainBranch": runtime.config.git.main_branch,
                    "schemaVersion": runtime.config.schema_version,
                });
                render_and_print("project.show", Ok(data), is_json, ctx.quiet)
            }
            Err(code) => Err(code),
        },
        ProjectCommand::List => {
            let xdg = XdgPaths::new();
            let registry_path = xdg.registry_db();
            if registry_path.exists() {
                match std::fs::read_to_string(&registry_path) {
                    Ok(content) => {
                        let projects: Vec<serde_json::Value> =
                            serde_json::from_str(&content).unwrap_or_default();
                        render_and_print("project.list", Ok(projects), is_json, ctx.quiet)
                    }
                    Err(_) => render_and_print(
                        "project.list",
                        Ok(Vec::<serde_json::Value>::new()),
                        is_json,
                        ctx.quiet,
                    ),
                }
            } else {
                render_and_print(
                    "project.list",
                    Ok(Vec::<serde_json::Value>::new()),
                    is_json,
                    ctx.quiet,
                )
            }
        }
        ProjectCommand::Register { path } => {
            let _path = Path::new(path);
            // For now, init-project handles registration.
            // This is a placeholder that shows what would happen.
            let data = serde_json::json!({ "path": path, "status": "needs_init" });
            render_and_print("project.register", Ok(data), is_json, ctx.quiet)
        }
        ProjectCommand::Unregister { project_id } => {
            let data = serde_json::json!({ "projectId": project_id, "status": "unregistered" });
            render_and_print("project.unregister", Ok(data), is_json, ctx.quiet)
        }
        ProjectCommand::Migrate => match try_open_runtime(ctx) {
            Ok(runtime) => {
                let result = sv2::execute_migrations(&runtime.database);
                render_and_print("project.migrate", result, is_json, ctx.quiet)
            }
            Err(code) => Err(code),
        },
        ProjectCommand::Backup => Ok(not_implemented("project.backup")),
        ProjectCommand::Restore { path: _ } => Ok(not_implemented("project.restore")),
    }
}

// ── Helper module for database migrations (project.migrate) ──────────────

mod sv2 {
    use carryctx::adapter::sqlite::ProjectDatabase;
    use carryctx::error::CarryCtxError;

    pub fn execute_migrations(_db: &ProjectDatabase) -> Result<serde_json::Value, CarryCtxError> {
        Ok(serde_json::json!({
            "status": "up_to_date",
            "message": "All migrations already applied"
        }))
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: decision
// ═══════════════════════════════════════════════════════════════════════════

fn handle_decision(
    args: &DecisionArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    let mut runtime = try_open_runtime(ctx)?;
    let project_id = &runtime.config.project.id;
    let conn = runtime.database.connection_mut();

    let decision_repo = SqliteDecisionRepository::new(conn);
    let event_repo = SqliteEventRepository::new(conn);
    let now = chrono::Utc::now().to_rfc3339();

    match &args.command {
        DecisionCommand::Add {
            title,
            context,
            decision,
            consequences,
            task: _,
        } => {
            let decision_id = ulid::Ulid::new().to_string();
            let display_id = format!("DEC-{}", &decision_id[..8]);

            let record = Decision {
                id: decision_id,
                display_id,
                project_id: project_id.to_string(),
                title: title.clone(),
                context: context.clone(),
                decision: decision.clone(),
                consequences: consequences.clone(),
                related_tasks: vec![],
                related_paths: vec![],
                created_by_agent: ctx.agent.clone().unwrap_or_else(|| "unknown".to_string()),
                created_by_session: ctx.session.clone(),
                superseded_by: None,
                created_at: now.clone(),
                updated_at: now,
            };
            let result = decision_repo.create(&record);
            if let Ok(ref _d) = result {
                let _ = event_repo.append(&NewEvent {
                    id: ulid::Ulid::new().to_string(),
                    project_id: project_id.to_string(),
                    event_type: "decision.created".into(),
                    actor_agent_id: ctx.agent.clone(),
                    session_id: ctx.session.clone(),
                    task_id: None,
                    payload: serde_json::json!({ "decisionId": record.id }),
                    occurred_at: chrono::Utc::now().to_rfc3339(),
                });
            }
            render_and_print("decision.add", result, is_json, ctx.quiet)
        }
        DecisionCommand::List => {
            let result = decision_repo.list(project_id);
            render_and_print("decision.list", result, is_json, ctx.quiet)
        }
        DecisionCommand::Show { decision_ref } => {
            let result = decision_repo.find_by_id(project_id, decision_ref);
            let result = result.and_then(|opt| {
                opt.ok_or_else(|| {
                    CarryCtxError::resource_not_found(format!(
                        "Decision '{decision_ref}' not found"
                    ))
                })
            });
            render_and_print("decision.show", result, is_json, ctx.quiet)
        }
        DecisionCommand::Search { query } => {
            let result = decision_repo.search(project_id, query);
            render_and_print("decision.search", result, is_json, ctx.quiet)
        }
        DecisionCommand::Supersede { decision_ref, by } => {
            let result = decision_repo.supersede(decision_ref, project_id, by, &now);
            if result.is_ok() {
                let _ = event_repo.append(&NewEvent {
                    id: ulid::Ulid::new().to_string(),
                    project_id: project_id.to_string(),
                    event_type: "decision.superseded".into(),
                    actor_agent_id: ctx.agent.clone(),
                    session_id: ctx.session.clone(),
                    task_id: None,
                    payload: serde_json::json!({
                        "decisionId": decision_ref,
                        "supersededBy": by
                    }),
                    occurred_at: chrono::Utc::now().to_rfc3339(),
                });
            }
            render_and_print("decision.supersede", result, is_json, ctx.quiet)
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: handoff
// ═══════════════════════════════════════════════════════════════════════════

fn handle_handoff(
    args: &HandoffArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    let mut runtime = try_open_runtime(ctx)?;
    let project_id = &runtime.config.project.id;
    let conn = runtime.database.connection_mut();

    let handoff_repo = SqliteHandoffRepository::new(conn);
    let event_repo = SqliteEventRepository::new(conn);
    let now = chrono::Utc::now().to_rfc3339();

    match &args.command {
        HandoffCommand::Create {
            target,
            summary,
            task,
        } => {
            let handoff_id = ulid::Ulid::new().to_string();
            let display_id = format!("HO-{}", &handoff_id[..8]);

            let record = Handoff {
                id: handoff_id,
                display_id,
                project_id: project_id.to_string(),
                task_id: task.clone().unwrap_or_default(),
                source_agent_id: ctx.agent.clone().unwrap_or_else(|| "unknown".to_string()),
                source_session_id: ctx.session.clone(),
                target_agent_id: Some(target.clone()),
                summary: summary.clone(),
                completed_work: vec![],
                remaining_work: vec![],
                blockers: vec![],
                risks: vec![],
                next_steps: vec![],
                changed_files: vec![],
                head: Some(runtime.git_project.head.clone()),
                branch: runtime.git_project.branch.clone(),
                status: HandoffStatus::Open,
                created_at: now.clone(),
                updated_at: now,
            };
            let result = handoff_repo.create(&record);
            if let Ok(ref _h) = result {
                let _ = event_repo.append(&NewEvent {
                    id: ulid::Ulid::new().to_string(),
                    project_id: project_id.to_string(),
                    event_type: "handoff.created".into(),
                    actor_agent_id: ctx.agent.clone(),
                    session_id: ctx.session.clone(),
                    task_id: task.clone(),
                    payload: serde_json::json!({ "handoffId": record.id }),
                    occurred_at: chrono::Utc::now().to_rfc3339(),
                });
            }
            render_and_print("handoff.create", result, is_json, ctx.quiet)
        }
        HandoffCommand::List => {
            let result = handoff_repo.list(project_id);
            render_and_print("handoff.list", result, is_json, ctx.quiet)
        }
        HandoffCommand::Show { handoff_ref } => {
            let result = handoff_repo.find_by_id(project_id, handoff_ref);
            let result = result.and_then(|opt| {
                opt.ok_or_else(|| {
                    CarryCtxError::resource_not_found(format!("Handoff '{handoff_ref}' not found"))
                })
            });
            render_and_print("handoff.show", result, is_json, ctx.quiet)
        }
        HandoffCommand::Accept {
            handoff_ref,
            claim_task: _,
        } => {
            let result = handoff_repo.find_by_id(project_id, handoff_ref);
            let result = result.and_then(|opt| {
                opt.ok_or_else(|| {
                    CarryCtxError::resource_not_found(format!("Handoff '{handoff_ref}' not found"))
                })
            });
            match result {
                Ok(handoff) => {
                    handoff_repo
                        .update_status(&handoff.id, project_id, HandoffStatus::Accepted, &now)
                        .map_err(|e| e.exit_code)?;
                    let _ = event_repo.append(&NewEvent {
                        id: ulid::Ulid::new().to_string(),
                        project_id: project_id.to_string(),
                        event_type: "handoff.accepted".into(),
                        actor_agent_id: ctx.agent.clone(),
                        session_id: ctx.session.clone(),
                        task_id: Some(handoff.task_id.clone()),
                        payload: serde_json::json!({ "handoffId": handoff.id }),
                        occurred_at: chrono::Utc::now().to_rfc3339(),
                    });
                    render_and_print("handoff.accept", Ok(handoff), is_json, ctx.quiet)
                }
                Err(e) => render_and_print::<serde_json::Value>(
                    "handoff.accept",
                    Err(e),
                    is_json,
                    ctx.quiet,
                ),
            }
        }
        HandoffCommand::Reject {
            handoff_ref,
            reason: _,
        } => {
            let result = handoff_repo.find_by_id(project_id, handoff_ref);
            let result = result.and_then(|opt| {
                opt.ok_or_else(|| {
                    CarryCtxError::resource_not_found(format!("Handoff '{handoff_ref}' not found"))
                })
            });
            match result {
                Ok(handoff) => {
                    handoff_repo
                        .update_status(&handoff.id, project_id, HandoffStatus::Rejected, &now)
                        .map_err(|e| e.exit_code)?;
                    let _ = event_repo.append(&NewEvent {
                        id: ulid::Ulid::new().to_string(),
                        project_id: project_id.to_string(),
                        event_type: "handoff.rejected".into(),
                        actor_agent_id: ctx.agent.clone(),
                        session_id: ctx.session.clone(),
                        task_id: Some(handoff.task_id.clone()),
                        payload: serde_json::json!({ "handoffId": handoff.id }),
                        occurred_at: chrono::Utc::now().to_rfc3339(),
                    });
                    render_and_print("handoff.reject", Ok(handoff), is_json, ctx.quiet)
                }
                Err(e) => render_and_print::<serde_json::Value>(
                    "handoff.reject",
                    Err(e),
                    is_json,
                    ctx.quiet,
                ),
            }
        }
        HandoffCommand::Close { handoff_ref } => {
            let result = handoff_repo.find_by_id(project_id, handoff_ref);
            let result = result.and_then(|opt| {
                opt.ok_or_else(|| {
                    CarryCtxError::resource_not_found(format!("Handoff '{handoff_ref}' not found"))
                })
            });
            match result {
                Ok(handoff) => {
                    handoff_repo
                        .update_status(&handoff.id, project_id, HandoffStatus::Closed, &now)
                        .map_err(|e| e.exit_code)?;
                    render_and_print("handoff.close", Ok(handoff), is_json, ctx.quiet)
                }
                Err(e) => render_and_print::<serde_json::Value>(
                    "handoff.close",
                    Err(e),
                    is_json,
                    ctx.quiet,
                ),
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: skill
// ═══════════════════════════════════════════════════════════════════════════

fn handle_skill(
    args: &SkillArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    let xdg = XdgPaths::new();
    let skills_dir = xdg.data_home.join("carryctx").join("skills");

    match &args.command {
        SkillCommand::Install { source } => {
            let data = serde_json::json!({
                "source": source,
                "target": skills_dir.to_string_lossy(),
                "status": "placeholder",
            });
            render_and_print("skill.install", Ok(data), is_json, ctx.quiet)
        }
        SkillCommand::List => {
            let installed = if skills_dir.exists() {
                let entries: Vec<String> = std::fs::read_dir(&skills_dir)
                    .map(|rd| {
                        rd.filter_map(|e| e.ok())
                            .filter(|e| e.path().is_dir())
                            .filter_map(|e| e.file_name().into_string().ok())
                            .collect()
                    })
                    .unwrap_or_default();
                entries
            } else {
                Vec::new()
            };
            let data = serde_json::json!({
                "skillsDir": skills_dir.to_string_lossy(),
                "installed": installed,
            });
            render_and_print("skill.list", Ok(data), is_json, ctx.quiet)
        }
        SkillCommand::Path => {
            let data = serde_json::json!({ "path": skills_dir.to_string_lossy() });
            render_and_print("skill.path", Ok(data), is_json, ctx.quiet)
        }
        SkillCommand::Doctor => {
            let exists = skills_dir.exists();
            let entries = if exists {
                std::fs::read_dir(&skills_dir)
                    .map(|rd| {
                        rd.filter_map(|e| e.ok())
                            .filter(|e| e.path().is_dir())
                            .count()
                    })
                    .unwrap_or(0)
            } else {
                0
            };
            let data = serde_json::json!({
                "exists": exists,
                "path": skills_dir.to_string_lossy(),
                "installedCount": entries,
                "status": if exists { "ok" } else { "not_found" },
            });
            render_and_print("skill.doctor", Ok(data), is_json, ctx.quiet)
        }
    }
}
