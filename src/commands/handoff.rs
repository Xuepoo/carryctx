use crate::*;
use carryctx::application::runtime::InvocationContext;
use carryctx::domain::collaboration::{Handoff, HandoffStatus};
use carryctx::error::{CarryCtxError, ExitCode};
use clap::Parser;

// ── Handoff ──────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub enum HandoffCommand {
    /// Create a new handoff request directed at another agent or role
    Create {
        /// The target agent ULID or role name
        #[arg(long)]
        target: String,
        /// A summary of what needs to be done or why the handoff is occurring
        #[arg(long)]
        summary: Option<String>,
        /// The task ULID associated with this handoff
        #[arg(long)]
        task: Option<String>,
    },
    /// List pending or historical handoffs
    List,
    /// Show details of a specific handoff request
    Show { handoff_ref: String },
    /// Accept an incoming handoff request
    Accept {
        handoff_ref: String,
        /// Automatically claim the associated task upon accepting the handoff
        #[arg(long)]
        claim_task: bool,
    },
    /// Reject an incoming handoff request
    Reject {
        handoff_ref: String,
        /// The reason for rejecting the handoff
        #[arg(long)]
        reason: Option<String>,
    },
    /// Close a handoff request that is no longer relevant
    Close { handoff_ref: String },
}

#[derive(Parser, Debug)]
pub struct HandoffArgs {
    /// Handoff subcommand to execute
    #[command(subcommand)]
    pub command: HandoffCommand,
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: handoff
// ═══════════════════════════════════════════════════════════════════════════

pub fn handle_handoff(
    args: &HandoffArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    if let Some(result) = check_dry_run(ctx, &format!("handoff {:?}", args.command)) {
        return result;
    }
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
            let task_id = match task.clone().or_else(|| ctx.task.clone()) {
                Some(ref t) if !t.is_empty() => match resolve_task_id(project_id, t, conn) {
                    Ok(id) => id,
                    Err(e) => {
                        return render_and_print::<serde_json::Value>(
                            "handoff.create",
                            Err(e),
                            is_json,
                            ctx.quiet,
                        );
                    }
                },
                _ => {
                    return render_and_print::<serde_json::Value>(
                        "handoff.create",
                        Err(CarryCtxError::validation_error(
                            "No task specified. Provide --task <TASK_REF> for the handoff.",
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
                        "handoff.create",
                        Err(CarryCtxError::validation_error(
                            "No agent specified. Set CARRYCTX_AGENT or use --agent <AGENT>.",
                        )),
                        is_json,
                        ctx.quiet,
                    );
                }
            };
            let handoff_id = ulid::Ulid::generate().to_string();
            let display_id = format!("HO-{}", &handoff_id[..8]);

            let record = Handoff {
                id: handoff_id,
                display_id,
                project_id: project_id.to_string(),
                task_id,
                source_agent_id: agent_id,
                source_session_id: ctx.session.clone(),
                target_agent_id: Some(target.clone()),
                summary: summary.clone(),
                completed_work: vec![],
                remaining_work: vec![],
                blockers: vec![],
                risks: vec![],
                next_steps: vec![],
                changed_files: vec![],
                head: runtime.git_project.head.clone(),
                branch: runtime.git_project.branch.clone(),
                status: HandoffStatus::Open,
                created_at: now.clone(),
                updated_at: now,
            };
            let result = handoff_repo.create(&record);
            if let Ok(ref _h) = result {
                let _ = event_repo.append(&NewEvent {
                    id: ulid::Ulid::generate().to_string(),
                    project_id: project_id.to_string(),
                    event_type: "handoff.created".into(),
                    actor_agent_id: ctx.agent.clone(),
                    session_id: ctx.session.clone(),
                    task_id: Some(record.task_id.clone()),
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
            let item = handoff_repo
                .find_by_display_id(project_id, handoff_ref)
                .map_err(|e| e.exit_code)?
                .or_else(|| {
                    handoff_repo
                        .find_by_id(project_id, handoff_ref)
                        .ok()
                        .flatten()
                })
                .ok_or(ExitCode::ResourceNotFound)?;
            render_and_print("handoff.show", Ok(item), is_json, ctx.quiet)
        }
        HandoffCommand::Accept {
            handoff_ref,
            claim_task: _,
        } => {
            let handoff = handoff_repo
                .find_by_display_id(project_id, handoff_ref)
                .map_err(|e| e.exit_code)?
                .or_else(|| {
                    handoff_repo
                        .find_by_id(project_id, handoff_ref)
                        .ok()
                        .flatten()
                })
                .ok_or(ExitCode::ResourceNotFound)?;
            handoff_repo
                .update_status(&handoff.id, project_id, HandoffStatus::Accepted, &now)
                .map_err(|e| e.exit_code)?;
            let _ = event_repo.append(&NewEvent {
                id: ulid::Ulid::generate().to_string(),
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
        HandoffCommand::Reject {
            handoff_ref,
            reason: _,
        } => {
            let handoff = handoff_repo
                .find_by_display_id(project_id, handoff_ref)
                .map_err(|e| e.exit_code)?
                .or_else(|| {
                    handoff_repo
                        .find_by_id(project_id, handoff_ref)
                        .ok()
                        .flatten()
                })
                .ok_or(ExitCode::ResourceNotFound)?;
            handoff_repo
                .update_status(&handoff.id, project_id, HandoffStatus::Rejected, &now)
                .map_err(|e| e.exit_code)?;
            let _ = event_repo.append(&NewEvent {
                id: ulid::Ulid::generate().to_string(),
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
        HandoffCommand::Close { handoff_ref } => {
            let handoff = handoff_repo
                .find_by_display_id(project_id, handoff_ref)
                .map_err(|e| e.exit_code)?
                .or_else(|| {
                    handoff_repo
                        .find_by_id(project_id, handoff_ref)
                        .ok()
                        .flatten()
                })
                .ok_or(ExitCode::ResourceNotFound)?;
            handoff_repo
                .update_status(&handoff.id, project_id, HandoffStatus::Closed, &now)
                .map_err(|e| e.exit_code)?;
            render_and_print("handoff.close", Ok(handoff), is_json, ctx.quiet)
        }
    }
}
