use carryctx::application::runtime::InvocationContext;
use carryctx::error::ExitCode;
use clap::Parser;

// ── Sync ─────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub enum SyncCommand {
    /// Copy the current state to a local snapshot path
    Push {
        /// Local snapshot directory (required; no default — pick an explicit path you control,
        /// e.g. a Syncthing folder or NAS mount)
        #[arg(long)]
        remote: String,
    },
    /// Replace the current state from a local snapshot path
    Pull {
        /// Local snapshot directory (required)
        #[arg(long)]
        remote: String,
    },
}

#[derive(Parser, Debug)]
pub struct SyncArgs {
    /// Sync subcommand to execute
    #[command(subcommand)]
    pub command: SyncCommand,
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: sync
// ═══════════════════════════════════════════════════════════════════════════

pub fn handle_sync(
    args: &SyncArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    let work_dir = crate::resolve_work_dir(ctx);
    match &args.command {
        SyncCommand::Push { remote } => {
            let result = carryctx::application::sync::sync_push(work_dir, remote);
            crate::render_and_print("sync.push", result, is_json, ctx.quiet)
        }
        SyncCommand::Pull { remote } => {
            let result = carryctx::application::sync::sync_pull(work_dir, remote);
            crate::render_and_print("sync.pull", result, is_json, ctx.quiet)
        }
    }
}
