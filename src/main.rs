use std::path::Path;
use std::sync::Arc;

use clap::Parser;

use carryctx::adapter::config::ConfigLoader;
use carryctx::adapter::filesystem::AdmissionLock;
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

/// Local-first memory for coding agents.
///
/// CarryCtx persists task state, checkpoints, decisions, and Git-aware context
/// across closed windows, restarted sessions, different agents, and multiple
/// Git worktrees — so `carryctx resume` always picks up exactly where you left off.
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
    #[arg(long, global = true, env = "CARRYCTX_AGENT", alias = "owner")]
    pub agent: Option<String>,

    /// The ULID of the active session. If not provided, the global or worktree active session is used.
    #[arg(long, global = true, env = "CARRYCTX_SESSION")]
    pub session: Option<String>,

    /// The ULID of the task currently being worked on. Context defaults to this task if set.
    #[arg(long, global = true, env = "CARRYCTX_TASK")]
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

    /// Comma-separated list of entity fields to keep in output (e.g. 'display_id,status,title').
    /// Overrides the per-command `[output.fields]` configuration for this invocation.
    #[arg(long, global = true, value_delimiter = ',')]
    pub fields: Option<Vec<String>>,

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
    /// Manage durable project team membership and coordination relations
    Team(TeamArgs),
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
    /// Full-text search across tasks, progress items, checkpoints, and decisions
    Search(SearchArgs),
}

// ═══════════════════════════════════════════════════════════════════════════
//  main()
// ═══════════════════════════════════════════════════════════════════════════

fn main() {
    // Convert broken-pipe panics (e.g. `carryctx ... | head`) into a clean
    // SIGPIPE-style exit instead of printing a panic message on stderr.
    install_broken_pipe_hook();
    // Initialize tracing subscriber for RUST_LOG support
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(true)
        .try_init();

    let cli = Cli::parse();
    let result = run(cli);
    match result {
        Ok(exit_code) => std::process::exit(exit_code as i32),
        Err(code) => std::process::exit(code as i32),
    }
}

/// When stdout (or stderr) is closed early — `carryctx task list | head -1` —
/// `println!` panics with "failed printing to stdout: Broken pipe". Catch that
/// panic and exit with 141 (128 + SIGPIPE), the conventional Unix behavior, so
/// piped invocations terminate silently instead of crashing with a backtrace.
fn install_broken_pipe_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| info.payload().downcast_ref::<String>().map(|s| s.as_str()))
            .unwrap_or("");
        if message.contains("Broken pipe") {
            std::process::exit(128 + 13);
        }
        default_hook(info);
    }));
}

// ═══════════════════════════════════════════════════════════════════════════
//  run()
// ═══════════════════════════════════════════════════════════════════════════

fn run(cli: Cli) -> Result<ExitCode, ExitCode> {
    tracing::debug!(command = ?cli.command, "dispatching command");
    let mut ctx = build_invocation_context(&cli)?;
    ctx.read_only = matches!(&cli.command, Some(Commands::Team(args))
        if matches!(&args.command, TeamCommand::Status { .. } | TeamCommand::Context { .. }));
    let is_json = matches!(ctx.format, OutputFormat::Json);
    let direct_lock = matches!(
        &cli.command,
        Some(Commands::Init(_))
            | Some(Commands::Sync(_))
            | Some(Commands::Project(ProjectArgs {
                command: ProjectCommand::Restore { .. }
            }))
    );
    // Config and hook commands stay in the normal admission path because
    // their mutating variants write project files. The only commands that
    // bypass this pre-dispatch lock are operations that acquire their own
    // lock immediately before database replacement/copy or initialization.
    if !ctx.read_only && !direct_lock {
        let git = GitCli::new();
        let project = git.discover(resolve_work_dir(&ctx)).map_err(|error| {
            report_runtime_open_error("runtime.open", &error, is_json);
            error.exit_code
        })?;
        let xdg = XdgPaths::new();
        let lock = acquire_runtime_lock(&xdg.admission_lock_dir(&project.git_common_dir)).map_err(
            |error| {
                report_runtime_open_error("runtime.open", &error, is_json);
                error.exit_code
            },
        )?;
        ctx.admission_lock = Some(Arc::new(lock));
    }
    // Pre-dispatch open: normalizes --agent/CARRYCTX_AGENT to a ULID and
    // primes the runtime so the entity commands can reuse it instead of
    // opening (and migrating) the database a second time. A failure here is
    // deliberately ignored; the command handler re-opens and reports through
    // the standard error envelope.
    let mut pre_opened: Option<ProjectRuntime> = None;
    if !direct_lock {
        if let Ok(runtime) = try_open_runtime(&ctx) {
            if let Some(agent_ref) = &ctx.agent {
                if !agent_ref.trim().is_empty() {
                    if let Ok(agent_id) = resolve_agent_id(
                        &runtime.config.project.id,
                        agent_ref,
                        runtime.database.connection(),
                    ) {
                        ctx.agent = Some(agent_id);
                    }
                }
            }
            pre_opened = Some(runtime);
        }
    }
    if let Some(Commands::Team(args)) = &cli.command {
        if matches!(
            &args.command,
            TeamCommand::Status { .. } | TeamCommand::Context { .. }
        ) {
            return handle_team(args, &ctx, is_json);
        }
    }
    match &cli.command {
        Some(Commands::Init(args)) => handle_init(args, &ctx),
        Some(Commands::Status(args)) => handle_status(args, pre_opened.take(), &ctx, is_json),
        Some(Commands::Resume(args)) => handle_resume(args, &ctx, is_json),
        Some(Commands::Context(args)) => handle_context(args, &ctx, is_json),
        Some(Commands::Checkpoint(args)) => {
            handle_checkpoint(args, pre_opened.take(), &ctx, is_json)
        }
        Some(Commands::Doctor(args)) => handle_doctor(args, &ctx, is_json),
        Some(Commands::Agent(args)) => handle_agent(args, pre_opened.take(), &ctx, is_json),
        Some(Commands::Task(args)) => handle_task(args, &ctx, is_json),
        Some(Commands::Team(args)) => handle_team(args, &ctx, is_json),
        Some(Commands::Session(args)) => handle_session(args, pre_opened.take(), &ctx, is_json),
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
        Some(Commands::Search(args)) => handle_search(args, pre_opened.take(), &ctx, is_json),
        None => {
            if !ctx.quiet {
                println!(
                    "CarryCtx v{} — Local-first memory for coding agents",
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
    let is_json = cli.json || cli.format.as_deref() == Some("json");
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
        cli.fields.clone(),
    )
    .map_err(|e| {
        // Context construction failures used to be mapped to a bare exit
        // code, discarding the explanation. Report like every other
        // pre-dispatch failure.
        report_runtime_open_error("carryctx", &e, is_json);
        e.exit_code
    })
}

pub fn resolve_work_dir(ctx: &InvocationContext) -> &Path {
    ctx.project.as_deref().map(Path::new).unwrap_or(&ctx.cwd)
}

/// Surface a failure that happens before any command handler runs (git
/// discovery, admission lock). Previously text mode printed nothing while
/// still exiting non-zero, leaving silent failures; now both modes report:
/// the standard error envelope on stderr in JSON mode, a human-readable
/// `Error [CODE]: message` line on stderr otherwise.
fn report_runtime_open_error(command: &str, error: &CarryCtxError, is_json: bool) {
    if is_json {
        let (text, _, _) = output::render_json::<serde_json::Value>(command, Err(error), true);
        eprintln!("{text}");
    } else {
        eprintln!("Error [{}]: {}", error.code, error.message);
    }
}

/// Open the project runtime, preserving the underlying [`CarryCtxError`]
/// (config parse failures, migration errors, …) instead of collapsing it to
/// an exit code. Callers that only need the exit code use
/// [`try_open_runtime`]; user-facing handlers should prefer
/// [`open_runtime_or_report`].
pub fn open_runtime(ctx: &InvocationContext) -> Result<ProjectRuntime, CarryCtxError> {
    let xdg = XdgPaths::new();
    let cfg_loader = ConfigLoader::new(xdg.clone());
    let work_dir = resolve_work_dir(ctx);
    let mut config = cfg_loader.load(Some(work_dir))?;
    let git = GitCli::new();
    let git_project = git.discover(work_dir)?;
    let db_path = xdg.project_db(&git_project.git_common_dir);
    let admission_lock = if ctx.read_only {
        None
    } else if let Some(lock) = &ctx.admission_lock {
        Some(lock.clone())
    } else {
        Some(Arc::new(acquire_runtime_lock(
            &xdg.admission_lock_dir(&git_project.git_common_dir),
        )?))
    };
    if !ctx.read_only {
        carryctx::application::project_mgmt::recover_restore_journals(
            &xdg,
            &git_project.git_common_dir,
        )?;
        carryctx::application::project_mgmt::recover_sync_journals(
            &xdg,
            &git_project.git_common_dir,
        )?;
        carryctx::application::worktree::recover_worktree_create_journals(
            &xdg,
            &git_project.git_common_dir,
        )?;
    }
    let database = if ctx.read_only {
        let database = ProjectDatabase::open_readonly(&db_path)?;
        database.is_up_to_date()?;
        database
    } else {
        let mut database = ProjectDatabase::open(&db_path)?;
        database.migrate()?;
        database
    };

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
        admission_lock,
    })
}

/// Legacy thin wrapper: open the runtime and collapse any error to its bare
/// exit code. Kept for callers outside the owned command surface.
pub fn try_open_runtime(ctx: &InvocationContext) -> Result<ProjectRuntime, ExitCode> {
    open_runtime(ctx).map_err(|e| e.exit_code)
}

/// Open the project runtime for `command`, rendering any failure through the
/// standard error path — the error envelope on stderr in JSON mode, a human
/// readable `Error [CODE]: message` line otherwise — so opening failures can
/// explain themselves instead of exiting silently with a bare code.
pub fn open_runtime_or_report(
    ctx: &InvocationContext,
    command: &str,
) -> Result<ProjectRuntime, ExitCode> {
    match open_runtime(ctx) {
        Ok(runtime) => Ok(runtime),
        Err(error) => {
            let is_json = matches!(ctx.format, OutputFormat::Json);
            report_runtime_open_error(command, &error, is_json);
            Err(error.exit_code)
        }
    }
}

/// Retry schedule for acquiring the project-wide admission lock.
///
/// The lock is a single-writer lock held for the whole process duration,
/// and the MCP server plus parallel subagents legitimately contend for it.
/// Instead of hard-erroring after a fixed 2.5s cap, latecomers queue with
/// exponential backoff: 25ms doubling up to 400ms per attempt, bounded by
/// a ~20s total retry budget before STATE_CONFLICT is surfaced. Long
/// operations therefore delay — rather than fail — their contenders.
const LOCK_RETRY_INITIAL_DELAY_MS: u64 = 25;
const LOCK_RETRY_MAX_DELAY_MS: u64 = 400;
const LOCK_RETRY_BUDGET_MS: u64 = 20_000;

/// Next sleep for the admission-lock retry loop.
///
/// Returns 0 once the retry budget is spent, which terminates the loop.
fn next_backoff_delay_ms(attempt: u32, waited_ms: u64) -> u64 {
    if waited_ms >= LOCK_RETRY_BUDGET_MS {
        return 0;
    }
    let doubling = LOCK_RETRY_INITIAL_DELAY_MS
        .checked_shl(attempt)
        .unwrap_or(LOCK_RETRY_MAX_DELAY_MS);
    doubling
        .min(LOCK_RETRY_MAX_DELAY_MS)
        .min(LOCK_RETRY_BUDGET_MS - waited_ms)
}

fn acquire_runtime_lock(path: &Path) -> Result<AdmissionLock, CarryCtxError> {
    let operation_id = ulid::Ulid::generate().to_string();
    let hostname = resolve_hostname();
    let now = chrono::Utc::now().to_rfc3339();
    let mut waited_ms: u64 = 0;
    let mut attempt: u32 = 0;
    loop {
        match AdmissionLock::acquire(path, &operation_id, std::process::id(), &hostname, &now) {
            Ok(lock) => return Ok(lock),
            Err(error) if error.code == "STATE_CONFLICT" => {
                let delay_ms = next_backoff_delay_ms(attempt, waited_ms);
                // Budget exhausted: surface the latest contention error.
                if delay_ms == 0 {
                    return Err(error);
                }
                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                waited_ms += delay_ms;
                attempt += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Resolve this machine's hostname for admission-lock ownership records.
///
/// `gethostname(2)` (Unix) and `/etc/hostname` are consulted first because
/// the `HOSTNAME` environment variable is shell-specific — bash usually
/// exports it, but sh, CI runners, and many service contexts do not — and
/// it is only used as the last resort before falling back to "unknown".
fn resolve_hostname() -> String {
    #[cfg(unix)]
    if let Some(host) = hostname_from_gethostname() {
        return host;
    }
    if let Ok(content) = std::fs::read_to_string("/etc/hostname") {
        if let Some(host) = first_hostname_line(&content) {
            return host;
        }
    }
    if let Ok(host) = std::env::var("HOSTNAME") {
        let trimmed = host.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    "unknown".to_string()
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn hostname_from_gethostname() -> Option<String> {
    // SAFETY: gethostname(2) writes at most `buf.len()` bytes into the
    // provided buffer and NUL-terminates within it on success; it performs
    // no allocation and no pointer is retained afterwards.
    let mut buf = [0u8; 256];
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let host = std::str::from_utf8(&buf[..end]).ok()?.trim();
    (!host.is_empty()).then(|| host.to_string())
}

/// First meaningful hostname entry from `/etc/hostname` content.
fn first_hostname_line(content: &str) -> Option<String> {
    content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
}
pub fn render_and_print<T: serde::Serialize>(
    command: &str,
    result: Result<T, CarryCtxError>,
    is_json: bool,
    quiet: bool,
) -> Result<ExitCode, ExitCode> {
    render_and_print_with_warnings(command, result, is_json, quiet, vec![])
}

/// Same as `render_and_print`, but attaches non-fatal `warnings` to a
/// successful response instead of silently discarding them.
pub fn render_and_print_with_warnings<T: serde::Serialize>(
    command: &str,
    result: Result<T, CarryCtxError>,
    is_json: bool,
    quiet: bool,
    warnings: Vec<String>,
) -> Result<ExitCode, ExitCode> {
    let (output, sink, exit_code) = match &result {
        Ok(data) => output::render_json_with_warnings(command, Ok(data), is_json, warnings),
        Err(err) => output::render_json_with_warnings::<serde_json::Value>(
            command,
            Err(err),
            is_json,
            vec![],
        ),
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

/// Render an entity result with compact text output by default. Pass
/// `verbose = true` (the global `--verbose` flag or `[output] verbose` config)
/// to restore the full pretty-printed record in text mode. JSON output is
/// always the full envelope.
///
/// `cli_fields` (`--fields`) and `config_fields` (`[output.fields]`)
/// optionally narrow emitted records to an allowlist of fields.
pub fn render_and_print_entity<T: serde::Serialize>(
    command: &str,
    result: Result<T, CarryCtxError>,
    is_json: bool,
    quiet: bool,
    verbose: bool,
    cli_fields: Option<&[String]>,
    config_fields: Option<&std::collections::HashMap<String, Vec<String>>>,
) -> Result<ExitCode, ExitCode> {
    render_and_print_entity_with_warnings(
        command,
        result,
        is_json,
        quiet,
        verbose,
        vec![],
        cli_fields,
        config_fields,
    )
}

/// Like `render_and_print_entity`, but attaches non-fatal `warnings` to a
/// successful response.
#[allow(clippy::too_many_arguments)]
pub fn render_and_print_entity_with_warnings<T: serde::Serialize>(
    command: &str,
    result: Result<T, CarryCtxError>,
    is_json: bool,
    quiet: bool,
    verbose: bool,
    warnings: Vec<String>,
    cli_fields: Option<&[String]>,
    config_fields: Option<&std::collections::HashMap<String, Vec<String>>>,
) -> Result<ExitCode, ExitCode> {
    let (output, sink, exit_code) = match &result {
        Ok(data) => output::render_entity(
            command,
            Ok(data),
            is_json,
            verbose,
            cli_fields,
            config_fields,
            warnings,
        ),
        Err(err) => output::render_entity::<serde_json::Value>(
            command,
            Err(err),
            is_json,
            verbose,
            cli_fields,
            config_fields,
            vec![],
        ),
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
        return require_active_agent(agent).map(|agent| agent.id);
    }
    if let Some(agent) = repo.find_by_id(project_id, agent_ref)? {
        return require_active_agent(agent).map(|agent| agent.id);
    }
    Err(CarryCtxError::resource_not_found(format!(
        "Agent '{agent_ref}' not found."
    )))
}

/// Deactivated agents must not resolve as actors. Mirrors the runtime
/// `CurrentEntityResolver` guard so a deactivated reference is rejected with
/// the same semantics at both the early command layer and inside handlers.
fn require_active_agent(
    agent: carryctx::domain::agent::Agent,
) -> Result<carryctx::domain::agent::Agent, CarryCtxError> {
    if agent.status == carryctx::domain::agent::AgentStatus::Active {
        Ok(agent)
    } else {
        Err(CarryCtxError::permission_scope(format!(
            "Agent '{}' is deactivated and cannot act.",
            agent.name
        )))
    }
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
    // Case-insensitive like parse_task_priority: agents routinely pass
    // "IN_PROGRESS" or "Completed" from shell variables and LLM output.
    match s.to_ascii_lowercase().as_str() {
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
    match s.to_ascii_lowercase().as_str() {
        "low" | "backlog" => Ok(TaskPriority::Low),
        "normal" | "medium" => Ok(TaskPriority::Normal),
        "high" => Ok(TaskPriority::High),
        "urgent" | "critical" => Ok(TaskPriority::Urgent),
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

#[cfg(test)]
mod hostname_backoff_tests {
    use super::*;

    #[test]
    fn backoff_doubles_then_caps_at_max_delay() {
        assert_eq!(next_backoff_delay_ms(0, 0), 25);
        assert_eq!(next_backoff_delay_ms(1, 25), 50);
        assert_eq!(next_backoff_delay_ms(2, 75), 100);
        assert_eq!(next_backoff_delay_ms(3, 175), 200);
        assert_eq!(next_backoff_delay_ms(4, 375), 400);
        // Beyond the doubling range the delay stays at its cap.
        assert_eq!(next_backoff_delay_ms(5, 775), 400);
        assert_eq!(next_backoff_delay_ms(40, 15_000), 400);
    }

    #[test]
    fn backoff_respects_total_budget_and_terminates() {
        assert_eq!(next_backoff_delay_ms(5, LOCK_RETRY_BUDGET_MS - 10), 10);
        assert_eq!(next_backoff_delay_ms(5, LOCK_RETRY_BUDGET_MS - 300), 300);
        assert_eq!(
            next_backoff_delay_ms(5, LOCK_RETRY_BUDGET_MS),
            0,
            "exhausted budget must stop the retry loop"
        );
        assert_eq!(
            next_backoff_delay_ms(5, LOCK_RETRY_BUDGET_MS + 1),
            0,
            "overshoot must stop the retry loop"
        );
    }

    #[test]
    fn first_hostname_line_skips_blanks_and_comments() {
        assert_eq!(first_hostname_line("myhost\n"), Some("myhost".into()));
        assert_eq!(
            first_hostname_line("\n \n# comment\nreal\n"),
            Some("real".into())
        );
        assert_eq!(first_hostname_line(""), None);
        assert_eq!(first_hostname_line("   \n#\n"), None);
    }

    #[test]
    fn resolve_hostname_returns_a_usable_token() {
        let host = resolve_hostname();
        assert!(!host.is_empty(), "hostname resolution must never be empty");
        assert!(!host.contains('\n'), "hostname must be a single token");
    }

    #[test]
    fn parse_task_status_is_case_insensitive_like_priority() {
        for (raw, expected) in [
            ("planned", "planned"),
            ("READY", "ready"),
            ("In_Progress", "inprogress"),
            ("BLOCKED", "blocked"),
            ("Review", "review"),
            ("COMPLETED", "completed"),
            ("Cancelled", "cancelled"),
        ] {
            let parsed = parse_task_status(raw)
                .unwrap_or_else(|e| panic!("status '{raw}' must parse case-insensitively: {e}"));
            assert_eq!(format!("{parsed:?}").to_ascii_lowercase(), expected);
        }
        // Unknown values still fail, and the error echoes normalized input.
        let err = parse_task_status("NOT_A_STATUS").unwrap_err();
        assert_eq!(err.code, "INVALID_ARGUMENTS", "{err:?}");
    }
}
