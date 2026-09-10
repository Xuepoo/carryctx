use std::io::IsTerminal as _;
use std::path::Path;

use crate::application::runtime::InvocationContext;
use crate::error::{CarryCtxError, ExitCode};
use clap::Parser;

// ── Import ───────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub struct ImportArgs {
    /// Export directory produced by `carryctx export --format dir`
    #[arg(value_name = "DIR")]
    pub dir: String,

    /// Import mode on an initialized project: `replace` (with `--yes`).
    /// `merge` is reserved and always reports UNSUPPORTED_OPERATION in v1.
    #[arg(long)]
    pub mode: Option<String>,
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: import
// ═══════════════════════════════════════════════════════════════════════════

pub fn handle_import(
    args: &ImportArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    // Destructive replace without --yes prompts on a text TTY and refuses
    // elsewhere (Section 5: non-TTY refuses with STATE_CONFLICT exit 3).
    // JSON mode never prompts: machine consumers must pass --yes explicitly.
    let mut effective_yes = ctx.yes;
    if args.mode.as_deref() == Some("replace") && !ctx.dry_run && !effective_yes {
        let interactive_tty = ctx.interactive
            && !is_json
            && std::io::stdin().is_terminal()
            && std::io::stdout().is_terminal();
        if interactive_tty {
            eprint!("Replace project state from '{}'? [y/N] ", args.dir);
            use std::io::Write as _;
            let _ = std::io::stderr().flush();
            let mut answer = String::new();
            let _ = std::io::stdin().read_line(&mut answer);
            if answer.trim().eq_ignore_ascii_case("y") || answer.trim().eq_ignore_ascii_case("yes")
            {
                effective_yes = true;
            } else {
                return crate::cli::render_and_print::<serde_json::Value>(
                    "import.create",
                    Err(CarryCtxError::state_conflict(
                        "Import cancelled: replace requires confirmation.",
                    )),
                    is_json,
                    ctx.quiet,
                );
            }
        }
    }

    let work_dir = crate::cli::resolve_work_dir(ctx);
    let result = crate::application::import::import_project(
        work_dir,
        Path::new(&args.dir),
        args.mode.as_deref(),
        ctx.dry_run,
        effective_yes,
    );
    crate::cli::render_and_print("import.create", result, is_json, ctx.quiet)
}
