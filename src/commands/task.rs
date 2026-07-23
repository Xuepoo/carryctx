use crate::*;
use carryctx::adapter::unit_of_work::UnitOfWork;
use carryctx::application;
use carryctx::application::runtime::InvocationContext;
use carryctx::domain::dependency::DependencyKind;
use carryctx::domain::task::TransitionAction;
use carryctx::error::{CarryCtxError, ExitCode};
use clap::Parser;

// ── Task ─────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub enum TaskCommand {
    /// Create a new task in the project tracking system
    Create {
        /// A short, descriptive title for the task
        #[arg(long)]
        title: String,
        /// Detailed markdown description of the task requirements
        #[arg(long)]
        description: Option<String>,
        /// Priority level (e.g., P0, P1, low, high)
        #[arg(long)]
        priority: Option<String>,
        /// The agent ULID to assign this task to
        #[arg(long)]
        owner: Option<String>,
        /// Initial status (e.g., PLANNED, READY)
        #[arg(long)]
        status: Option<String>,
        /// List of task ULIDs this new task depends on
        #[arg(long)]
        depends_on: Vec<String>,
    },
    /// List tasks matching specified filters
    List {
        /// Filter by exact task status
        #[arg(long)]
        status: Option<String>,
        /// Filter by assigned owner ULID
        #[arg(long)]
        owner: Option<String>,
        /// Only show tasks assigned to the current agent
        #[arg(long)]
        mine: bool,
    },
    /// Show full details of a specific task
    Show { task_ref: String },
    /// Edit the title or priority of an existing task
    Edit {
        task_ref: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        priority: Option<String>,
    },
    /// Claim ownership of an unassigned task
    Claim { task_ref: String },
    /// Release ownership of a currently claimed task
    Release { task_ref: String },
    /// Transition a READY task to IN_PROGRESS and automatically bind it
    Start { task_ref: String },
    /// Mark a task as BLOCKED and require a reason
    Block {
        task_ref: String,
        #[arg(long)]
        reason: String,
    },
    /// Remove the blocked status from a task, returning it to IN_PROGRESS
    Unblock { task_ref: String },
    /// Mark an IN_PROGRESS task as IN_REVIEW
    Review { task_ref: String },
    /// Mark a task as COMPLETED
    Complete { task_ref: String },
    /// Mark a task as CANCELLED and require a reason
    Cancel {
        task_ref: String,
        #[arg(long)]
        reason: String,
    },
    /// Transition a terminal task back to IN_PROGRESS
    Reopen { task_ref: String },
    /// Establish a new dependency link between tasks
    Depend {
        task_ref: String,
        /// The task ULID that the current task depends on
        #[arg(long)]
        on: String,
        /// The type of dependency (e.g., blocks, relates_to)
        #[arg(long)]
        kind: Option<String>,
    },
    /// Remove an existing dependency link between tasks
    Undepend {
        task_ref: String,
        /// The task ULID to sever the dependency with
        #[arg(long)]
        on: String,
    },
}

#[derive(Parser, Debug)]
pub struct TaskArgs {
    /// Task subcommand to execute
    #[command(subcommand)]
    pub command: TaskCommand,
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: task
// ═══════════════════════════════════════════════════════════════════════════

pub fn handle_task(
    args: &TaskArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    if let Some(result) = check_dry_run(ctx, &format!("task {:?}", args.command)) {
        return result;
    }
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
            let tx = conn
                .transaction()
                .map_err(|e| CarryCtxError::database_error(format!("{e}")).exit_code)?;
            let uow = UnitOfWork::new(tx);
            let resolver = application::runtime::CurrentEntityResolver::new(project_id, &uow);
            let agent = match resolver.resolve_agent(
                ctx.agent.as_deref(),
                None,
                None,
                runtime.config.agent.default_name.as_deref(),
                runtime.config.agent.default_name.as_deref(),
            ) {
                Ok(a) => a,
                Err(e) => {
                    return render_and_print::<serde_json::Value>(
                        "task.claim",
                        Err(e),
                        is_json,
                        ctx.quiet,
                    );
                }
            };

            let result = application::task::claim_task(project_id, task_ref, &agent.id, &uow);
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
