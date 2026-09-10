use super::{check_dry_run_envelope, print_markdown_result, subcommand_label, truncate_chars};
use crate::adapter::sqlite_repos::{
    SqliteCheckpointRepository, SqliteEventRepository, SqliteSessionRepository,
    SqliteTaskRepository, SqliteWorktreeRepository,
};
use crate::application;
use crate::application::runtime::{InvocationContext, ProjectRuntime};
use crate::cli::{
    open_runtime_or_report, render_and_print, render_and_print_entity,
    render_and_print_entity_with_warnings, resolve_agent_id, resolve_task_id,
};
use crate::error::{CarryCtxError, ExitCode};
use crate::repository::{CheckpointRepository, SessionRepository};
use clap::Parser;
use std::io::{self, IsTerminal, Write};

// ── Session ──────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub enum SessionCommand {
    /// Initialize and start a new agent session, binding it to the current context
    Start {
        /// Override the agent ULID creating this session
        #[arg(long)]
        agent: Option<String>,
        /// Bind the session explicitly to a task ULID
        #[arg(long)]
        task: Option<String>,
        /// Specify the LLM provider for telemetry
        #[arg(long)]
        provider: Option<String>,
        /// Bind to a specific worktree directory
        #[arg(long)]
        worktree: Option<String>,
        /// Re-use the currently active session if one exists, rather than erroring
        #[arg(long)]
        reuse: bool,
    },
    /// List historical and active sessions
    List,
    /// Show metadata and transition history for a specific session
    Show { session_id: String },
    /// Print the currently active session ID
    Current,
    /// Pause the active session, logging a sleep/pause transition
    Pause { session_id: Option<String> },
    /// Resume a previously paused session, logging an awake/resume transition
    Resume { session_id: Option<String> },
    /// End the active session cleanly, marking it as terminated
    End {
        session_id: Option<String>,
        /// A brief summary of what was accomplished during the session
        #[arg(long)]
        summary: Option<String>,
    },
    /// Forcibly abandon a session without recording a clean end state
    Abandon {
        session_id: Option<String>,
        /// The reason the session was abandoned (e.g., crash, fatal error)
        #[arg(long)]
        reason: Option<String>,
    },
}

#[derive(Parser, Debug)]
pub struct SessionArgs {
    /// Session subcommand to execute
    #[command(subcommand)]
    pub command: SessionCommand,
}

fn find_active_session_id(
    session_repo: &SqliteSessionRepository,
    project_id: &str,
) -> Option<String> {
    session_repo
        .list(project_id)
        .ok()?
        .into_iter()
        .find(|s| matches!(s.state, crate::domain::session::SessionState::Active))
        .map(|s| s.id)
}

fn find_paused_session_id(
    session_repo: &SqliteSessionRepository,
    project_id: &str,
) -> Option<String> {
    session_repo
        .list(project_id)
        .ok()?
        .into_iter()
        .find(|s| matches!(s.state, crate::domain::session::SessionState::Paused))
        .map(|s| s.id)
}

fn resolve_session_id(
    session_id: &Option<String>,
    session_repo: &SqliteSessionRepository,
    project_id: &str,
    conn: &rusqlite::Connection,
) -> Result<Option<String>, CarryCtxError> {
    match session_id {
        Some(reference) => crate::cli::resolve_session_ref(project_id, reference, conn).map(Some),
        None => Ok(find_active_session_id(session_repo, project_id)),
    }
}

/// Component-wise containment check: `cwd` is inside (or equal to) `base`.
///
/// Mirrors `application::runtime`'s worktree matcher: unlike
/// `str::starts_with`, `Path::starts_with` compares whole path components,
/// so `/repo/wt-x` does NOT match base `/repo/wt` while `/repo/wt/sub`
/// does. An empty or relative base never matches anything.
fn cwd_within_worktree(cwd: &str, worktree_path: &str) -> bool {
    if worktree_path.trim().is_empty() {
        return false;
    }
    std::path::Path::new(cwd).starts_with(std::path::Path::new(worktree_path))
}

fn checkpoint_prompt_eligible(
    ctx: &InvocationContext,
    is_json: bool,
    stdin_is_terminal: bool,
    stdout_is_terminal: bool,
) -> bool {
    ctx.interactive && !is_json && !ctx.yes && stdin_is_terminal && stdout_is_terminal
}

fn checkpoint_confirmation_eligible(
    ctx: &InvocationContext,
    is_json: bool,
    stdin_is_terminal: bool,
    stdout_is_terminal: bool,
) -> bool {
    !is_json && ctx.yes && stdin_is_terminal && stdout_is_terminal
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: session
// ═══════════════════════════════════════════════════════════════════════════

pub fn handle_session(
    args: &SessionArgs,
    pre_opened: Option<ProjectRuntime>,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    if let Some(result) = check_dry_run_envelope(
        ctx,
        &subcommand_label("session", &args.command),
        &format!("session {:?}", args.command),
    ) {
        return result;
    }
    // Reuse the dispatcher's pre-opened runtime when available; a second
    // open only happens (and reports) when that failed.
    let mut runtime = match pre_opened {
        Some(runtime) => runtime,
        None => open_runtime_or_report(ctx, "session")?,
    };
    let project_id = &runtime.config.project.id;
    let conn = runtime.database.connection_mut();
    let verbose = ctx.verbose || runtime.config.output.verbose;

    let now = chrono::Utc::now().to_rfc3339();

    match &args.command {
        SessionCommand::Start {
            agent,
            task,
            provider,
            worktree,
            reuse,
        } => {
            let agent_candidate = agent
                .clone()
                .or_else(|| ctx.agent.clone())
                // Issue #105: honor the configured `[agent] default_name`
                // instead of hardcoding "default"; the literal stays as the
                // last-resort fallback, matching the auto-register resolver.
                .unwrap_or_else(|| {
                    runtime
                        .config
                        .agent
                        .default_name
                        .clone()
                        .filter(|name| !name.trim().is_empty())
                        .unwrap_or_else(|| "default".to_string())
                });
            let agent_id = match resolve_agent_id(project_id, &agent_candidate, conn) {
                Ok(id) => id,
                Err(e) => {
                    return render_and_print_entity(
                        "session.start",
                        Err::<serde_json::Value, _>(e),
                        is_json,
                        ctx.quiet,
                        verbose,
                        ctx.fields.as_deref(),
                        Some(&runtime.config.output.fields),
                    );
                }
            };

            // Honor documented `--reuse`: return the existing active session
            // for this agent (same worktree scope the supersede check uses)
            // instead of ending it and creating a fresh one. With no active
            // session this falls through to normal creation.
            if *reuse {
                let session_repo = SqliteSessionRepository::new(conn);
                let active = crate::repository::session::SessionRepository::find_active(
                    &session_repo,
                    project_id,
                    &agent_id,
                    worktree.as_deref(),
                );
                if let Ok(Some(existing)) = active.map(|sessions| sessions.into_iter().next()) {
                    return render_and_print_entity(
                        "session.start",
                        Ok(existing),
                        is_json,
                        ctx.quiet,
                        verbose,
                        ctx.fields.as_deref(),
                        Some(&runtime.config.output.fields),
                    );
                }
            }

            let task_id = match task.clone().or_else(|| ctx.task.clone()) {
                Some(t_ref) if !t_ref.is_empty() => {
                    match resolve_task_id(project_id, &t_ref, conn) {
                        Ok(id) => Some(id),
                        Err(e) => {
                            return render_and_print_entity(
                                "session.start",
                                Err::<serde_json::Value, _>(e),
                                is_json,
                                ctx.quiet,
                                verbose,
                                ctx.fields.as_deref(),
                                Some(&runtime.config.output.fields),
                            );
                        }
                    }
                }
                _ => {
                    let mut inferred = None;
                    // 1. Try to infer from current worktree path
                    let worktree_repo = SqliteWorktreeRepository::new(conn);
                    if let Ok(wts) = crate::repository::worktree::WorktreeRepository::list(
                        &worktree_repo,
                        project_id,
                    ) {
                        let current_path = ctx.cwd.to_string_lossy();
                        if let Some(wt) = wts
                            .into_iter()
                            .find(|w| cwd_within_worktree(&current_path, &w.path))
                        {
                            inferred = wt.task_id.clone();
                        }
                    }
                    // 2. Try to infer from agent's single active task
                    if inferred.is_none() {
                        let task_repo = SqliteTaskRepository::new(conn);
                        let filter = crate::repository::task::TaskFilter {
                            project_id: project_id.to_string(),
                            status: Some(crate::domain::task::TaskStatus::InProgress),
                            owner_agent_id: Some(agent_id.clone()),
                            ready: false,
                            blocked: false,
                            mine: None,
                        };
                        if let Ok(mut tasks) =
                            crate::repository::task::TaskRepository::list(&task_repo, &filter)
                        {
                            if tasks.len() == 1 {
                                inferred = Some(tasks.pop().unwrap().id);
                            }
                        }
                    }
                    inferred
                }
            };

            let input = application::session::StartSessionInput {
                project_id: project_id.to_string(),
                agent_id,
                task_id,
                worktree_id: worktree.clone(),
                branch: runtime.git_project.branch.clone(),
                head: runtime.git_project.head.clone(),
                cwd: Some(ctx.cwd.to_string_lossy().to_string()),
                provider: provider.clone(),
            };
            let uow =
                crate::adapter::unit_of_work::UnitOfWork::begin(conn).map_err(|e| e.exit_code)?;
            let session_repo = SqliteSessionRepository::new(uow.connection());
            let event_repo = SqliteEventRepository::new(uow.connection());
            let result =
                application::session::start_session(&session_repo, &event_repo, &input, &now)
                    .and_then(|session| uow.commit().map(|_| session));
            render_and_print_entity(
                "session.start",
                result,
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        SessionCommand::List => {
            let session_repo = SqliteSessionRepository::new(conn);
            let result = application::session::list_sessions(&session_repo, project_id);

            // Markdown format support
            if ctx.format == crate::application::runtime::OutputFormat::Markdown {
                return print_markdown_result(
                    "session.list",
                    result,
                    |sessions| {
                        let mut out = String::from("# Sessions\n\n");
                        out.push_str("| ID | Agent | State | Branch | Created |\n");
                        out.push_str("|---|---|---|---|---|\n");
                        for s in sessions {
                            let id_short = truncate_chars(&s.id, 8);
                            let agent_short = truncate_chars(&s.agent_id, 8);
                            out.push_str(&format!(
                                "| {} | {} | {:?} | {} | {} |\n",
                                id_short,
                                agent_short,
                                s.state,
                                s.branch.as_deref().unwrap_or("-"),
                                truncate_chars(&s.created_at, 19)
                            ));
                        }
                        out
                    },
                    ctx,
                );
            }

            render_and_print_entity(
                "session.list",
                result,
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        SessionCommand::Show { session_id } => {
            let session_repo = SqliteSessionRepository::new(conn);
            let result = crate::cli::resolve_session_ref(project_id, session_id, conn)
                .and_then(|id| application::session::show_session(&session_repo, project_id, &id));
            render_and_print_entity(
                "session.show",
                result,
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        SessionCommand::Current => {
            let session_repo = SqliteSessionRepository::new(conn);
            let sessions = session_repo.list(project_id).map_err(|e| e.exit_code)?;
            let current = sessions
                .into_iter()
                .find(|s| matches!(s.state, crate::domain::session::SessionState::Active));
            render_and_print_entity(
                "session.current",
                current.ok_or_else(|| CarryCtxError::resource_not_found("No active session")),
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        SessionCommand::Pause { session_id } => {
            let session_repo = SqliteSessionRepository::new(conn);
            let event_repo = SqliteEventRepository::new(conn);
            let sid = match resolve_session_id(session_id, &session_repo, project_id, conn) {
                Ok(Some(id)) => id,
                Ok(None) => {
                    return render_and_print::<serde_json::Value>(
                        "session.pause",
                        Err(CarryCtxError::resource_not_found(
                            "No active session found. Start a session first.",
                        )),
                        is_json,
                        ctx.quiet,
                    );
                }
                Err(error) => {
                    return render_and_print::<serde_json::Value>(
                        "session.pause",
                        Err(error),
                        is_json,
                        ctx.quiet,
                    );
                }
            };
            let agent_id = match ctx.agent.clone() {
                Some(id) => id,
                None => {
                    return render_and_print::<serde_json::Value>(
                        "session.pause",
                        Err(CarryCtxError::validation_error(
                            "No agent specified. Set CARRYCTX_AGENT or use --agent <AGENT>.",
                        )),
                        is_json,
                        ctx.quiet,
                    );
                }
            };
            let input = application::session::PauseSessionInput {
                project_id: project_id.to_string(),
                session_id: sid.clone(),
                agent_id,
            };
            let result =
                application::session::pause_session(&session_repo, &event_repo, &input, &now);
            render_and_print_entity(
                "session.pause",
                result,
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        SessionCommand::Resume { session_id } => {
            let session_repo = SqliteSessionRepository::new(conn);
            let event_repo = SqliteEventRepository::new(conn);
            let sid = match session_id {
                Some(reference) => {
                    match crate::cli::resolve_session_ref(project_id, reference, conn) {
                        Ok(id) => id,
                        Err(error) => {
                            return render_and_print::<serde_json::Value>(
                                "session.resume",
                                Err(error),
                                is_json,
                                ctx.quiet,
                            );
                        }
                    }
                }
                None => match find_paused_session_id(&session_repo, project_id) {
                    Some(id) => id,
                    None => {
                        return render_and_print::<serde_json::Value>(
                            "session.resume",
                            Err(CarryCtxError::resource_not_found(
                                "No paused session found.",
                            )),
                            is_json,
                            ctx.quiet,
                        );
                    }
                },
            };
            let agent_id = match ctx.agent.clone() {
                Some(id) => id,
                None => {
                    return render_and_print::<serde_json::Value>(
                        "session.resume",
                        Err(CarryCtxError::validation_error(
                            "No agent specified. Set CARRYCTX_AGENT or use --agent <AGENT>.",
                        )),
                        is_json,
                        ctx.quiet,
                    );
                }
            };
            let input = application::session::ResumeSessionInput {
                project_id: project_id.to_string(),
                session_id: sid.clone(),
                agent_id,
            };
            let result =
                application::session::resume_session(&session_repo, &event_repo, &input, &now);
            render_and_print_entity(
                "session.resume",
                result,
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        SessionCommand::End {
            session_id,
            summary,
        } => {
            let sid = match resolve_session_id(
                session_id,
                &SqliteSessionRepository::new(conn),
                project_id,
                conn,
            ) {
                Ok(Some(id)) => id,
                Ok(None) => {
                    return render_and_print::<serde_json::Value>(
                        "session.end",
                        Err(CarryCtxError::resource_not_found(
                            "No active session found. Start a session first.",
                        )),
                        is_json,
                        ctx.quiet,
                    );
                }
                Err(error) => {
                    return render_and_print::<serde_json::Value>(
                        "session.end",
                        Err(error),
                        is_json,
                        ctx.quiet,
                    );
                }
            };
            let agent_id = match ctx.agent.clone() {
                Some(id) => id,
                None => {
                    return render_and_print::<serde_json::Value>(
                        "session.end",
                        Err(CarryCtxError::validation_error(
                            "No agent specified. Set CARRYCTX_AGENT or use --agent <AGENT>.",
                        )),
                        is_json,
                        ctx.quiet,
                    );
                }
            };
            let input = application::session::EndSessionInput {
                project_id: project_id.to_string(),
                session_id: sid.clone(),
                agent_id,
                summary: summary.clone(),
            };
            let session = SqliteSessionRepository::new(conn).find_by_id(project_id, &sid);
            let session = match session {
                Ok(Some(session)) => session,
                Ok(None) => unreachable!("session was resolved immediately before lookup"),
                Err(error) => {
                    return render_and_print::<serde_json::Value>(
                        "session.end",
                        Err(error),
                        is_json,
                        ctx.quiet,
                    );
                }
            };
            let mut warnings = Vec::new();
            if runtime.config.checkpoint.require_before_session_end {
                let has_checkpoint = match session.task_id.as_deref() {
                    Some(task_id) => match SqliteCheckpointRepository::new(conn)
                        .find_latest_for_task(project_id, task_id)
                    {
                        Ok(checkpoint) => checkpoint.is_some(),
                        Err(error) => {
                            return render_and_print_entity_with_warnings(
                                "session.end",
                                Err::<serde_json::Value, _>(
                                    error.with_context("Checkpoint verification failed"),
                                ),
                                is_json,
                                ctx.quiet,
                                verbose,
                                warnings,
                                ctx.fields.as_deref(),
                                Some(&runtime.config.output.fields),
                            );
                        }
                    },
                    None => true,
                };
                if !has_checkpoint {
                    let message =
                        "No checkpoint exists for this session. Create one before ending?";
                    let can_prompt = checkpoint_prompt_eligible(
                        ctx,
                        is_json,
                        io::stdin().is_terminal(),
                        io::stdout().is_terminal(),
                    );
                    let can_confirm = checkpoint_confirmation_eligible(
                        ctx,
                        is_json,
                        io::stdin().is_terminal(),
                        io::stdout().is_terminal(),
                    );
                    if can_prompt || can_confirm {
                        if can_confirm {
                            // --yes explicitly confirms only for a text TTY.
                        }
                        if !can_prompt {
                            // --yes has already supplied confirmation.
                        }
                    } else {
                        return render_and_print_entity_with_warnings(
                            "session.end",
                            Err::<serde_json::Value, _>(CarryCtxError::validation_error(
                                "A checkpoint is required before ending this session.",
                            )),
                            is_json,
                            ctx.quiet,
                            verbose,
                            warnings,
                            ctx.fields.as_deref(),
                            Some(&runtime.config.output.fields),
                        );
                    }
                    if can_prompt {
                        print!("{message} [y/N] ");
                        let _ = io::stdout().flush();
                        let mut answer = String::new();
                        let _ = io::stdin().read_line(&mut answer);
                        if !answer.trim().eq_ignore_ascii_case("y") {
                            return render_and_print::<serde_json::Value>(
                                "session.end",
                                Err(CarryCtxError::state_conflict(
                                    "Session end cancelled: create a checkpoint first.",
                                )),
                                is_json,
                                ctx.quiet,
                            );
                        }
                    }
                }
            }
            let uow = match crate::adapter::unit_of_work::UnitOfWork::begin(conn) {
                Ok(uow) => uow,
                Err(error) => {
                    return render_and_print::<serde_json::Value>(
                        "session.end",
                        Err(error),
                        is_json,
                        ctx.quiet,
                    );
                }
            };
            let result = application::session::end_session(&input, &now, &uow)
                .and_then(|ended| uow.commit().map(|_| ended));
            if result.is_ok() {
                match ctx.admission_lock.as_deref() {
                    Some(lock) => {
                        match application::cleanup::reconcile_cleanup_for_session_with_policy(
                            conn,
                            project_id,
                            session.task_id.as_deref(),
                            session.worktree_id.as_deref(),
                            &runtime.git_project.repository_root,
                            ctx.agent.as_deref(),
                            lock,
                            &runtime.config.worktree.cleanup,
                            &runtime.config.session,
                        ) {
                            Ok(cleanup_warnings) => warnings.extend(cleanup_warnings),
                            Err(error) => warnings.push(format!(
                                "Cleanup reconciliation deferred: {}",
                                error.message
                            )),
                        }
                    }
                    None => warnings.push(
                        "Cleanup reconciliation deferred: project admission lock unavailable."
                            .into(),
                    ),
                }
            }
            render_and_print_entity_with_warnings(
                "session.end",
                result,
                is_json,
                ctx.quiet,
                verbose,
                warnings,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        SessionCommand::Abandon { session_id, reason } => {
            let session_repo = SqliteSessionRepository::new(conn);
            let event_repo = SqliteEventRepository::new(conn);
            let sid = match resolve_session_id(session_id, &session_repo, project_id, conn) {
                Ok(Some(id)) => id,
                Ok(None) => {
                    return render_and_print::<serde_json::Value>(
                        "session.abandon",
                        Err(CarryCtxError::resource_not_found(
                            "No active session found. Start a session first.",
                        )),
                        is_json,
                        ctx.quiet,
                    );
                }
                Err(error) => {
                    return render_and_print::<serde_json::Value>(
                        "session.abandon",
                        Err(error),
                        is_json,
                        ctx.quiet,
                    );
                }
            };
            let agent_id = match ctx.agent.clone() {
                Some(id) => id,
                None => {
                    return render_and_print::<serde_json::Value>(
                        "session.abandon",
                        Err(CarryCtxError::validation_error(
                            "No agent specified. Set CARRYCTX_AGENT or use --agent <AGENT>.",
                        )),
                        is_json,
                        ctx.quiet,
                    );
                }
            };
            // A dedicated abandon path (not end_session): the session must land
            // in the distinct `abandoned` state and the reason must reach the
            // audit event payload instead of being discarded.
            let input = application::session::AbandonSessionInput {
                project_id: project_id.to_string(),
                session_id: sid,
                agent_id,
                reason: reason.clone(),
            };
            let result =
                application::session::abandon_session(&session_repo, &event_repo, &input, &now);
            render_and_print_entity(
                "session.abandon",
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

#[cfg(test)]
mod worktree_path_tests {
    use super::cwd_within_worktree;

    #[test]
    fn matches_exact_and_nested_paths() {
        assert!(cwd_within_worktree("/repo/wt", "/repo/wt"));
        assert!(cwd_within_worktree("/repo/wt/sub/dir", "/repo/wt"));
    }

    #[test]
    fn rejects_prefix_collisions_without_component_boundary() {
        // The old `str::starts_with` inference matched /repo/foo for the
        // /repo/f worktree, binding sessions to the wrong task.
        assert!(!cwd_within_worktree("/repo/foo", "/repo/f"));
        assert!(!cwd_within_worktree("/repo/wt-x", "/repo/wt"));
    }

    #[test]
    fn rejects_empty_or_relative_bases() {
        assert!(!cwd_within_worktree("/repo/wt", ""));
        assert!(!cwd_within_worktree("/repo/wt", "   "));
        assert!(!cwd_within_worktree("/repo/wt", "repo/wt"));
    }
}

#[cfg(test)]
mod checkpoint_prompt_tests {
    use super::{InvocationContext, checkpoint_confirmation_eligible, checkpoint_prompt_eligible};

    #[test]
    fn json_mode_never_prompts_even_when_both_streams_are_terminals() {
        let ctx = InvocationContext {
            interactive: true,
            ..Default::default()
        };

        assert!(!checkpoint_prompt_eligible(&ctx, true, true, true));
        assert!(checkpoint_prompt_eligible(&ctx, false, true, true));
        assert!(!checkpoint_prompt_eligible(&ctx, false, false, true));
        assert!(!checkpoint_confirmation_eligible(&ctx, false, false, true));
    }
}
