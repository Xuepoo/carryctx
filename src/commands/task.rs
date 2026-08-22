use crate::*;
use carryctx::adapter::unit_of_work::UnitOfWork;
use carryctx::application;
use carryctx::application::runtime::InvocationContext;
use carryctx::domain::dependency::DependencyKind;
use carryctx::domain::task::{TaskPriority, TransitionAction};
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
        /// Priority level
        #[arg(long, value_enum)]
        priority: Option<TaskPriority>,
        /// The agent ULID to assign this task to
        #[arg(long)]
        assignee: Option<String>,
        /// Advisory role required for this task
        #[arg(long)]
        required_role: Option<String>,
        /// Team ID or display reference for this task
        #[arg(long)]
        team: Option<String>,
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
        /// Filter by assigned agent ULID (formerly --owner)
        #[arg(long)]
        assignee: Option<String>,
        /// Only show tasks assigned to the current agent
        #[arg(long)]
        mine: bool,
    },
    /// Show full details of a specific task
    Show { task_ref: String },
    /// Edit the title, priority, or description of an existing task
    Edit {
        task_ref: String,
        #[arg(long)]
        title: Option<String>,
        /// Priority level
        #[arg(long, value_enum)]
        priority: Option<TaskPriority>,
        /// Detailed markdown description of the task requirements
        #[arg(long)]
        description: Option<String>,
        /// Advisory role required for this task
        #[arg(long)]
        required_role: Option<String>,
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
        /// The type of dependency: strong or informational (default: strong)
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
    /// Associate an existing task with a Team.
    Team {
        #[command(subcommand)]
        command: TaskTeamCommand,
    },
}

#[derive(Parser, Debug)]
pub enum TaskTeamCommand {
    Set {
        task_ref: String,
        #[arg(long)]
        team: String,
    },
    Unset {
        task_ref: String,
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
    if !is_json {
        if let Some(result) = check_dry_run(ctx, &format!("task {:?}", args.command)) {
            return result;
        }
    }
    let mut runtime = try_open_runtime(ctx)?;
    let project_id = &runtime.config.project.id;
    let conn = runtime.database.connection_mut();
    let verbose = ctx.verbose || runtime.config.output.verbose;

    if ctx.dry_run && is_json {
        let (command, data) = match &args.command {
            TaskCommand::Create { team, .. } => {
                let team_id = team
                    .as_deref()
                    .map(|reference| resolve_team_id(project_id, reference, conn))
                    .transpose()
                    .map_err(|e| {
                        render_and_print_entity::<serde_json::Value>(
                            "task.create",
                            Err(e),
                            true,
                            ctx.quiet,
                            false,
                            None,
                            None,
                        )
                        .err()
                        .unwrap_or(ExitCode::General)
                    })?;
                (
                    "task.create",
                    serde_json::json!({"team_id": team_id, "operation": {"applied": false}}),
                )
            }
            TaskCommand::Team {
                command: TaskTeamCommand::Set { task_ref, team },
            } => {
                let task_id =
                    resolve_task_id(project_id, task_ref, conn).map_err(|e| e.exit_code)?;
                let team_id = if team == "none" {
                    None
                } else {
                    Some(resolve_team_id(project_id, team, conn).map_err(|e| e.exit_code)?)
                };
                let preview_uow = UnitOfWork::begin(conn).map_err(|e| e.exit_code)?;
                let task = application::task::show_task(project_id, &task_id, &preview_uow)
                    .map_err(|e| e.exit_code)?;
                let previous_team_id = task.task.team_id.clone();
                if team_id.is_none() {
                    return render_and_print_entity(
                        "task.team_unset",
                        Ok(task_team_response(task, None, previous_team_id, false)),
                        true,
                        ctx.quiet,
                        false,
                        None,
                        None,
                    );
                }
                (
                    "task.team_set",
                    task_team_response(task, team_id.as_deref(), previous_team_id, false),
                )
            }
            TaskCommand::Team {
                command: TaskTeamCommand::Unset { task_ref },
            } => {
                let task_id =
                    resolve_task_id(project_id, task_ref, conn).map_err(|e| e.exit_code)?;
                let preview_uow = UnitOfWork::begin(conn).map_err(|e| e.exit_code)?;
                let task = application::task::show_task(project_id, &task_id, &preview_uow)
                    .map_err(|e| e.exit_code)?;
                let previous_team_id = task.task.team_id.clone();
                (
                    "task.team_unset",
                    task_team_response(task, None, previous_team_id, false),
                )
            }
            TaskCommand::Edit { .. } => (
                "task.edit",
                serde_json::json!({"operation": {"applied": false}}),
            ),
            TaskCommand::Depend { .. } => (
                "task.depend",
                serde_json::json!({"operation": {"applied": false}}),
            ),
            TaskCommand::Undepend { .. } => (
                "task.undepend",
                serde_json::json!({"operation": {"applied": false}}),
            ),
            TaskCommand::Claim { .. } => (
                "task.claim",
                serde_json::json!({"operation": {"applied": false}}),
            ),
            TaskCommand::Release { .. } => (
                "task.release",
                serde_json::json!({"operation": {"applied": false}}),
            ),
            TaskCommand::Start { .. } => (
                "task.start",
                serde_json::json!({"operation": {"applied": false}}),
            ),
            TaskCommand::Block { .. } => (
                "task.block",
                serde_json::json!({"operation": {"applied": false}}),
            ),
            TaskCommand::Unblock { .. } => (
                "task.unblock",
                serde_json::json!({"operation": {"applied": false}}),
            ),
            TaskCommand::Review { .. } => (
                "task.review",
                serde_json::json!({"operation": {"applied": false}}),
            ),
            TaskCommand::Complete { .. } => (
                "task.complete",
                serde_json::json!({"operation": {"applied": false}}),
            ),
            TaskCommand::Cancel { .. } => (
                "task.cancel",
                serde_json::json!({"operation": {"applied": false}}),
            ),
            TaskCommand::Reopen { .. } => (
                "task.reopen",
                serde_json::json!({"operation": {"applied": false}}),
            ),
            _ => unreachable!(),
        };
        return render_and_print_entity(command, Ok(data), true, ctx.quiet, false, None, None);
    }

    match &args.command {
        TaskCommand::Create {
            title,
            description,
            priority,
            assignee,
            required_role,
            team,
            status,
            depends_on,
        } => {
            let parsed_status = parse_opt(
                status.as_deref(),
                parse_task_status,
                "task.create",
                ctx,
                is_json,
                verbose,
                &runtime.config.output.fields,
            )?;

            let uow = UnitOfWork::begin(conn).map_err(|e| e.exit_code)?;
            let result = application::task::create_task(
                project_id,
                title,
                description.as_deref(),
                Some(&runtime.config.project.task_prefix),
                parsed_status,
                *priority,
                assignee.as_deref(),
                required_role.as_deref(),
                team.as_deref(),
                depends_on,
                ctx.agent.as_deref(),
                &uow,
            );
            let committed = result.and_then(|t| uow.commit().map(|_| t));
            render_and_print_entity(
                "task.create",
                committed,
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        TaskCommand::List {
            status,
            assignee,
            mine,
        } => {
            let parsed_status = parse_opt(
                status.as_deref(),
                parse_task_status,
                "task.list",
                ctx,
                is_json,
                verbose,
                &runtime.config.output.fields,
            )?;
            let filter = TaskFilter {
                project_id: project_id.to_string(),
                status: parsed_status,
                owner_agent_id: assignee.clone(),
                ready: false,
                blocked: false,
                mine: if *mine { ctx.agent.clone() } else { None },
            };
            let uow = UnitOfWork::begin(conn).map_err(|e| e.exit_code)?;
            let result = application::task::list_tasks(project_id, &filter, &uow);

            // Markdown format support
            if ctx.format == carryctx::application::runtime::OutputFormat::Markdown {
                let md = match &result {
                    Ok(tasks) => {
                        let mut out = String::from("# Tasks\n\n");
                        out.push_str("| ID | Title | Status | Priority |\n");
                        out.push_str("|---|---|---|---|\n");
                        for t in tasks {
                            out.push_str(&format!(
                                "| {} | {} | {:?} | {:?} |\n",
                                t.display_id, t.title, t.status, t.priority
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

            render_and_print_entity(
                "task.list",
                result,
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        TaskCommand::Show { task_ref } => {
            let uow = UnitOfWork::begin(conn).map_err(|e| e.exit_code)?;
            let result = application::task::show_task(project_id, task_ref, &uow);
            render_and_print_entity(
                "task.show",
                result,
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        TaskCommand::Edit {
            task_ref,
            title,
            priority,
            description,
            required_role,
        } => {
            let uow = UnitOfWork::begin(conn).map_err(|e| e.exit_code)?;
            let result = application::task::edit_task(
                project_id,
                task_ref,
                title.as_deref(),
                *priority,
                description.as_deref(),
                required_role.as_deref(),
                ctx.agent.as_deref(),
                &uow,
            );
            let committed = result.and_then(|t| uow.commit().map(|_| t));
            render_and_print_entity(
                "task.edit",
                committed,
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        TaskCommand::Claim { task_ref } => {
            let uow = UnitOfWork::begin(conn).map_err(|e| e.exit_code)?;
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
                    return render_and_print_entity::<serde_json::Value>(
                        "task.claim",
                        Err(e),
                        is_json,
                        ctx.quiet,
                        verbose,
                        ctx.fields.as_deref(),
                        Some(&runtime.config.output.fields),
                    );
                }
            };

            let result = application::task::claim_task(project_id, task_ref, &agent.id, &uow);
            let committed = result.and_then(|t| uow.commit().map(|_| t));
            render_and_print_entity(
                "task.claim",
                committed,
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        TaskCommand::Release { task_ref } => {
            let uow = UnitOfWork::begin(conn).map_err(|e| e.exit_code)?;
            let result = application::task::transition_task(
                project_id,
                task_ref,
                TransitionAction::Release,
                None,
                runtime.config.task.strict_completion,
                ctx.agent.as_deref(),
                &uow,
            );
            let warnings = result.as_ref().map(|(_, w)| w.clone()).unwrap_or_default();
            let committed = result.and_then(|(t, _w)| uow.commit().map(|_| t));
            render_and_print_entity_with_warnings(
                "task.release",
                committed,
                is_json,
                ctx.quiet,
                verbose,
                warnings,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        TaskCommand::Start { task_ref } => {
            let uow = UnitOfWork::begin(conn).map_err(|e| e.exit_code)?;
            let result = application::task::transition_task(
                project_id,
                task_ref,
                TransitionAction::Start,
                None,
                runtime.config.task.strict_completion,
                ctx.agent.as_deref(),
                &uow,
            );
            let warnings = result.as_ref().map(|(_, w)| w.clone()).unwrap_or_default();
            let committed = result.and_then(|(t, _w)| uow.commit().map(|_| t));
            render_and_print_entity_with_warnings(
                "task.start",
                committed,
                is_json,
                ctx.quiet,
                verbose,
                warnings,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        TaskCommand::Block { task_ref, reason } => {
            let uow = UnitOfWork::begin(conn).map_err(|e| e.exit_code)?;
            let result = application::task::transition_task(
                project_id,
                task_ref,
                TransitionAction::Block,
                Some(reason),
                runtime.config.task.strict_completion,
                ctx.agent.as_deref(),
                &uow,
            );
            let warnings = result.as_ref().map(|(_, w)| w.clone()).unwrap_or_default();
            let committed = result.and_then(|(t, _w)| uow.commit().map(|_| t));
            render_and_print_entity_with_warnings(
                "task.block",
                committed,
                is_json,
                ctx.quiet,
                verbose,
                warnings,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        TaskCommand::Unblock { task_ref } => {
            let uow = UnitOfWork::begin(conn).map_err(|e| e.exit_code)?;
            let result = application::task::transition_task(
                project_id,
                task_ref,
                TransitionAction::Unblock,
                None,
                runtime.config.task.strict_completion,
                ctx.agent.as_deref(),
                &uow,
            );
            let warnings = result.as_ref().map(|(_, w)| w.clone()).unwrap_or_default();
            let committed = result.and_then(|(t, _w)| uow.commit().map(|_| t));
            render_and_print_entity_with_warnings(
                "task.unblock",
                committed,
                is_json,
                ctx.quiet,
                verbose,
                warnings,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        TaskCommand::Review { task_ref } => {
            let uow = UnitOfWork::begin(conn).map_err(|e| e.exit_code)?;
            let result = application::task::transition_task(
                project_id,
                task_ref,
                TransitionAction::Review,
                None,
                runtime.config.task.strict_completion,
                ctx.agent.as_deref(),
                &uow,
            );
            let warnings = result.as_ref().map(|(_, w)| w.clone()).unwrap_or_default();
            let committed = result.and_then(|(t, _w)| uow.commit().map(|_| t));
            render_and_print_entity_with_warnings(
                "task.review",
                committed,
                is_json,
                ctx.quiet,
                verbose,
                warnings,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        TaskCommand::Complete { task_ref } => {
            let uow = UnitOfWork::begin(conn).map_err(|e| e.exit_code)?;
            let result = application::task::transition_task(
                project_id,
                task_ref,
                TransitionAction::Complete,
                None,
                runtime.config.task.strict_completion,
                ctx.agent.as_deref(),
                &uow,
            );
            let warnings = result.as_ref().map(|(_, w)| w.clone()).unwrap_or_default();
            let committed = result.and_then(|(t, _w)| uow.commit().map(|_| t));
            render_and_print_entity_with_warnings(
                "task.complete",
                committed,
                is_json,
                ctx.quiet,
                verbose,
                warnings,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        TaskCommand::Cancel { task_ref, reason } => {
            let uow = UnitOfWork::begin(conn).map_err(|e| e.exit_code)?;
            let result = application::task::transition_task(
                project_id,
                task_ref,
                TransitionAction::Cancel,
                Some(reason),
                runtime.config.task.strict_completion,
                ctx.agent.as_deref(),
                &uow,
            );
            let warnings = result.as_ref().map(|(_, w)| w.clone()).unwrap_or_default();
            let committed = result.and_then(|(t, _w)| uow.commit().map(|_| t));
            render_and_print_entity_with_warnings(
                "task.cancel",
                committed,
                is_json,
                ctx.quiet,
                verbose,
                warnings,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        TaskCommand::Reopen { task_ref } => {
            let uow = UnitOfWork::begin(conn).map_err(|e| e.exit_code)?;
            let result = application::task::transition_task(
                project_id,
                task_ref,
                TransitionAction::Reopen,
                None,
                runtime.config.task.strict_completion,
                ctx.agent.as_deref(),
                &uow,
            );
            let warnings = result.as_ref().map(|(_, w)| w.clone()).unwrap_or_default();
            let committed = result.and_then(|(t, _w)| uow.commit().map(|_| t));
            render_and_print_entity_with_warnings(
                "task.reopen",
                committed,
                is_json,
                ctx.quiet,
                verbose,
                warnings,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        TaskCommand::Depend { task_ref, on, kind } => {
            let dep_kind = match parse_opt(
                kind.as_deref(),
                parse_dependency_kind,
                "task.depend",
                ctx,
                is_json,
                verbose,
                &runtime.config.output.fields,
            )? {
                Some(k) => k,
                None => DependencyKind::Strong,
            };
            let uow = UnitOfWork::begin(conn).map_err(|e| e.exit_code)?;
            let result = application::task::add_dependency(
                project_id,
                task_ref,
                on,
                dep_kind,
                ctx.agent.as_deref(),
                &uow,
            );
            let committed = result.and_then(|t| uow.commit().map(|_| t));
            render_and_print_entity(
                "task.depend",
                committed,
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        TaskCommand::Undepend { task_ref, on } => {
            let uow = UnitOfWork::begin(conn).map_err(|e| e.exit_code)?;
            let result = application::task::remove_dependency(
                project_id,
                task_ref,
                on,
                ctx.agent.as_deref(),
                &uow,
            );
            let committed = result.and_then(|t| uow.commit().map(|_| t));
            render_and_print_entity(
                "task.undepend",
                committed,
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        TaskCommand::Team {
            command: TaskTeamCommand::Set { task_ref, team },
        } => {
            let uow = UnitOfWork::begin(conn).map_err(|e| e.exit_code)?;
            let task_id = match resolve_task_id(project_id, task_ref, uow.connection()) {
                Ok(id) => id,
                Err(e) => {
                    return render_and_print_entity::<serde_json::Value>(
                        "task.team_set",
                        Err(e),
                        is_json,
                        ctx.quiet,
                        verbose,
                        ctx.fields.as_deref(),
                        Some(&runtime.config.output.fields),
                    );
                }
            };
            if team == "none" {
                let previous_team_id = application::team::set_task_team(
                    project_id,
                    &task_id,
                    None,
                    ctx.agent.as_deref(),
                    &uow,
                );
                let result = previous_team_id.and_then(|previous_team_id| {
                    application::task::show_task(project_id, &task_id, &uow)
                        .map(|task| task_team_response(task, None, previous_team_id, true))
                });
                let result = result.and_then(|data| uow.commit().map(|_| data));
                return render_and_print_entity(
                    "task.team_unset",
                    result,
                    is_json,
                    ctx.quiet,
                    verbose,
                    ctx.fields.as_deref(),
                    Some(&runtime.config.output.fields),
                );
            }
            let team_id = match crate::resolve_team_id(project_id, team, uow.connection()) {
                Ok(id) => id,
                Err(e) => {
                    return render_and_print_entity::<serde_json::Value>(
                        "task.team_set",
                        Err(e),
                        is_json,
                        ctx.quiet,
                        verbose,
                        ctx.fields.as_deref(),
                        Some(&runtime.config.output.fields),
                    );
                }
            };
            let previous_team_id = match application::team::set_task_team(
                project_id,
                &task_id,
                Some(&team_id),
                ctx.agent.as_deref(),
                &uow,
            ) {
                Ok(previous) => previous,
                Err(e) => {
                    return render_and_print_entity::<serde_json::Value>(
                        "task.team_set",
                        Err(e),
                        is_json,
                        ctx.quiet,
                        verbose,
                        ctx.fields.as_deref(),
                        Some(&runtime.config.output.fields),
                    );
                }
            };
            let result = application::task::show_task(project_id, &task_id, &uow)
                .map(|task| task_team_response(task, Some(&team_id), previous_team_id, true));
            let result = result.and_then(|data| uow.commit().map(|_| data));
            render_and_print_entity(
                "task.team_set",
                result,
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        TaskCommand::Team {
            command: TaskTeamCommand::Unset { task_ref },
        } => {
            let uow = UnitOfWork::begin(conn).map_err(|e| e.exit_code)?;
            let task_id = match resolve_task_id(project_id, task_ref, uow.connection()) {
                Ok(id) => id,
                Err(e) => {
                    return render_and_print_entity::<serde_json::Value>(
                        "task.team_unset",
                        Err(e),
                        is_json,
                        ctx.quiet,
                        verbose,
                        ctx.fields.as_deref(),
                        Some(&runtime.config.output.fields),
                    );
                }
            };
            let previous_team_id = match application::team::set_task_team(
                project_id,
                &task_id,
                None,
                ctx.agent.as_deref(),
                &uow,
            ) {
                Ok(previous) => previous,
                Err(e) => {
                    return render_and_print_entity::<serde_json::Value>(
                        "task.team_unset",
                        Err(e),
                        is_json,
                        ctx.quiet,
                        verbose,
                        ctx.fields.as_deref(),
                        Some(&runtime.config.output.fields),
                    );
                }
            };
            let result = application::task::show_task(project_id, &task_id, &uow)
                .map(|task| task_team_response(task, None, previous_team_id, true));
            let result = result.and_then(|data| uow.commit().map(|_| data));
            render_and_print_entity(
                "task.team_unset",
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

fn task_team_response(
    task: application::task::TaskWithDependencies,
    intended_team_id: Option<&str>,
    previous_team_id: Option<String>,
    applied: bool,
) -> serde_json::Value {
    let mut task = serde_json::to_value(task).expect("task response is serializable");
    task["team_id"] = intended_team_id
        .map(|id| serde_json::Value::String(id.to_owned()))
        .unwrap_or(serde_json::Value::Null);
    serde_json::json!({
        "task": task,
        "previous_team_id": previous_team_id,
        "operation": {"applied": applied}
    })
}

/// Parse an optional enum-style CLI value, rendering a parse failure through
/// the standard entity error path so the message is printed instead of being
/// discarded (the old `.map_err(|e| e.exit_code)?` shape exited 2 silently).
fn parse_opt<T>(
    value: Option<&str>,
    parse: fn(&str) -> Result<T, CarryCtxError>,
    command: &str,
    ctx: &InvocationContext,
    is_json: bool,
    verbose: bool,
    config_fields: &std::collections::HashMap<String, Vec<String>>,
) -> Result<Option<T>, ExitCode> {
    match value {
        None => Ok(None),
        Some(v) => match parse(v) {
            Ok(parsed) => Ok(Some(parsed)),
            Err(e) => {
                let outcome = render_and_print_entity::<serde_json::Value>(
                    command,
                    Err(e),
                    is_json,
                    ctx.quiet,
                    verbose,
                    ctx.fields.as_deref(),
                    Some(config_fields),
                );
                // The rendered entity is always an error here, so only the
                // exit code propagates; the message is already on stderr.
                Err(outcome.err().unwrap_or(ExitCode::General))
            }
        },
    }
}
