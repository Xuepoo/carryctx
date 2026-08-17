use crate::*;
use carryctx::adapter::unit_of_work::UnitOfWork;
use carryctx::application;
use carryctx::application::collaboration::CreateDecisionInput;
use carryctx::application::runtime::InvocationContext;
use carryctx::error::{CarryCtxError, ExitCode};
use clap::Parser;

// ── Decision ─────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub enum DecisionCommand {
    /// Record a new architectural or design decision (ADR)
    Add {
        /// The title or summary of the decision made
        #[arg(long)]
        title: String,
        /// The context, problem statement, or background leading to this decision
        #[arg(long)]
        context: Option<String>,
        /// The actual decision or chosen alternative
        #[arg(long)]
        decision: Option<String>,
        /// The consequences, trade-offs, or impact of this decision
        #[arg(long)]
        consequences: Option<String>,
        /// The reasoning behind this decision: why it was made, not just what was decided
        #[arg(long)]
        rationale: Option<String>,
        /// Task ULID that prompted or is associated with this decision
        #[arg(long)]
        task: Option<String>,
    },
    /// List all decisions recorded in the project (optionally for one task)
    List {
        /// Only show decisions attached to this task (ref: display ID or ULID)
        #[arg(long)]
        task: Option<String>,
    },
    /// Show full details of a specific decision
    Show { decision_ref: String },
    /// Search decisions by keyword or content
    Search { query: String },
    /// Mark a previous decision as superseded by a new one
    Supersede {
        decision_ref: String,
        /// The ULID of the new decision that supersedes this one
        #[arg(long)]
        by: String,
    },
}

#[derive(Parser, Debug)]
pub struct DecisionArgs {
    /// Decision subcommand to execute
    #[command(subcommand)]
    pub command: DecisionCommand,
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: decision
// ═══════════════════════════════════════════════════════════════════════════

pub fn handle_decision(
    args: &DecisionArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    if let Some(result) = check_dry_run(ctx, &format!("decision {:?}", args.command)) {
        return result;
    }
    let mut runtime = try_open_runtime(ctx)?;
    let verbose = ctx.verbose || runtime.config.output.verbose;
    let project_id = &runtime.config.project.id;
    let conn = runtime.database.connection_mut();

    match &args.command {
        DecisionCommand::Add {
            title,
            context,
            decision,
            consequences,
            rationale,
            task,
        } => {
            let task_id = match &task.clone().or_else(|| ctx.task.clone()) {
                Some(t) if !t.is_empty() => match resolve_task_id(project_id, t, conn) {
                    Ok(id) => id,
                    Err(e) => {
                        return render_and_print_entity::<serde_json::Value>(
                            "decision.add",
                            Err(e),
                            is_json,
                            ctx.quiet,
                            verbose,
                            ctx.fields.as_deref(),
                            Some(&runtime.config.output.fields),
                        );
                    }
                },
                _ => {
                    return render_and_print_entity::<serde_json::Value>(
                        "decision.add",
                        Err(CarryCtxError::validation_error(
                            "No task specified. Provide --task <TASK_REF> for the decision.",
                        )),
                        is_json,
                        ctx.quiet,
                        verbose,
                        ctx.fields.as_deref(),
                        Some(&runtime.config.output.fields),
                    );
                }
            };
            let agent_id = match ctx.agent.clone() {
                Some(id) => id,
                None => {
                    return render_and_print_entity::<serde_json::Value>(
                        "decision.add",
                        Err(CarryCtxError::validation_error(
                            "No agent specified. Set CARRYCTX_AGENT or use --agent <AGENT>.",
                        )),
                        is_json,
                        ctx.quiet,
                        verbose,
                        ctx.fields.as_deref(),
                        Some(&runtime.config.output.fields),
                    );
                }
            };

            let uow = UnitOfWork::begin(conn).map_err(|e| e.exit_code)?;
            let input = CreateDecisionInput {
                task_id,
                title: title.clone(),
                context: context.clone(),
                decision: decision.clone(),
                consequences: consequences.clone(),
                rationale: rationale.clone(),
                related_tasks: vec![],
                related_paths: vec![],
                created_by_agent: agent_id,
                created_by_session: ctx.session.clone(),
            };
            let result = application::collaboration::create_decision(project_id, &input, &uow);
            let committed = result.and_then(|d| uow.commit().map(|_| d));
            render_and_print_entity(
                "decision.add",
                committed,
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        DecisionCommand::List { task } => {
            let decision_repo = SqliteDecisionRepository::new(conn);
            // A `--task` ref is resolved and validated first so a bad ref
            // yields RESOURCE_NOT_FOUND instead of silently dumping everything.
            let result = match task.as_deref().filter(|t| !t.trim().is_empty()) {
                Some(ref_) => match resolve_task_id(project_id, ref_, conn) {
                    Ok(task_id) => decision_repo.list_for_task(project_id, &task_id),
                    Err(e) => {
                        return render_and_print_entity::<serde_json::Value>(
                            "decision.list",
                            Err(e),
                            is_json,
                            ctx.quiet,
                            verbose,
                            ctx.fields.as_deref(),
                            Some(&runtime.config.output.fields),
                        );
                    }
                },
                None => decision_repo.list(project_id),
            };

            // Markdown format support
            if ctx.format == carryctx::application::runtime::OutputFormat::Markdown {
                let md = match &result {
                    Ok(decisions) => {
                        let mut out = String::from("# Decisions\n\n");
                        out.push_str("| ID | Title | Agent | Created |\n");
                        out.push_str("|---|---|---|---|\n");
                        for d in decisions {
                            let title_short = if d.title.len() > 40 {
                                format!("{}...", &d.title[..40])
                            } else {
                                d.title.clone()
                            };
                            let agent_short =
                                &d.created_by_agent[..d.created_by_agent.len().min(8)];
                            out.push_str(&format!(
                                "| {} | {} | {} | {} |\n",
                                d.display_id,
                                title_short,
                                agent_short,
                                &d.created_at[..10]
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
                "decision.list",
                result,
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        DecisionCommand::Show { decision_ref } => {
            let decision_repo = SqliteDecisionRepository::new(conn);
            let item = decision_repo
                .find_by_display_id(project_id, decision_ref)
                .map_err(|e| e.exit_code)?
                .or_else(|| {
                    decision_repo
                        .find_by_id(project_id, decision_ref)
                        .ok()
                        .flatten()
                })
                .ok_or(ExitCode::ResourceNotFound)?;
            render_and_print_entity(
                "decision.show",
                Ok(item),
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        DecisionCommand::Search { query } => {
            let decision_repo = SqliteDecisionRepository::new(conn);
            let result = decision_repo.search(project_id, query);
            render_and_print_entity(
                "decision.search",
                result,
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        DecisionCommand::Supersede { decision_ref, by } => {
            let uow = UnitOfWork::begin(conn).map_err(|e| e.exit_code)?;
            let agent_id = ctx.agent.as_deref().unwrap_or("unknown");
            let result = application::collaboration::supersede_decision(
                project_id,
                decision_ref,
                by,
                agent_id,
                ctx.session.as_deref(),
                &uow,
            );
            let committed = result.and_then(|d| uow.commit().map(|_| d));
            render_and_print_entity(
                "decision.supersede",
                committed,
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
    }
}
