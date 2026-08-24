use crate::*;
use carryctx::application::runtime::{InvocationContext, ProjectRuntime};
use carryctx::domain::search::{SearchKind, sanitize_fts5_query};
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
    /// (name or ULID). Named `--assignee` (formerly `--owner`) to avoid clashing
    /// with the global `--agent` (alias `--owner`) identity flag.
    #[arg(long)]
    pub assignee: Option<String>,

    /// Maximum number of hits to return.
    #[arg(long, default_value_t = 20)]
    pub limit: u32,
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: search
// ═══════════════════════════════════════════════════════════════════════════

pub fn handle_search(
    args: &SearchArgs,
    pre_opened: Option<ProjectRuntime>,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    // Reuse the dispatcher's pre-opened runtime when available; a second
    // open only happens (and reports) when that failed.
    let runtime = match pre_opened {
        Some(runtime) => runtime,
        None => open_runtime_or_report(ctx, "search")?,
    };
    let project_id = &runtime.config.project.id;
    let conn = runtime.database.connection();
    let verbose = ctx.verbose || runtime.config.output.verbose;

    let kind_and_assignee = || -> Result<(Option<SearchKind>, Option<String>), CarryCtxError> {
        let kind = args
            .r#type
            .as_deref()
            .map(|t| {
                SearchKind::parse(t).ok_or_else(|| {
                    CarryCtxError::validation_error(format!("Unknown --type '{t}'."))
                })
            })
            .transpose()?;
        let resolved_agent_id = match &args.assignee {
            Some(a) if !a.trim().is_empty() => Some(resolve_agent_id(project_id, a, conn)?),
            _ => None,
        };
        Ok((kind, resolved_agent_id))
    }();
    // Argument validation and reference resolution used to bail with a bare
    // `.map_err(|e| e.exit_code)?`, printing nothing anywhere (issue #96
    // remainder). Render failures through the standard error envelope.
    let (kind, agent_filter) = resolve_or_render(
        "search",
        kind_and_assignee,
        ctx,
        is_json,
        verbose,
        ctx.fields.as_deref(),
        Some(&runtime.config.output.fields),
    )?;

    let options = SearchOptions {
        kind,
        status: args.status.clone(),
        agent_id: agent_filter,
        limit: args.limit,
    };

    let repo = SearchRepository::new(conn);
    // A query with no searchable terms after sanitizing (whitespace-only,
    // lone quotes, bare punctuation) would reach FTS5 as an empty match and
    // fail with a raw syntax error; short-circuit to "no matches" instead.
    let result = if sanitize_fts5_query(&args.query)
        .chars()
        .any(|ch| ch.is_alphanumeric())
    {
        repo.search(project_id, &args.query, &options)
    } else {
        Ok(Vec::new())
    };

    if ctx.format == carryctx::application::runtime::OutputFormat::Markdown {
        return print_markdown_result(
            "search",
            result,
            |hits| {
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
            },
            ctx,
        );
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
