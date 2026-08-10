use crate::*;
use carryctx::application::runtime::InvocationContext;
use carryctx::domain::search::SearchKind;
use carryctx::error::ExitCode;
use carryctx::repository::search::{SearchOptions, SearchRepository};
use clap::Parser;

// ── Search ───────────────────────────────────────────────────────────────

/// Full-text search across tasks, progress items, checkpoints, and decisions.
///
/// Every hit resolves back to its owning task's display ID, status, and
/// (where known) the branch it was worked on, since that's usually the
/// reason a search happens — the branch name alone rarely carries what
/// actually changed and why. Ranked by SQLite FTS5's BM25 score.
#[derive(Parser, Debug)]
pub struct SearchArgs {
    /// The search query. Supports SQLite FTS5 syntax (e.g. `"exact phrase"`,
    /// `term1 OR term2`, `prefix*`).
    pub query: String,

    /// Restrict the search to one entity kind.
    #[arg(long, value_parser = ["task", "progress", "checkpoint", "decision"])]
    pub r#type: Option<String>,

    /// Restrict to hits whose owning task has this exact status
    /// (e.g. `in_progress`, `completed`).
    #[arg(long)]
    pub status: Option<String>,

    /// Restrict to hits whose owning task is owned by this agent
    /// (name or ULID). Named `--owner` (not `--agent`) to avoid clashing
    /// with the global `--agent`/`CARRYCTX_AGENT` identity flag.
    #[arg(long)]
    pub owner: Option<String>,

    /// Maximum number of hits to return.
    #[arg(long, default_value_t = 20)]
    pub limit: u32,
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: search
// ═══════════════════════════════════════════════════════════════════════════

pub fn handle_search(
    args: &SearchArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    let runtime = try_open_runtime(ctx)?;
    let project_id = &runtime.config.project.id;
    let conn = runtime.database.connection();
    let verbose = ctx.verbose || runtime.config.output.verbose;

    let kind = args
        .r#type
        .as_deref()
        .map(|t| {
            SearchKind::parse(t)
                .ok_or_else(|| CarryCtxError::validation_error(format!("Unknown --type '{t}'.")))
        })
        .transpose()
        .map_err(|e| e.exit_code)?;

    let resolved_agent_id = match &args.owner {
        Some(a) if !a.trim().is_empty() => {
            Some(resolve_agent_id(project_id, a, conn).map_err(|e| e.exit_code)?)
        }
        _ => None,
    };

    let options = SearchOptions {
        kind,
        status: args.status.clone(),
        agent_id: resolved_agent_id,
        limit: args.limit,
    };

    let repo = SearchRepository::new(conn);
    let result = repo.search(project_id, &args.query, &options);

    if ctx.format == carryctx::application::runtime::OutputFormat::Markdown {
        let md = match &result {
            Ok(hits) => {
                if hits.is_empty() {
                    "No matches.\n".to_string()
                } else {
                    let mut out = String::from("# Search Results\n\n");
                    out.push_str("| Kind | Task | Branch | Snippet |\n");
                    out.push_str("|---|---|---|---|\n");
                    for hit in hits {
                        out.push_str(&format!(
                            "| {} | {} ({}) | {} | {} |\n",
                            hit.kind.as_str(),
                            hit.task_display_id,
                            hit.task_status,
                            hit.branch.as_deref().unwrap_or("-"),
                            hit.snippet.replace('|', "\\|").replace('\n', " ")
                        ));
                    }
                    out
                }
            }
            Err(e) => format!("Error: {e}"),
        };
        if !ctx.quiet {
            print!("{md}");
        }
        return Ok(ExitCode::Success);
    }

    render_and_print_entity(
        "search",
        result,
        is_json,
        ctx.quiet,
        verbose,
        ctx.fields.as_deref(),
        Some(&runtime.config.output.fields),
    )
}
