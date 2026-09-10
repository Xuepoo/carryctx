use crate::application::runtime::InvocationContext;
use crate::error::ExitCode;
use clap::Parser;

// ── Sync ─────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub enum SyncCommand {
    /// Copy the current state to a local snapshot path
    Push {
        /// Explicit snapshot path you control (e.g. a Syncthing folder or NAS mount)
        #[arg(long)]
        remote: String,
    },
    /// Replace the current state from a local snapshot path
    Pull {
        /// Explicit snapshot path you control (e.g. a Syncthing folder or NAS mount)
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
    let work_dir = crate::cli::resolve_work_dir(ctx);
    match &args.command {
        SyncCommand::Push { remote } => {
            let result = crate::application::sync::sync_push(work_dir, remote);
            crate::cli::render_and_print("sync.push", result, is_json, ctx.quiet)
        }
        SyncCommand::Pull { remote } => {
            let result = crate::application::sync::sync_pull(work_dir, remote);
            crate::cli::render_and_print("sync.pull", result, is_json, ctx.quiet)
        }
    }
}
