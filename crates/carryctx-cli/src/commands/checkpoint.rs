use super::{check_dry_run_envelope, truncate_chars};
use crate::adapter::git::GitCli;
use crate::adapter::sqlite_repos::{SqliteCheckpointRepository, SqliteEventRepository};
use crate::application;
use crate::application::runtime::{InvocationContext, ProjectRuntime};
use crate::cli::{open_runtime_or_report, render_and_print, render_and_print_entity};
use crate::error::{CarryCtxError, ExitCode};
use crate::repository::CheckpointRepository;
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
    pre_opened: Option<ProjectRuntime>,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    let command_label = match &args.command {
        Some(CheckpointCommand::Show { .. }) => "checkpoint.show",
        Some(CheckpointCommand::Correct { .. }) => "checkpoint.correct",
        Some(CheckpointCommand::List) => "checkpoint.list",
        None => "checkpoint.create",
    };
    if let Some(result) = check_dry_run_envelope(
        ctx,
        command_label,
        &format!("checkpoint {:?}", args.command),
    ) {
        return result;
    }
    // Reuse the dispatcher's pre-opened runtime when available; a second
    // open only happens (and reports) when that failed.
    let mut runtime = match pre_opened {
        Some(runtime) => runtime,
        None => open_runtime_or_report(ctx, command_label)?,
    };
    let verbose = ctx.verbose || runtime.config.output.verbose;
    let fields = ctx.fields.as_deref();
    let config_fields = Some(&runtime.config.output.fields);
    // A failed transaction start used to bail with a bare exit code
    // (issue #96 remainder); render it through the standard error envelope.
    let uow =
        match crate::adapter::unit_of_work::UnitOfWork::begin(runtime.database.connection_mut()) {
            Ok(uow) => uow,
            Err(e) => {
                return render_and_print_entity::<serde_json::Value>(
                    command_label,
                    Err(e),
                    is_json,
                    ctx.quiet,
                    verbose,
                    fields,
                    config_fields,
                );
            }
        };
    let project_id = &runtime.config.project.id;

    let checkpoint_repo = SqliteCheckpointRepository::new(uow.connection());
    let event_repo = SqliteEventRepository::new(uow.connection());
    let git_cli = GitCli::new();

    match &args.command {
        Some(CheckpointCommand::List) => {
            let task_ref = args.task.as_deref().or(ctx.task.as_deref());
            let resolved_task_id = match task_ref {
                Some(t_ref) => {
                    match crate::cli::resolve_task_id(project_id, t_ref, uow.connection()) {
                        Ok(id) => Some(id),
                        Err(e) => {
                            return render_and_print::<serde_json::Value>(
                                "checkpoint.list",
                                Err(e),
                                is_json,
                                ctx.quiet,
                            );
                        }
                    }
                }
                None => None,
            };
            let checkpoints = match checkpoint_repo.list(project_id, resolved_task_id.as_deref()) {
                Ok(checkpoints) => checkpoints,
                // A failed listing used to exit bare (issue #96 remainder).
                Err(e) => {
                    return render_and_print_entity::<serde_json::Value>(
                        "checkpoint.list",
                        Err(e),
                        is_json,
                        ctx.quiet,
                        verbose,
                        fields,
                        config_fields,
                    );
                }
            };

            // Markdown format support
            if ctx.format == crate::application::runtime::OutputFormat::Markdown {
                let mut out = String::from("# Checkpoints\n\n");
                out.push_str("| ID | Task | Done Items | Created |\n");
                out.push_str("|---|---|---|---|\n");
                for cp in &checkpoints {
                    let id_short = truncate_chars(&cp.id, 8);
                    let task_trunc = truncate_chars(cp.task_id.as_str(), 8);
                    out.push_str(&format!(
                        "| {} | {} | {} | {} |\n",
                        id_short,
                        task_trunc,
                        cp.done.len(),
                        truncate_chars(&cp.created_at, 19)
                    ));
                }
                if !ctx.quiet {
                    print!("{out}");
                }
                return Ok(ExitCode::Success);
            }

            render_and_print_entity(
                "checkpoint.list",
                Ok(checkpoints),
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        Some(CheckpointCommand::Show { checkpoint_id }) => {
            let cp = match checkpoint_repo.find_by_id(project_id, checkpoint_id) {
                Ok(Some(cp)) => cp,
                Ok(None) => {
                    return render_and_print_entity::<serde_json::Value>(
                        "checkpoint.show",
                        Err(CarryCtxError::resource_not_found(format!(
                            "Checkpoint '{checkpoint_id}' not found."
                        ))),
                        is_json,
                        ctx.quiet,
                        verbose,
                        ctx.fields.as_deref(),
                        Some(&runtime.config.output.fields),
                    );
                }
                Err(e) => {
                    return render_and_print_entity::<serde_json::Value>(
                        "checkpoint.show",
                        Err(e),
                        is_json,
                        ctx.quiet,
                        verbose,
                        ctx.fields.as_deref(),
                        Some(&runtime.config.output.fields),
                    );
                }
            };
            render_and_print_entity(
                "checkpoint.show",
                Ok(cp),
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
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
            render_and_print_entity(
                "checkpoint.correct",
                result,
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        None => {
            let resolver =
                crate::application::runtime::CurrentEntityResolver::new(project_id, &uow);

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

            // CTX-0148: resolve a short `--session`/CARRYCTX_SESSION reference
            // to the canonical ULID. Unknown or ambiguous refs fail closed
            // instead of reaching the `checkpoints.session_id` foreign key.
            let session_id = match args.session.as_deref().or(ctx.session.as_deref()) {
                Some(reference) => {
                    match crate::cli::resolve_session_ref(project_id, reference, uow.connection()) {
                        Ok(id) => Some(id),
                        Err(e) => {
                            return render_and_print::<serde_json::Value>(
                                "checkpoint.create",
                                Err(e),
                                is_json,
                                ctx.quiet,
                            );
                        }
                    }
                }
                None => None,
            };

            let input = application::checkpoint::CreateCheckpointInput {
                project_id: project_id.to_string(),
                task_id: resolved_task_id,
                session_id,
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
            let graph_repo = crate::repository::graph::GraphRepository::new(uow.connection());
            let result = application::checkpoint::create_checkpoint(
                &checkpoint_repo,
                &event_repo,
                Some(&graph_repo),
                &git_cli,
                &input,
                &now,
            );
            if result.is_ok() {
                // A failed commit used to exit bare (issue #96 remainder);
                // surface it as a checkpoint.create error envelope instead.
                if let Err(e) = uow.commit() {
                    return render_and_print_entity::<serde_json::Value>(
                        "checkpoint.create",
                        Err(e),
                        is_json,
                        ctx.quiet,
                        verbose,
                        fields,
                        config_fields,
                    );
                }
            }
            render_and_print_entity(
                "checkpoint.create",
                result,
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
    }
}
