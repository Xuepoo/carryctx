use std::path::PathBuf;

use crate::application::runtime::InvocationContext;
use crate::error::{CarryCtxError, ExitCode};
use clap::Parser;

// ── Export (ctxpack dir v1) ────────────────────────────────────────────

/// Offline-first portable export of project state (`ctxpack-dir` v1).
///
/// The bundle-format flag is deliberately `--pack-format`, not `--format`:
/// the root CLI already defines a global `--format` (output style
/// text|json|markdown) and a same-named flag here collides inside this scope
/// (see the `graph export --type` precedent and the CLI-integrity test in
/// main.rs). `--dry-run` arrives via the global flag and prints the plan
/// (entity counts, target path) without writing anything.
#[derive(Parser, Debug)]
pub struct PackArgs {
    /// Bundle layout format (v1 supports only `dir`)
    #[arg(long, default_value = "dir")]
    pub pack_format: String,

    /// Output directory for the export bundle
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Stream a tar archive of the bundle to stdout (unsupported in v1)
    #[arg(long)]
    pub stdout: bool,
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: export
// ═══════════════════════════════════════════════════════════════════════════

pub fn handle_export(
    args: &PackArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    // v1 ships no in-binary tar writer (offline, no new dependencies):
    // fail closed with UNSUPPORTED_OPERATION instead of a corrupt stream.
    if args.stdout {
        return crate::cli::render_and_print(
            "export.create",
            Err::<serde_json::Value, _>(CarryCtxError::unsupported_operation(
                "Export '--stdout' tar streaming is not supported in v1: the binary ships no tar writer. Export with '-o <dir>' and stream externally, e.g. 'tar -cf - <dir> | ssh ...'.",
            )),
            is_json,
            ctx.quiet,
        );
    }
    let work_dir = crate::cli::resolve_work_dir(ctx);
    if ctx.dry_run {
        let result = require_output(args).and_then(|out| {
            crate::application::export::plan_export(work_dir, &args.pack_format, &out)
        });
        if !ctx.quiet {
            if let Ok(data) = &result {
                let rows: u64 = data
                    .get("counts")
                    .and_then(|c| c.as_object())
                    .map(|counts| counts.values().filter_map(|v| v.as_u64()).sum())
                    .unwrap_or(0);
                eprintln!(
                    "[dry-run] Would export {rows} rows to '{}'; nothing written.",
                    data.get("path").and_then(|p| p.as_str()).unwrap_or("?")
                );
            }
        }
        return crate::cli::render_and_print("export.create", result, is_json, ctx.quiet);
    }
    let result = require_output(args).and_then(|out| {
        crate::application::export::run_export(
            work_dir,
            &args.pack_format,
            &out,
            ctx.agent.clone(),
            ctx.session.clone(),
        )
    });
    crate::cli::render_and_print("export.create", result, is_json, ctx.quiet)
}

fn require_output(args: &PackArgs) -> Result<PathBuf, CarryCtxError> {
    args.output.clone().ok_or_else(|| {
        CarryCtxError::invalid_arguments(
            "Missing export target: pass '-o <dir>' (or '--stdout' for a tar stream).",
        )
    })
}
