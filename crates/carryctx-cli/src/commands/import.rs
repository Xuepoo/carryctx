use std::io::IsTerminal as _;
use std::path::Path;

use crate::adapter::git::SNAPSHOT_REF_DEFAULT;
use crate::application::runtime::InvocationContext;
use crate::error::{CarryCtxError, ExitCode};
use clap::Parser;

// ── Import ───────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub struct ImportArgs {
    /// Export directory produced by `carryctx export --pack-format dir`.
    /// Mutually exclusive with `--from-git`.
    #[arg(value_name = "DIR")]
    pub dir: Option<String>,

    /// Import the ctxpack bundle stored at the tip of a local Git ref
    /// (e.g. `refs/carryctx/local` or a remote-tracking ref). Fully offline;
    /// mutually exclusive with the positional `<DIR>`.
    #[arg(long, value_name = "REF")]
    pub from_git: Option<String>,

    /// Import mode on an initialized project: `replace` (whole-state, with
    /// `--yes`) or `merge` (three-way merge from the export DAG).
    #[arg(long)]
    pub mode: Option<String>,

    /// Merge base override for `--mode merge`: a ctxpack directory, a local
    /// snapshot-cache export id, or a Git revision/ref.
    #[arg(long, value_name = "DIR|EXPORT_ID|REF")]
    pub base: Option<String>,

    /// `--mode merge`: refuse a degraded base-less merge instead of running it.
    #[arg(long)]
    pub require_base: bool,

    /// `--mode merge`: promote last-writer-wins row edits to blocking conflicts.
    #[arg(long)]
    pub strict_edits: bool,

    /// `--mode merge`: write one two-parent merge snapshot commit to this
    /// local-only Git ref after a successful merge. Must live under
    /// `refs/carryctx/...` (default `refs/carryctx/local` when passed without a
    /// value). Omit the flag to write no snapshot commit and leave
    /// `snapshot_state` unchanged.
    #[arg(
        long,
        value_name = "REF",
        num_args = 0..=1,
        default_missing_value = SNAPSHOT_REF_DEFAULT
    )]
    pub snapshot_ref: Option<String>,
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: import
// ═══════════════════════════════════════════════════════════════════════════

pub fn handle_import(
    args: &ImportArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    // Exactly one source: positional `<DIR>` or `--from-git <REF>`.
    let source = match (args.dir.as_deref(), args.from_git.as_deref()) {
        (Some(_), Some(_)) => {
            return crate::cli::render_and_print::<serde_json::Value>(
                "import.create",
                Err(CarryCtxError::invalid_arguments(
                    "Pass either a positional <DIR> or --from-git <REF>, not both.",
                )),
                is_json,
                ctx.quiet,
            );
        }
        (None, None) => {
            return crate::cli::render_and_print::<serde_json::Value>(
                "import.create",
                Err(CarryCtxError::invalid_arguments(
                    "Missing import source: pass a positional <DIR> or --from-git <REF>.",
                )),
                is_json,
                ctx.quiet,
            );
        }
        (Some(dir), None) => ImportSource::Dir(dir.to_string()),
        (None, Some(git_ref)) => ImportSource::GitRef(git_ref.to_string()),
    };

    // Reject leading `-` in revision-shaped values so a value can never be
    // mistaken for a Git option (argv-based, no shell).
    for (flag, value) in [
        ("--from-git", args.from_git.as_deref()),
        ("--base", args.base.as_deref()),
        ("--snapshot-ref", args.snapshot_ref.as_deref()),
    ] {
        if let Some(value) = value {
            if value.starts_with('-') {
                return crate::cli::render_and_print::<serde_json::Value>(
                    "import.create",
                    Err(CarryCtxError::invalid_arguments(format!(
                        "{flag} value '{value}' must not start with '-'."
                    ))),
                    is_json,
                    ctx.quiet,
                );
            }
        }
    }

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
            eprint!("Replace project state from '{}'? [y/N] ", source.label());
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
    let merge_options = crate::application::merge_import::MergeImportOptions {
        base: args.base.as_deref(),
        require_base: args.require_base,
        strict_edits: args.strict_edits,
        from_git_ref: args.from_git.as_deref(),
        snapshot_ref: args.snapshot_ref.as_deref(),
    };
    let result = match &source {
        ImportSource::Dir(dir) => crate::application::import::import_project(
            work_dir,
            Path::new(dir),
            args.mode.as_deref(),
            ctx.dry_run,
            effective_yes,
            &merge_options,
            ctx.agent.clone(),
            ctx.session.clone(),
        ),
        ImportSource::GitRef(git_ref) => crate::application::import::import_from_git_project(
            work_dir,
            git_ref,
            args.mode.as_deref(),
            ctx.dry_run,
            effective_yes,
            &merge_options,
            ctx.agent.clone(),
            ctx.session.clone(),
        ),
    };
    crate::cli::render_and_print("import.create", result, is_json, ctx.quiet)
}

/// The two mutually exclusive import sources.
enum ImportSource {
    Dir(String),
    GitRef(String),
}

impl ImportSource {
    fn label(&self) -> &str {
        match self {
            ImportSource::Dir(dir) => dir,
            ImportSource::GitRef(git_ref) => git_ref,
        }
    }
}
