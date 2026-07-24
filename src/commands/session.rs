use crate::*;
use carryctx::application;
use carryctx::application::runtime::InvocationContext;
use carryctx::error::{CarryCtxError, ExitCode};
use clap::Parser;

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
        .find(|s| matches!(s.state, carryctx::domain::session::SessionState::Active))
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
        .find(|s| matches!(s.state, carryctx::domain::session::SessionState::Paused))
        .map(|s| s.id)
}

fn resolve_session_id(
    session_id: &Option<String>,
    session_repo: &SqliteSessionRepository,
    project_id: &str,
) -> Option<String> {
    session_id
        .clone()
        .or_else(|| find_active_session_id(session_repo, project_id))
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: session
// ═══════════════════════════════════════════════════════════════════════════

pub fn handle_session(
    args: &SessionArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    if let Some(result) = check_dry_run(ctx, &format!("session {:?}", args.command)) {
        return result;
    }
    let mut runtime = try_open_runtime(ctx)?;
    let project_id = &runtime.config.project.id;
    let conn = runtime.database.connection_mut();

    let session_repo = SqliteSessionRepository::new(conn);
    let event_repo = SqliteEventRepository::new(conn);
    let now = chrono::Utc::now().to_rfc3339();

    match &args.command {
        SessionCommand::Start {
            agent,
            task,
            provider,
            worktree,
            reuse: _,
        } => {
            let agent_candidate = agent
                .clone()
                .or_else(|| ctx.agent.clone())
                .unwrap_or_else(|| "default".to_string());
            let agent_id = match resolve_agent_id(project_id, &agent_candidate, conn) {
                Ok(id) => id,
                Err(e) => {
                    return render_and_print(
                        "session.start",
                        Err::<serde_json::Value, _>(e),
                        is_json,
                        ctx.quiet,
                    );
                }
            };

            let task_id = match task.clone().or_else(|| ctx.task.clone()) {
                Some(t_ref) if !t_ref.is_empty() => {
                    match resolve_task_id(project_id, &t_ref, conn) {
                        Ok(id) => Some(id),
                        Err(e) => {
                            return render_and_print(
                                "session.start",
                                Err::<serde_json::Value, _>(e),
                                is_json,
                                ctx.quiet,
                            );
                        }
                    }
                }
                _ => {
                    let mut inferred = None;
                    // 1. Try to infer from current worktree path
                    let worktree_repo = SqliteWorktreeRepository::new(conn);
                    if let Ok(wts) = carryctx::repository::worktree::WorktreeRepository::list(
                        &worktree_repo,
                        project_id,
                    ) {
                        let current_path = ctx.cwd.to_string_lossy();
                        if let Some(wt) =
                            wts.into_iter().find(|w| current_path.starts_with(&w.path))
                        {
                            inferred = wt.task_id.clone();
                        }
                    }
                    // 2. Try to infer from agent's single active task
                    if inferred.is_none() {
                        let task_repo = SqliteTaskRepository::new(conn);
                        let filter = carryctx::repository::task::TaskFilter {
                            project_id: project_id.to_string(),
                            status: Some(carryctx::domain::task::TaskStatus::InProgress),
                            owner_agent_id: Some(agent_id.clone()),
                            ready: false,
                            blocked: false,
                            mine: None,
                        };
                        if let Ok(mut tasks) =
                            carryctx::repository::task::TaskRepository::list(&task_repo, &filter)
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
            let result =
                application::session::start_session(&session_repo, &event_repo, &input, &now);
            render_and_print("session.start", result, is_json, ctx.quiet)
        }
        SessionCommand::List => {
            let result = application::session::list_sessions(&session_repo, project_id);

            // Markdown format support
            if ctx.format == carryctx::application::runtime::OutputFormat::Markdown {
                let md = match &result {
                    Ok(sessions) => {
                        let mut out = String::from("# Sessions\n\n");
                        out.push_str("| ID | Agent | State | Branch | Created |\n");
                        out.push_str("|---|---|---|---|---|\n");
                        for s in sessions {
                            let id_short = &s.id[..s.id.len().min(8)];
                            let agent_short = &s.agent_id[..s.agent_id.len().min(8)];
                            out.push_str(&format!(
                                "| {} | {} | {:?} | {} | {} |\n",
                                id_short,
                                agent_short,
                                s.state,
                                s.branch.as_deref().unwrap_or("-"),
                                &s.created_at[..19]
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

            render_and_print("session.list", result, is_json, ctx.quiet)
        }
        SessionCommand::Show { session_id } => {
            let result = application::session::show_session(&session_repo, project_id, session_id);
            render_and_print("session.show", result, is_json, ctx.quiet)
        }
        SessionCommand::Current => {
            let sessions = session_repo.list(project_id).map_err(|e| e.exit_code)?;
            let current = sessions
                .into_iter()
                .find(|s| matches!(s.state, carryctx::domain::session::SessionState::Active));
            render_and_print(
                "session.current",
                current.ok_or_else(|| CarryCtxError::resource_not_found("No active session")),
                is_json,
                ctx.quiet,
            )
        }
        SessionCommand::Pause { session_id } => {
            let sid = match resolve_session_id(session_id, &session_repo, project_id) {
                Some(id) => id,
                None => {
                    return render_and_print::<serde_json::Value>(
                        "session.pause",
                        Err(CarryCtxError::resource_not_found(
                            "No active session found. Start a session first.",
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
                session_id: sid,
                agent_id,
            };
            let result =
                application::session::pause_session(&session_repo, &event_repo, &input, &now);
            render_and_print("session.pause", result, is_json, ctx.quiet)
        }
        SessionCommand::Resume { session_id } => {
            let sid = match session_id
                .clone()
                .or_else(|| find_paused_session_id(&session_repo, project_id))
            {
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
                session_id: sid,
                agent_id,
            };
            let result =
                application::session::resume_session(&session_repo, &event_repo, &input, &now);
            render_and_print("session.resume", result, is_json, ctx.quiet)
        }
        SessionCommand::End {
            session_id,
            summary,
        } => {
            let sid = match resolve_session_id(session_id, &session_repo, project_id) {
                Some(id) => id,
                None => {
                    return render_and_print::<serde_json::Value>(
                        "session.end",
                        Err(CarryCtxError::resource_not_found(
                            "No active session found. Start a session first.",
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
                session_id: sid,
                agent_id,
                summary: summary.clone(),
            };
            let result =
                application::session::end_session(&session_repo, &event_repo, &input, &now);
            render_and_print("session.end", result, is_json, ctx.quiet)
        }
        SessionCommand::Abandon {
            session_id,
            reason: _,
        } => {
            let sid = match resolve_session_id(session_id, &session_repo, project_id) {
                Some(id) => id,
                None => {
                    return render_and_print::<serde_json::Value>(
                        "session.abandon",
                        Err(CarryCtxError::resource_not_found(
                            "No active session found. Start a session first.",
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
                        "session.abandon",
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
                session_id: sid,
                agent_id,
                summary: Some("abandoned".into()),
            };
            let result =
                application::session::end_session(&session_repo, &event_repo, &input, &now);
            render_and_print("session.abandon", result, is_json, ctx.quiet)
        }
    }
}
