use carryctx::application::runtime::InvocationContext;
use carryctx::error::ExitCode;
use clap::Parser;

// ── Sync ─────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub enum SyncCommand {
    /// Push the current state to the configured remote
    Push {
        #[arg(long, default_value = "/tmp/carryctx-remote")]
        remote: String,
    },
    /// Pull the latest state from the configured remote
    Pull {
        #[arg(long, default_value = "/tmp/carryctx-remote")]
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
