use crate::*;
use carryctx::application;
use carryctx::application::runtime::InvocationContext;
use carryctx::error::ExitCode;
use clap::Parser;

// ── Init ─────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub struct InitArgs {
    /// Provide a custom name for the new project. Defaults to the directory name.
    #[arg(long)]
    pub name: Option<String>,

    /// Task prefix identifier for issue tracking (e.g., 'PROJ' for tasks like 'PROJ-123').
    #[arg(long)]
    pub task_prefix: Option<String>,

    /// Set the main/default branch name for Git (e.g., 'main' or 'master').
    #[arg(long)]
    pub main_branch: Option<String>,

    /// Force initialization even if the directory already contains a .carryctx folder.
    #[arg(long)]
    pub force: bool,

    /// Create a minimal setup without adding standard documentation and agent templates.
    #[arg(long)]
    pub minimal: bool,

    /// Automatically install standard agent skills during initialization.
    #[arg(long)]
    pub install_skill: bool,
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: init
// ═══════════════════════════════════════════════════════════════════════════

pub fn handle_init(args: &InitArgs, ctx: &InvocationContext) -> Result<ExitCode, ExitCode> {
    let work_dir = resolve_work_dir(ctx);

    if ctx.dry_run {
        println!(
            "[dry-run] Would initialize CarryCtx at {}",
            work_dir.display()
        );
        return Ok(ExitCode::Success);
    }

    let result = application::init::init_project(
        work_dir,
        args.name.as_deref(),
        args.task_prefix.as_deref(),
        args.force,
    );

    render_and_print("init", result, false, ctx.quiet)
}
