use super::{check_dry_run_envelope, resolve_or_render, subcommand_label};
use crate::adapter::sqlite_repos::{
    SqliteAgentRepository, SqliteEventRepository, SqliteHandoffRepository,
};
use crate::application::collaboration::{CreateHandoffInput, create_handoff};
use crate::application::runtime::{InvocationContext, ProjectRuntime};
use crate::cli::{open_runtime_or_report, render_and_print, render_and_print_entity};
use crate::domain::collaboration::HandoffStatus;
use crate::error::{CarryCtxError, ExitCode};
use crate::repository::HandoffRepository;
use crate::repository::event::NewEvent;
use crate::repository::{AgentFilter, AgentRepository};
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
    /// List handoffs. Shows only actionable (pending) requests by default; pass
    /// --status or --all to widen.
    List {
        /// Filter by exact status: pending, accepted, declined, closed
        #[arg(long)]
        status: Option<String>,
        /// Show every handoff regardless of status, including resolved ones
        #[arg(long)]
        all: bool,
        /// Only handoffs routed to this agent (name, ULID, or role)
        #[arg(long)]
        for_agent: Option<String>,
    },
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

/// Parse a `--status` value into a `HandoffStatus`.
///
/// Accepts the on-the-wire SQL spellings (`pending`, `declined`) as well as the
/// domain names (`open`, `rejected`), because the two diverge: `HandoffStatus::Open`
/// persists as `"pending"` and `Rejected` as `"declined"`, and a user reading either
/// the JSON output or the schema should be able to pass what they saw.
fn parse_handoff_status(value: &str) -> Result<HandoffStatus, CarryCtxError> {
    match value.to_ascii_lowercase().as_str() {
        "pending" | "open" => Ok(HandoffStatus::Open),
        "accepted" => Ok(HandoffStatus::Accepted),
        "declined" | "rejected" => Ok(HandoffStatus::Rejected),
        "closed" => Ok(HandoffStatus::Closed),
        other => Err(CarryCtxError::validation_error(format!(
            "Unknown handoff status '{other}'. Expected one of: pending (open), accepted, declined (rejected), closed."
        ))),
    }
}

/// Truncate to at most `max_chars` characters on char boundaries, appending an
/// ellipsis only when something was cut. Byte slicing (`&s[..40]`) panics when
/// the offset lands inside a multibyte character.
fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let cut: String = s.chars().take(max_chars).collect();
    format!("{cut}...")
}

/// Resolve a handoff by display ID, then internal id.
///
/// Database errors from either lookup propagate (the previous copy-pasted
/// blocks swallowed `find_by_id` failures with `.ok().flatten()`, rendering
/// DB errors as "not found"); only a genuinely unknown reference yields the
/// not-found error.
fn require_handoff(
    repo: &SqliteHandoffRepository<'_>,
    project_id: &str,
    handoff_ref: &str,
) -> Result<crate::domain::collaboration::Handoff, CarryCtxError> {
    let found = if let Some(item) = repo.find_by_display_id(project_id, handoff_ref)? {
        Some(item)
    } else {
        repo.find_by_id(project_id, handoff_ref)?
    };
    found.ok_or_else(|| {
        CarryCtxError::resource_not_found(format!("Handoff '{handoff_ref}' not found."))
    })
}

/// Append the user-supplied rejection rationale as a `handoff.rejection_recorded`
/// audit event.
///
/// Must be called on the same [`crate::adapter::unit_of_work::UnitOfWork`]
/// that performed the rejection so the reason is committed (or rolled back)
/// atomically with the state change it explains — the handoffs table has no
/// dedicated reason column, and adding one would require a migration.
fn append_rejection_reason_event(
    uow: &crate::adapter::unit_of_work::UnitOfWork,
    project_id: &str,
    handoff: &crate::domain::collaboration::Handoff,
    actor_agent_id: Option<&str>,
    session_id: Option<&str>,
    reason: &str,
) -> Result<(), CarryCtxError> {
    let event_repo = SqliteEventRepository::new(uow.connection());
    crate::repository::event::EventRepository::append(
        &event_repo,
        &NewEvent {
            id: ulid::Ulid::generate().to_string(),
            project_id: project_id.to_string(),
            event_type: "handoff.rejection_recorded".into(),
            actor_agent_id: actor_agent_id.map(str::to_string),
            session_id: session_id.map(str::to_string),
            task_id: Some(handoff.task_id.clone()),
            payload: serde_json::json!({
                "handoffId": handoff.id,
                "displayId": handoff.display_id,
                "reason": reason,
            }),
            occurred_at: chrono::Utc::now().to_rfc3339(),
        },
    )
    .map(|_| ())
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: handoff
// ═══════════════════════════════════════════════════════════════════════════

pub fn handle_handoff(
    args: &HandoffArgs,
    pre_opened: Option<ProjectRuntime>,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    if let Some(result) = check_dry_run_envelope(
        ctx,
        &subcommand_label("handoff", &args.command),
        &format!("handoff {:?}", args.command),
    ) {
        return result;
    }
    // Reuse the dispatcher's pre-opened runtime when available; a second
    // open only happens (and reports) when that failed.
    let mut runtime = match pre_opened {
        Some(runtime) => runtime,
        None => open_runtime_or_report(ctx, "handoff")?,
    };
    let project_id = &runtime.config.project.id;
    let conn = runtime.database.connection_mut();
    let verbose = ctx.verbose || runtime.config.output.verbose;

    let uow = crate::adapter::unit_of_work::UnitOfWork::begin(conn).map_err(|e| e.exit_code)?;

    let handoff_repo = SqliteHandoffRepository::new(uow.connection());

    match &args.command {
        HandoffCommand::Create {
            target,
            summary,
            task,
        } => {
            let resolver =
                crate::application::runtime::CurrentEntityResolver::new(project_id, &uow);

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
                        "handoff.create",
                        Err(e),
                        is_json,
                        ctx.quiet,
                    );
                }
            };
            let agent_id = agent.id;

            // Resolve --target to a registered agent ULID. Accepts a name, a
            // ULID, or a role name; previously the raw value was inserted into
            // to_agent_id, failing the agents(id) FK for anything but a ULID.
            let target_agent_id = (|| -> Result<String, CarryCtxError> {
                let agent_repo = SqliteAgentRepository::new(uow.connection());
                let filter = AgentFilter {
                    project_id: project_id.to_string(),
                    status: None,
                };
                agent_repo
                    .find_by_name(project_id, target)?
                    .or(agent_repo.find_by_id(project_id, target)?)
                    .or_else(|| {
                        agent_repo
                            .list(&filter)
                            .ok()?
                            .into_iter()
                            .find(|a| a.role.as_deref() == Some(target.as_str()))
                    })
                    .map(|a| a.id)
                    .ok_or_else(|| {
                        CarryCtxError::resource_not_found(format!(
                            "Target agent '{target}' not found. Register it with `carryctx agent register` first, or pass a registered agent name/ULID/role."
                        ))
                    })
            })();
            let target_agent_id = match target_agent_id {
                Ok(id) => id,
                Err(e) => {
                    return render_and_print::<serde_json::Value>(
                        "handoff.create",
                        Err(e),
                        is_json,
                        ctx.quiet,
                    );
                }
            };

            let task_id = match resolver.resolve_task(
                task.as_deref().or(ctx.task.as_deref()),
                Some(&ctx.cwd.to_string_lossy()),
                Some(&agent_id),
            ) {
                Ok(Some(t)) => t.id,
                Ok(None) => {
                    return render_and_print::<serde_json::Value>(
                        "handoff.create",
                        Err(CarryCtxError::validation_error(
                            "No task specified. Provide --task <TASK_REF> for the handoff.",
                        )),
                        is_json,
                        ctx.quiet,
                    );
                }
                Err(e) => {
                    return render_and_print::<serde_json::Value>(
                        "handoff.create",
                        Err(e),
                        is_json,
                        ctx.quiet,
                    );
                }
            };
            // Delegate to the application layer so the display id comes from the
            // `sequences` counter (HF-0001, HF-0002, …). Generating it here from
            // a ULID prefix collided for two handoffs created in the same
            // millisecond, and skipped the `handoff.created` event the
            // application layer appends.
            let input = CreateHandoffInput {
                task_id,
                source_agent_id: agent_id,
                source_session_id: ctx.session.clone(),
                target_agent_id: Some(target_agent_id),
                summary: summary.clone(),
                completed_work: vec![],
                remaining_work: vec![],
                blockers: vec![],
                risks: vec![],
                next_steps: vec![],
                changed_files: vec![],
                head: runtime.git_project.head.clone(),
                branch: runtime.git_project.branch.clone(),
            };
            let result = create_handoff(project_id, &input, &uow);
            if result.is_ok() {
                uow.commit().map_err(|e| {
                    crate::error::CarryCtxError::database_error(e.to_string()).exit_code
                })?;
            }
            render_and_print_entity(
                "handoff.create",
                result,
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        HandoffCommand::List {
            status,
            all,
            for_agent,
        } => {
            // Default to pending only: a list dominated by resolved handoffs is
            // what made `handoff list` unusable as a session-start check. An
            // explicit --status wins over the default; --all drops the filter.
            let status_filter = match (all, status.as_deref()) {
                (true, _) => None,
                (false, Some(s)) => match parse_handoff_status(s) {
                    Ok(parsed) => Some(parsed),
                    Err(e) => {
                        return render_and_print::<serde_json::Value>(
                            "handoff.list",
                            Err(e),
                            is_json,
                            ctx.quiet,
                        );
                    }
                },
                (false, None) => Some(HandoffStatus::Open),
            };

            // Resolved the same way as --target on create, so the same spellings
            // (name, ULID, role) work on both sides of a handoff.
            let target_filter = match for_agent {
                None => None,
                Some(want) => {
                    let agent_repo = SqliteAgentRepository::new(uow.connection());
                    let filter = AgentFilter {
                        project_id: project_id.to_string(),
                        status: None,
                    };
                    let resolved = agent_repo
                        .find_by_name(project_id, want)
                        .ok()
                        .flatten()
                        .or_else(|| agent_repo.find_by_id(project_id, want).ok().flatten())
                        .or_else(|| {
                            agent_repo
                                .list(&filter)
                                .ok()?
                                .into_iter()
                                .find(|a| a.role.as_deref() == Some(want.as_str()))
                        })
                        .map(|a| a.id);
                    match resolved {
                        Some(id) => Some(id),
                        None => {
                            return render_and_print::<serde_json::Value>(
                                "handoff.list",
                                Err(CarryCtxError::resource_not_found(format!(
                                    "Agent '{want}' not found. Pass a registered agent name, ULID, or role."
                                ))),
                                is_json,
                                ctx.quiet,
                            );
                        }
                    }
                }
            };

            let result = handoff_repo.list(&crate::repository::HandoffFilter {
                project_id: project_id.to_string(),
                status: status_filter,
                target_agent_id: target_filter,
            });

            // Markdown format support
            if ctx.format == crate::application::runtime::OutputFormat::Markdown {
                // Errors must follow the standard envelope contract (stderr +
                // non-zero exit), not a printed "Error:" line.
                let handoffs = match result {
                    Ok(handoffs) => handoffs,
                    Err(e) => {
                        return render_and_print::<serde_json::Value>(
                            "handoff.list",
                            Err(e),
                            is_json,
                            ctx.quiet,
                        );
                    }
                };
                let mut out = String::from("# Handoffs\n\n");
                out.push_str("| ID | Summary | Status | Created |\n");
                out.push_str("|---|---|---|---|\n");
                for h in handoffs {
                    let summary = h.summary.as_deref().unwrap_or("");
                    let s_short = truncate_chars(summary, 40);
                    out.push_str(&format!(
                        "| {} | {} | {:?} | {} |\n",
                        h.display_id,
                        s_short,
                        h.status,
                        &h.created_at[..10]
                    ));
                }
                if !ctx.quiet {
                    print!("{out}");
                }
                return Ok(ExitCode::Success);
            }

            render_and_print_entity(
                "handoff.list",
                result,
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        HandoffCommand::Show { handoff_ref } => {
            let handoff = resolve_or_render(
                "handoff.show",
                require_handoff(&handoff_repo, project_id, handoff_ref),
                ctx,
                is_json,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )?;
            render_and_print_entity(
                "handoff.show",
                Ok(handoff),
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        HandoffCommand::Accept {
            handoff_ref,
            claim_task,
        } => {
            let handoff = resolve_or_render(
                "handoff.accept",
                require_handoff(&handoff_repo, project_id, handoff_ref),
                ctx,
                is_json,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )?;
            if *claim_task {
                let resolver =
                    crate::application::runtime::CurrentEntityResolver::new(project_id, &uow);
                let agent = resolve_or_render(
                    "handoff.accept",
                    resolver.resolve_agent(
                        ctx.agent.as_deref(),
                        None,
                        None,
                        runtime.config.agent.default_name.as_deref(),
                        runtime.config.agent.default_name.as_deref(),
                    ),
                    ctx,
                    is_json,
                    verbose,
                    ctx.fields.as_deref(),
                    Some(&runtime.config.output.fields),
                )?;
                // Claim the associated task for the accepting agent in the same
                // transaction: if the task cannot be claimed (already owned,
                // wrong status, incomplete dependencies), fail the whole accept
                // instead of silently dropping the documented --claim-task
                // behavior.
                resolve_or_render(
                    "handoff.accept",
                    crate::application::task::claim_task(
                        project_id,
                        &handoff.task_id,
                        &agent.id,
                        &uow,
                    ),
                    ctx,
                    is_json,
                    verbose,
                    ctx.fields.as_deref(),
                    Some(&runtime.config.output.fields),
                )?;
            }
            // Route through the guarded application use case so the handoff
            // lifecycle (open -> accepted/rejected -> closed) and the audit
            // event are enforced in the same transaction as the mutation.
            let result = crate::application::collaboration::accept_handoff(
                project_id,
                &handoff.id,
                ctx.agent.as_deref(),
                ctx.session.as_deref(),
                &uow,
            );
            let updated = resolve_or_render(
                "handoff.accept",
                result,
                ctx,
                is_json,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )?;
            uow.commit().map_err(|e| {
                crate::error::CarryCtxError::database_error(e.to_string()).exit_code
            })?;
            render_and_print_entity(
                "handoff.accept",
                Ok(updated),
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        HandoffCommand::Reject {
            handoff_ref,
            reason,
        } => {
            let handoff = resolve_or_render(
                "handoff.reject",
                require_handoff(&handoff_repo, project_id, handoff_ref),
                ctx,
                is_json,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )?;
            let result = crate::application::collaboration::reject_handoff(
                project_id,
                &handoff.id,
                ctx.agent.as_deref(),
                ctx.session.as_deref(),
                &uow,
            );
            let updated = resolve_or_render(
                "handoff.reject",
                result,
                ctx,
                is_json,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )?;
            // The user-supplied rationale must survive the audit trail: append
            // it as a dedicated event inside the SAME transaction as the
            // rejection (the handoffs table has no dedicated reason column,
            // and migrations are out of scope for this change). An absent or
            // blank --reason records nothing.
            let reason = reason.as_deref().map(str::trim).filter(|r| !r.is_empty());
            if let Some(reason) = reason {
                resolve_or_render(
                    "handoff.reject",
                    append_rejection_reason_event(
                        &uow,
                        project_id,
                        &updated,
                        ctx.agent.as_deref(),
                        ctx.session.as_deref(),
                        reason,
                    ),
                    ctx,
                    is_json,
                    verbose,
                    ctx.fields.as_deref(),
                    Some(&runtime.config.output.fields),
                )?;
            }
            uow.commit().map_err(|e| {
                crate::error::CarryCtxError::database_error(e.to_string()).exit_code
            })?;
            render_and_print_entity(
                "handoff.reject",
                Ok(updated),
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        HandoffCommand::Close { handoff_ref } => {
            let handoff = resolve_or_render(
                "handoff.close",
                require_handoff(&handoff_repo, project_id, handoff_ref),
                ctx,
                is_json,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )?;
            let result = crate::application::collaboration::close_handoff(
                project_id,
                &handoff.id,
                ctx.agent.as_deref(),
                ctx.session.as_deref(),
                &uow,
            );
            let updated = resolve_or_render(
                "handoff.close",
                result,
                ctx,
                is_json,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )?;
            uow.commit().map_err(|e| {
                crate::error::CarryCtxError::database_error(e.to_string()).exit_code
            })?;
            render_and_print_entity(
                "handoff.close",
                Ok(updated),
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
    }
}
