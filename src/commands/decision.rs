use crate::*;
use carryctx::application::runtime::InvocationContext;
use carryctx::domain::collaboration::Decision;
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
        /// Task ULID that prompted or is associated with this decision
        #[arg(long)]
        task: Option<String>,
    },
    /// List all decisions recorded in the project
    List,
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
            task,
        } => {
            let task_id = match task.clone().or_else(|| ctx.task.clone()) {
                Some(ref t) if !t.is_empty() => {
                    match resolve_task_id(project_id, t, conn) {
                        Ok(id) => id,
                        Err(e) => {
                            return render_and_print::<serde_json::Value>(
                                "decision.add",
                                Err(e),
                                is_json,
                                ctx.quiet,
                            );
                        }
                    }
                }
                _ => {
                    return render_and_print::<serde_json::Value>(
                        "decision.add",
                        Err(CarryCtxError::validation_error(
                            "No task specified. Provide --task <TASK_REF> for the decision.",
                        )),
                        is_json,
                        ctx.quiet,
                    );
                }
            };
            let agent_id = match ctx.agent.clone() {
                Some(id) => id,
                None => {
                    return render_and_print::<serde_json::Value>(
                        "decision.add",
                        Err(CarryCtxError::validation_error(
                            "No agent specified. Set CARRYCTX_AGENT or use --agent <AGENT>.",
                        )),
                        is_json,
                        ctx.quiet,
                    );
                }
            };
            let decision_id = ulid::Ulid::generate().to_string();
            let display_id = format!("DEC-{}", &decision_id[..8]);

            let record = Decision {
                id: decision_id,
                display_id,
                project_id: project_id.to_string(),
                task_id,
                title: title.clone(),
                context: context.clone(),
                decision: decision.clone(),
                consequences: consequences.clone(),
                related_tasks: vec![],
                related_paths: vec![],
                created_by_agent: agent_id,
                created_by_session: ctx.session.clone(),
                superseded_by: None,
                created_at: now.clone(),
                updated_at: now,
            };
            let result = decision_repo.create(&record);
            if let Ok(ref _d) = result {
                let _ = event_repo.append(&NewEvent {
                    id: ulid::Ulid::generate().to_string(),
                    project_id: project_id.to_string(),
                    event_type: "decision.created".into(),
                    actor_agent_id: ctx.agent.clone(),
                    session_id: ctx.session.clone(),
                    task_id: Some(record.task_id.clone()),
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
                    id: ulid::Ulid::generate().to_string(),
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
