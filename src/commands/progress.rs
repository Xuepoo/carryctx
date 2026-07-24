use crate::*;
use carryctx::application;
use carryctx::application::runtime::InvocationContext;
use carryctx::domain::progress::ProgressType;
use carryctx::error::ExitCode;
use clap::Parser;

// ── Progress ─────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub enum ProgressCommand {
    /// Add a "todo" item to a task
    Todo {
        content: String,
        /// Optional task ULID to attach the progress to (defaults to active task)
        #[arg(long)]
        task: Option<String>,
    },
    /// Add a "blocker" or issue to a task
    Block {
        content: String,
        /// Optional task ULID to attach the progress to (defaults to active task)
        #[arg(long)]
        task: Option<String>,
    },
    /// Add a "risk" identification to a task
    Risk {
        content: String,
        /// Optional task ULID to attach the progress to (defaults to active task)
        #[arg(long)]
        task: Option<String>,
    },
    /// Add a general "note" or observation to a task
    Note {
        content: String,
        /// Optional task ULID to attach the progress to (defaults to active task)
        #[arg(long)]
        task: Option<String>,
    },
    /// List all progress events attached to a task
    List {
        /// Optional task ULID to filter by (defaults to active task)
        #[arg(long)]
        task: Option<String>,
    },
    /// Show full details of a specific progress entry
    Show { progress_ref: String },
    /// Edit the content of an existing progress entry
    Edit {
        progress_ref: String,
        #[arg(long)]
        content: String,
    },
    /// Mark a "todo" or "blocker" as resolved/completed
    Complete { progress_ref: String },
    /// Reopen a previously completed progress entry
    Reopen { progress_ref: String },
    /// Permanently remove a progress entry
    Remove { progress_ref: String },
    /// Reorder progress items within a task
    Reorder {
        /// Task ULID containing the progress items
        #[arg(long)]
        task: String,
        /// Ordered list of progress ULIDs
        #[arg(long)]
        order: Vec<String>,
    },
}

#[derive(Parser, Debug)]
pub struct ProgressArgs {
    /// Progress subcommand to execute
    #[command(subcommand)]
    pub command: ProgressCommand,
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: progress
pub fn handle_progress(
    args: &ProgressArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    if let Some(result) = check_dry_run(ctx, &format!("progress {:?}", args.command)) {
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
    let now = chrono::Utc::now().to_rfc3339();

    let progress_repo = SqliteProgressRepository::new(uow.connection());
    let event_repo = SqliteEventRepository::new(uow.connection());
    let task_repo = SqliteTaskRepository::new(uow.connection());

    match &args.command {
        ProgressCommand::Todo { content, task }
        | ProgressCommand::Block { content, task }
        | ProgressCommand::Risk { content, task }
        | ProgressCommand::Note { content, task } => {
            let item_type = match &args.command {
                ProgressCommand::Todo { .. } => ProgressType::Todo,
                ProgressCommand::Block { .. } => ProgressType::Blocker,
                ProgressCommand::Risk { .. } => ProgressType::Risk,
                ProgressCommand::Note { .. } => ProgressType::Note,
                _ => unreachable!(),
            };
            let resolver =
                carryctx::application::runtime::CurrentEntityResolver::new(project_id, &uow);
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
            let task_id = match resolver.resolve_task(
                task.as_deref().or(ctx.task.as_deref()),
                Some(&ctx.cwd.to_string_lossy()),
                agent_id.as_deref(),
            ) {
                Ok(Some(t)) => t.id,
                Ok(None) => {
                    return render_and_print::<serde_json::Value>(
                        "progress.create",
                        Err(CarryCtxError::validation_error(
                            "No task specified. Provide --task <TASK_REF> or bind a task to the active session.",
                        )),
                        is_json,
                        ctx.quiet,
                    );
                }
                Err(e) => {
                    return render_and_print::<serde_json::Value>(
                        "progress.create",
                        Err(e),
                        is_json,
                        ctx.quiet,
                    );
                }
            };
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
            if result.is_ok() {
                uow.commit().map_err(|e| {
                    carryctx::error::CarryCtxError::database_error(e.to_string()).exit_code
                })?;
            }
            render_and_print("progress.create", result, is_json, ctx.quiet)
        }
        ProgressCommand::List { task } => {
            let resolver =
                carryctx::application::runtime::CurrentEntityResolver::new(project_id, &uow);
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
            let task_id = match resolver.resolve_task(
                task.as_deref().or(ctx.task.as_deref()),
                Some(&ctx.cwd.to_string_lossy()),
                agent_id.as_deref(),
            ) {
                Ok(Some(t)) => t.id,
                Ok(None) => {
                    return render_and_print::<serde_json::Value>(
                        "progress.list",
                        Err(CarryCtxError::validation_error(
                            "No task specified. Provide --task <TASK_REF> or bind a task to the active session.",
                        )),
                        is_json,
                        ctx.quiet,
                    );
                }
                Err(e) => {
                    return render_and_print::<serde_json::Value>(
                        "progress.list",
                        Err(e),
                        is_json,
                        ctx.quiet,
                    );
                }
            };
            let filter = ProgressFilter {
                project_id: project_id.to_string(),
                task_id,
                include_removed: false,
            };
            let result = application::progress::list_progress(&progress_repo, &task_repo, &filter);

            // Markdown format support
            if ctx.format == carryctx::application::runtime::OutputFormat::Markdown {
                let md = match &result {
                    Ok(items) => {
                        let mut out = String::from("# Progress Items\n\n");
                        out.push_str("| ID | Type | Content | Status | Position |\n");
                        out.push_str("|---|---|---|---|---|\n");
                        for p in items {
                            let content_short = if p.content.len() > 40 {
                                format!("{}...", &p.content[..40])
                            } else {
                                p.content.clone()
                            };
                            out.push_str(&format!(
                                "| {} | {:?} | {} | {:?} | {} |\n",
                                p.display_id, p.item_type, content_short, p.status, p.position
                            ));
                        }
                        out
                    }
                    Err(e) => format!("Error: {e}"),
                };
                if !ctx.quiet {
                    print!("{md}");
                }
                return Ok(ExitCode::Success);
            }

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
            if result.is_ok() {
                uow.commit().map_err(|e| {
                    carryctx::error::CarryCtxError::database_error(e.to_string()).exit_code
                })?;
            }
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
            if result.is_ok() {
                uow.commit().map_err(|e| {
                    carryctx::error::CarryCtxError::database_error(e.to_string()).exit_code
                })?;
            }
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
            if result.is_ok() {
                uow.commit().map_err(|e| {
                    carryctx::error::CarryCtxError::database_error(e.to_string()).exit_code
                })?;
            }
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
            if result.is_ok() {
                uow.commit().map_err(|e| {
                    carryctx::error::CarryCtxError::database_error(e.to_string()).exit_code
                })?;
            }
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
            if result.is_ok() {
                uow.commit().map_err(|e| {
                    carryctx::error::CarryCtxError::database_error(e.to_string()).exit_code
                })?;
            }
            render_and_print("progress.reorder", result, is_json, ctx.quiet)
        }
    }
}
