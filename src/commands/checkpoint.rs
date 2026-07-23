use crate::*;
use carryctx::application;
use carryctx::application::runtime::InvocationContext;
use carryctx::error::ExitCode;
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
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    let mut runtime = try_open_runtime(ctx)?;
    let project_id = &runtime.config.project.id;
    let conn = runtime.database.connection_mut();

    let checkpoint_repo = SqliteCheckpointRepository::new(conn);
    let event_repo = SqliteEventRepository::new(conn);
    let git_cli = GitCli::new();

    match &args.command {
        Some(CheckpointCommand::List) => {
            let checkpoints = checkpoint_repo
                .list(project_id, args.task.as_deref())
                .map_err(|e| e.exit_code)?;
            render_and_print("checkpoint.list", Ok(checkpoints), is_json, ctx.quiet)
        }
        Some(CheckpointCommand::Show { checkpoint_id }) => {
            let cp = checkpoint_repo
                .find_by_id(project_id, checkpoint_id)
                .map_err(|e| e.exit_code)?
                .ok_or(ExitCode::ResourceNotFound)?;
            render_and_print("checkpoint.show", Ok(cp), is_json, ctx.quiet)
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
            render_and_print("checkpoint.correct", result, is_json, ctx.quiet)
        }
        None => {
            let now = chrono::Utc::now().to_rfc3339();
            let task_candidate = args.task.as_deref().or(ctx.task.as_deref());
            let resolved_task_id = match task_candidate {
                Some(t_ref) if t_ref != "current" && !t_ref.is_empty() => {
                    resolve_task_id(project_id, t_ref, conn).map_err(|e| e.exit_code)?
                }
                _ => {
                    let session_repo = SqliteSessionRepository::new(conn);
                    session_repo
                        .list(project_id)
                        .ok()
                        .and_then(|sessions| {
                            sessions
                                .into_iter()
                                .find(|s| {
                                    matches!(
                                        s.state,
                                        carryctx::domain::session::SessionState::Active
                                    )
                                })
                                .and_then(|s| s.task_id)
                        })
                        .unwrap_or_else(|| "current".to_string())
                }
            };
            let resolved_agent_id = match ctx.agent.as_deref() {
                Some(a_ref) if !a_ref.is_empty() => resolve_agent_id(project_id, a_ref, conn).ok(),
                _ => None,
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

            let input = application::checkpoint::CreateCheckpointInput {
                project_id: project_id.to_string(),
                task_id: resolved_task_id,
                session_id: args.session.clone().or_else(|| ctx.session.clone()),
                agent_id: resolved_agent_id,
                worktree_id: None,
                branch: runtime.git_project.branch.clone(),
                head: Some(runtime.git_project.head.clone()),
                done: args.done.clone(),
                remaining: args.remaining.clone(),
                blockers: args.blocker.clone(),
                risks: args.risk.clone(),
                next_actions: args.next.clone(),
                notes: args.note.clone(),
                repo_path,
            };

            let graph_repo = carryctx::repository::graph::GraphRepository::new(conn);
            let result = application::checkpoint::create_checkpoint(
                &checkpoint_repo,
                &event_repo,
                Some(&graph_repo),
                &git_cli,
                &input,
                &now,
            );
            render_and_print("checkpoint.create", result, is_json, ctx.quiet)
        }
    }
}
