use crate::*;
use carryctx::application;
use carryctx::application::runtime::InvocationContext;
use carryctx::error::ExitCode;
use clap::Parser;

// ── Checkpoint ───────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub enum CheckpointCommand {
    /// List all checkpoints created for the current session or task
    List,
    /// Display details, including changes and state metadata, for a specific checkpoint ULID
    Show { checkpoint_id: String },
    /// Rollback the project and agent state to a previous checkpoint, discarding subsequent changes
    Correct { checkpoint_id: String },
}

#[derive(Parser, Debug)]
pub struct CheckpointArgs {
    /// Checkpoint subcommand to execute
    #[command(subcommand)]
    pub command: Option<CheckpointCommand>,

    /// Attach a "done" progress event (e.g. what was completed) to this checkpoint.
    #[arg(long)]
    pub done: Vec<String>,

    /// Record "remaining" work items (what still needs to be done) at this checkpoint.
    #[arg(long)]
    pub remaining: Vec<String>,

    /// Record any blockers or issues that are preventing further progress.
    #[arg(long)]
    pub blocker: Vec<String>,

    /// Document identified risks or architectural concerns.
    #[arg(long)]
    pub risk: Vec<String>,

    /// Note the very next step or command the agent intends to run.
    #[arg(long)]
    pub next: Vec<String>,

    /// Attach an arbitrary text note or observation to this checkpoint.
    #[arg(long)]
    pub note: Vec<String>,

    /// Explicitly bind this checkpoint to a specific task ULID.
    #[arg(long)]
    pub task: Option<String>,

    /// Explicitly bind this checkpoint to a specific session ULID.
    #[arg(long)]
    pub session: Option<String>,

    /// Do not automatically invoke `git add` or `git commit` to capture file changes.
    #[arg(long)]
    pub no_git: bool,

    /// Embed the active, uncommitted Git diff directly into the checkpoint database record.
    #[arg(long)]
    pub include_diff: bool,
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: checkpoint
// ═══════════════════════════════════════════════════════════════════════════

pub fn handle_checkpoint(
    args: &CheckpointArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    if let Some(result) = check_dry_run(ctx, &format!("checkpoint {:?}", args.command)) {
        return result;
    }
    let mut runtime = try_open_runtime(ctx)?;
    let project_id = &runtime.config.project.id;
    let tx = runtime
        .database
        .connection_mut()
        .transaction()
        .map_err(|e| carryctx::error::CarryCtxError::database_error(e.to_string()).exit_code)?;
    let uow = carryctx::adapter::unit_of_work::UnitOfWork::new(tx);

    let checkpoint_repo = SqliteCheckpointRepository::new(uow.connection());
    let event_repo = SqliteEventRepository::new(uow.connection());
    let git_cli = GitCli::new();

    match &args.command {
        Some(CheckpointCommand::List) => {
            let checkpoints = checkpoint_repo
                .list(project_id, args.task.as_deref())
                .map_err(|e| e.exit_code)?;

            // Markdown format support
            if ctx.format == carryctx::application::runtime::OutputFormat::Markdown {
                let mut out = String::from("# Checkpoints\n\n");
                out.push_str("| ID | Task | Done Items | Created |\n");
                out.push_str("|---|---|---|---|\n");
                for cp in &checkpoints {
                    let id_short = &cp.id[..cp.id.len().min(8)];
                    let task_short = cp.task_id.as_str();
                    let task_trunc = if task_short.len() > 8 {
                        &task_short[..8]
                    } else {
                        task_short
                    };
                    out.push_str(&format!(
                        "| {} | {} | {} | {} |\n",
                        id_short,
                        task_trunc,
                        cp.done.len(),
                        &cp.created_at[..19]
                    ));
                }
                if !ctx.quiet {
                    print!("{out}");
                }
                return Ok(ExitCode::Success);
            }

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
            let resolver =
                carryctx::application::runtime::CurrentEntityResolver::new(project_id, &uow);

            let agent = resolver
                .resolve_agent(
                    ctx.agent.as_deref(),
                    None,
                    None,
                    runtime.config.agent.default_name.as_deref(),
                    runtime.config.agent.default_name.as_deref(),
                )
                .ok();
            let resolved_agent_id = agent.as_ref().map(|a| a.id.clone());

            let t_ref = args.task.as_deref().or(ctx.task.as_deref());
            let t_ref = if t_ref == Some("current") {
                None
            } else {
                t_ref
            };

            let resolved_task_id = match resolver.resolve_task(
                t_ref,
                Some(&ctx.cwd.to_string_lossy()),
                resolved_agent_id.as_deref(),
            ) {
                Ok(Some(t)) => t.id,
                Ok(None) => {
                    return render_and_print::<serde_json::Value>(
                        "checkpoint.create",
                        Err(CarryCtxError::validation_error(
                            "No task specified. Provide --task <TASK_REF> or bind a task to the active session.",
                        )),
                        is_json,
                        ctx.quiet,
                    );
                }
                Err(e) => {
                    return render_and_print::<serde_json::Value>(
                        "checkpoint.create",
                        Err(e),
                        is_json,
                        ctx.quiet,
                    );
                }
            };

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
                task_id: resolved_task_id,
                session_id: args.session.clone().or_else(|| ctx.session.clone()),
                agent_id: resolved_agent_id,
                worktree_id: None,
                branch: runtime.git_project.branch.clone(),
                head: runtime.git_project.head.clone(),
                done: args.done.clone(),
                remaining: args.remaining.clone(),
                blockers: args.blocker.clone(),
                risks: args.risk.clone(),
                next_actions: args.next.clone(),
                notes: args.note.clone(),
                repo_path,
            };
            let now = chrono::Utc::now().to_rfc3339();
            let graph_repo = carryctx::repository::graph::GraphRepository::new(uow.connection());
            let result = application::checkpoint::create_checkpoint(
                &checkpoint_repo,
                &event_repo,
                Some(&graph_repo),
                &git_cli,
                &input,
                &now,
            );
            if result.is_ok() {
                uow.commit().map_err(|e| {
                    carryctx::error::CarryCtxError::database_error(e.to_string()).exit_code
                })?;
            }
            render_and_print("checkpoint.create", result, is_json, ctx.quiet)
        }
    }
}
