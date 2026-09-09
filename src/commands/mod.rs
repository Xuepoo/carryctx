pub mod agent;
pub mod checkpoint;
pub mod completions;
pub mod config;
pub mod context;
pub mod decision;
pub mod doctor;
pub mod event;
pub mod export;
pub mod graph;
pub mod handoff;
pub mod hooks;
pub mod import;
pub mod init;
pub mod mcp;
pub mod preset;
pub mod progress;
pub mod project;
pub mod resume;
pub mod search;
pub mod session;
pub mod skill;
pub mod stats;
pub mod status;
pub mod sync;
pub mod task;
pub mod team;
pub mod version;
pub mod worktree;

pub use agent::*;
pub use checkpoint::*;
pub use completions::*;
pub use config::*;
pub use context::*;
pub use decision::*;
pub use doctor::*;
pub use event::*;
pub use export::*;
pub use graph::*;
pub use handoff::*;
pub use hooks::*;
pub use import::*;
pub use init::*;
pub use mcp::*;
pub use preset::*;
pub use progress::*;
pub use project::*;
pub use resume::*;
pub use search::*;
pub use session::*;
pub use skill::*;
pub use stats::*;
pub use status::*;
pub use sync::*;
pub use task::*;
pub use team::*;
pub use version::*;
pub use worktree::*;

// ═══════════════════════════════════════════════════════════════════════════
//  Shared command-surface helpers
// ═══════════════════════════════════════════════════════════════════════════

use carryctx::application::runtime::InvocationContext;
use carryctx::error::{CarryCtxError, ExitCode};

/// Derive a dotted command label (`"progress.todo"`) from a parsed
/// subcommand enum, for use by the shared dry-run envelope so JSON consumers
/// see the same `command` value the real execution path would emit.
pub fn subcommand_label(prefix: &str, command: &impl std::fmt::Debug) -> String {
    let variant = format!("{command:?}")
        .split([' ', '(', '{'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    format!("{prefix}.{variant}")
}

/// Truncate `s` to at most `max_chars` characters on char boundaries.
///
/// Every markdown preview slice must go through this helper: raw byte
/// slicing (`&s[..8]`) panics with "byte index is not a char boundary"
/// whenever the value contains multibyte UTF-8 (CJK titles, emoji agent
/// names), which violates the zero-panic standard.
pub fn truncate_chars(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    match s.char_indices().nth(max_chars) {
        Some((idx, _)) => s[..idx].to_string(),
        None => s.to_string(),
    }
}

/// Print a rendered markdown document on stdout, or route a failure through
/// the standard error path.
///
/// Markdown renderers used to print `Error: {e}` onto stdout and return exit
/// code 0, so machine consumers saw a successful document containing the
/// error text. This helper keeps the success behavior (markdown on stdout,
/// exit 0) and sends errors to the standard envelope renderer: human-readable
/// message on stderr in text mode, error envelope in JSON mode, always with
/// the real exit code. Consumes `result` so the caller's non-markdown branch
/// stays untouched.
pub fn print_markdown_result<T, F>(
    command: &str,
    result: Result<T, CarryCtxError>,
    render: F,
    ctx: &InvocationContext,
) -> Result<ExitCode, ExitCode>
where
    F: FnOnce(T) -> String,
{
    match result {
        Ok(value) => {
            let md = render(value);
            if !ctx.quiet {
                print!("{md}");
            }
            Ok(ExitCode::Success)
        }
        Err(err) => crate::render_and_print_entity::<serde_json::Value>(
            command,
            Err(err),
            matches!(
                ctx.format,
                carryctx::application::runtime::OutputFormat::Json
            ),
            ctx.quiet,
            false,
            None,
            None,
        ),
    }
}

/// JSON-aware dry-run gate for handlers that have no bespoke dry-run block.
///
/// The legacy `check_dry_run` printed a stderr note regardless of the output
/// format, so `--json --dry-run` produced stderr-only text and no stdout
/// envelope — scripts parsing stdout hung waiting for a document that never
/// arrived. This variant emits the standard success envelope with
/// `operation.applied = false` in JSON mode (matching the bespoke task/team/
/// agent dry-run blocks) and keeps the `[dry-run] Would …` note on stderr in
/// text mode. Returns `None` when dry-run is not active.
pub fn check_dry_run_envelope(
    ctx: &InvocationContext,
    command: &str,
    description: &str,
) -> Option<Result<ExitCode, ExitCode>> {
    if !ctx.dry_run {
        return None;
    }
    let is_json = matches!(
        ctx.format,
        carryctx::application::runtime::OutputFormat::Json
    );
    eprintln!("[dry-run] Would {description}");
    if is_json {
        Some(crate::render_and_print_entity::<serde_json::Value>(
            command,
            Ok(serde_json::json!({"operation": {"applied": false}})),
            true,
            ctx.quiet,
            false,
            None,
            None,
        ))
    } else {
        Some(Ok(ExitCode::Success))
    }
}

/// Render a failure that occurs while building a bespoke JSON dry-run
/// preview (e.g. resolving a task/team reference) through the standard
/// error-envelope path instead of silently mapping it to a bare exit code.
///
/// The old `.map_err(|e| e.exit_code)?` shape aborted with the right code but
/// printed nothing anywhere — no stdout envelope, no stderr message — leaving
/// JSON consumers hanging and humans clueless.
pub fn render_dry_run_error(
    command: &str,
    err: CarryCtxError,
    ctx: &InvocationContext,
) -> ExitCode {
    crate::render_and_print_entity::<serde_json::Value>(
        command,
        Err(err),
        true,
        ctx.quiet,
        false,
        None,
        None,
    )
    .err()
    .unwrap_or(ExitCode::General)
}

/// Resolve-entity combinator for mutating handlers.
///
/// On success the resolved value passes through untouched; on failure the
/// standard error envelope is rendered for `command` (JSON mode: error
/// document on stdout, text mode: human message on stderr) and the mapped
/// exit code is returned. Collapses the repeated
/// `match resolve_x() { Ok.. Err(e) => return render_and_print_entity(..) }`
/// blocks that used to be pasted per-arm across team/handoff/task handlers.
#[allow(clippy::too_many_arguments)]
pub fn resolve_or_render<T>(
    command: &str,
    result: Result<T, CarryCtxError>,
    ctx: &InvocationContext,
    is_json: bool,
    verbose: bool,
    cli_fields: Option<&[String]>,
    config_fields: Option<&std::collections::HashMap<String, Vec<String>>>,
) -> Result<T, ExitCode> {
    result.map_err(|err| {
        crate::render_and_print_entity::<serde_json::Value>(
            command,
            Err(err),
            is_json,
            ctx.quiet,
            verbose,
            cli_fields,
            config_fields,
        )
        .err()
        .unwrap_or(ExitCode::General)
    })
}

#[cfg(test)]
mod shared_helper_tests {
    use super::*;

    #[test]
    fn truncate_chars_is_char_boundary_safe() {
        assert_eq!(truncate_chars("hello", 3), "hel");
        assert_eq!(truncate_chars("hi", 8), "hi");
        assert_eq!(truncate_chars("", 4), "");
        // CJK: three chars even though each is 3 bytes.
        assert_eq!(truncate_chars("日本語テスト", 3), "日本語");
        // Emoji (astral plane, 4 bytes per char).
        assert_eq!(truncate_chars("🚀🚀🚀", 2), "🚀🚀");
    }

    #[test]
    fn truncate_chars_never_panics_on_multibyte_input() {
        let samples = ["中文标题很长很长", "mix中英text混排", "\u{1F600}\u{1F600}"];
        for s in samples {
            for n in 0..=12 {
                let cut = truncate_chars(s, n);
                assert!(cut.is_char_boundary(cut.len()));
                assert!(cut.chars().count() <= n);
            }
        }
    }

    #[test]
    fn truncate_chars_zero_yields_empty() {
        assert_eq!(truncate_chars("abc", 0), "");
    }
}
