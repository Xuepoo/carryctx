use crate::*;
use carryctx::application;
use carryctx::application::runtime::{InvocationContext, ProjectRuntime};
use carryctx::error::ExitCode;
use clap::Parser;

// ── Worktree ─────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub enum WorktreeCommand {
    /// Create a new Git worktree bound to a specific task
    Create {
        task_ref: String,
        /// Path to create the worktree in. Defaults to '../<task_id>'
        #[arg(long)]
        path: Option<String>,
        /// Branch name to create or checkout. Defaults to task ID
        #[arg(long)]
        branch: Option<String>,
        /// Base commit or branch to branch from. Defaults to main_branch
        #[arg(long)]
        base: Option<String>,
    },
    /// Bind an existing directory/worktree to a task
    Bind {
        path: String,
        /// Task ULID to bind to
        #[arg(long)]
        task: Option<String>,
    },
    /// List all known worktrees and their task bindings
    List,
    /// Show details for a specific worktree
    Show { worktree_ref: String },
    /// Show the binding status of the current directory
    Status,
    /// Detach a worktree from its task without deleting anything
    ///
    /// Only nulls the task binding: the worktree directory and its
    /// registration row both stay (use `worktree remove` to delete them).
    Unbind { worktree_ref: String },
    /// Remove a Git worktree and delete its registration
    ///
    /// Accepts a registration ULID, bound task display id (CTX-XXXX), or
    /// directory path (absolute or repository-relative). A live, clean
    /// worktree is removed via `git worktree remove`; a dirty or
    /// uncommitted one is refused with STATE_CONFLICT unless --force. When
    /// the directory is already gone, only the orphaned registration row is
    /// deleted.
    Remove {
        worktree_ref: String,
        /// Remove even when the worktree has modified or untracked files
        #[arg(long)]
        force: bool,
    },
}

#[derive(Parser, Debug)]
pub struct WorktreeArgs {
    /// Worktree subcommand to execute
    #[command(subcommand)]
    pub command: WorktreeCommand,
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: worktree
// ═══════════════════════════════════════════════════════════════════════════

pub fn handle_worktree(
    args: &WorktreeArgs,
    pre_opened: Option<ProjectRuntime>,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    if let Some(result) = check_dry_run_envelope(
        ctx,
        &subcommand_label("worktree", &args.command),
        &format!("worktree {:?}", args.command),
    ) {
        return result;
    }
    // Reuse the dispatcher's pre-opened runtime when available; a second
    // open only happens (and reports) when that failed.
    let mut runtime = match pre_opened {
        Some(runtime) => runtime,
        None => open_runtime_or_report(ctx, "worktree")?,
    };
    let verbose = ctx.verbose || runtime.config.output.verbose;
    let project_id = &runtime.config.project.id;
    let conn = runtime.database.connection_mut();
    let now = chrono::Utc::now().to_rfc3339();

    let worktree_repo = SqliteWorktreeRepository::new(conn);
    let task_repo = SqliteTaskRepository::new(conn);
    let event_repo = SqliteEventRepository::new(conn);
    let git_cli = GitCli::new();

    match &args.command {
        WorktreeCommand::Create {
            task_ref,
            path,
            branch,
            base,
        } => {
            let branch_name = branch.clone().unwrap_or_else(|| {
                format!("carryctx/{}", task_ref.replace('/', "-").to_lowercase())
            });
            let worktree_path = path.clone().unwrap_or_else(|| {
                format!(".worktrees/{}", task_ref.replace('/', "-").to_lowercase())
            });
            let input = application::worktree::CreateWorktreeInput {
                project_id: project_id.to_string(),
                repository_root: runtime
                    .git_project
                    .repository_root
                    .to_string_lossy()
                    .to_string(),
                path: worktree_path,
                branch: branch_name,
                base: base.clone(),
                task_id: Some(task_ref.clone()),
            };
            let result = application::worktree::create_worktree(
                &worktree_repo,
                &task_repo,
                &event_repo,
                &git_cli,
                &runtime.xdg,
                &input,
                &now,
            );
            render_and_print_entity(
                "worktree.create",
                result,
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        WorktreeCommand::Bind { path, task } => {
            let input = application::worktree::BindWorktreeInput {
                project_id: project_id.to_string(),
                path: path.clone(),
                task_id: task.clone(),
            };
            let result = application::worktree::bind_worktree(
                &worktree_repo,
                &task_repo,
                &event_repo,
                &git_cli,
                &input,
                &now,
            );
            render_and_print_entity(
                "worktree.bind",
                result,
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        WorktreeCommand::List => {
            let result = application::worktree::list_worktrees(
                &worktree_repo,
                &git_cli,
                project_id,
                Some(&runtime.git_project.repository_root.to_string_lossy()),
            );

            // Markdown format support
            if ctx.format == carryctx::application::runtime::OutputFormat::Markdown {
                return print_markdown_result(
                    "worktree.list",
                    result,
                    |trees| {
                        let mut out = String::from("# Worktrees\n\n");
                        out.push_str("| Path | Branch | Task |\n");
                        out.push_str("|---|---|---|\n");
                        for w in trees {
                            let path = w.path.split('/').next_back().unwrap_or(&w.path);
                            let task = w
                                .task_id
                                .as_deref()
                                .map(|t| truncate_chars(t, 8))
                                .unwrap_or_else(|| "-".to_string());
                            let branch = w.branch.as_deref().unwrap_or("-");
                            out.push_str(&format!("| {} | {} | {} |\n", path, branch, task));
                        }
                        out
                    },
                    ctx,
                );
            }

            render_and_print_entity(
                "worktree.list",
                result,
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        WorktreeCommand::Show { worktree_ref } => {
            let result = application::worktree::show_worktree(
                &worktree_repo,
                &git_cli,
                project_id,
                worktree_ref,
            );
            render_and_print_entity(
                "worktree.show",
                result,
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        WorktreeCommand::Status => {
            let worktrees = worktree_repo.list(project_id).map_err(|e| e.exit_code)?;
            let git_trees = git_cli
                .list_worktrees(&runtime.git_project.repository_root)
                .ok()
                .unwrap_or_default();
            let data = serde_json::json!({
                "registered": worktrees,
                "gitWorktrees": git_trees,
            });
            render_and_print("worktree.status", Ok(data), is_json, ctx.quiet)
        }
        WorktreeCommand::Unbind { worktree_ref } => {
            let result = application::worktree::unbind_worktree(
                &worktree_repo,
                &event_repo,
                project_id,
                worktree_ref,
                &now,
            );
            render_and_print_entity(
                "worktree.unbind",
                result,
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        WorktreeCommand::Remove {
            worktree_ref,
            force,
        } => {
            let result = application::worktree::remove_worktree(
                &worktree_repo,
                &task_repo,
                &event_repo,
                &git_cli,
                &application::worktree::RemoveWorktreeInput {
                    project_id: project_id.to_string(),
                    repository_root: runtime
                        .git_project
                        .repository_root
                        .to_string_lossy()
                        .to_string(),
                    worktree_ref: worktree_ref.clone(),
                    force: *force,
                },
                &now,
            );
            render_and_print_entity(
                "worktree.remove",
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
