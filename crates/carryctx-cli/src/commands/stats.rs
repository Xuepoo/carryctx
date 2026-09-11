use crate::application::runtime::{InvocationContext, OutputFormat};
use crate::application::stats::{
    ProjectStats, available_publication_ref, compute_stats, export_stats_csv, render_stats_markdown,
};
use crate::error::ExitCode;
use clap::Parser;
use std::path::Path;

// ── Stats ────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub struct StatsArgs {
    /// Agent to show stats for (optional, shows all if not provided)
    #[arg(long)]
    pub for_agent: Option<String>,

    /// Output report file path (.md, .csv, .json)
    #[arg(short, long)]
    pub output: Option<String>,

    /// Format report in Markdown
    #[arg(long)]
    pub markdown: bool,
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: stats
// ═══════════════════════════════════════════════════════════════════════════

pub fn handle_stats(
    args: &StatsArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    let work_dir = crate::cli::resolve_work_dir(ctx);
    let agent_filter = args.for_agent.as_deref();

    let result = compute_stats(work_dir, agent_filter);

    if let Ok(stats) = &result {
        if let Some(out_path) = &args.output {
            let content = if out_path.ends_with(".csv") {
                export_stats_csv(stats)
            } else if out_path.ends_with(".json") {
                serde_json::to_string_pretty(stats).unwrap_or_default()
            } else {
                render_stats_markdown(stats)
            };

            if let Err(e) = std::fs::write(out_path, content) {
                eprintln!("Failed to write stats output: {e}");
                return Err(ExitCode::General);
            }

            if !ctx.quiet {
                println!("Successfully exported project stats to {}", out_path);
            }
            return Ok(ExitCode::Success);
        }

        if args.markdown || matches!(ctx.format, OutputFormat::Markdown) {
            print!("{}", render_stats_markdown(stats));
            return Ok(ExitCode::Success);
        }

        if !is_json && !ctx.quiet {
            println!("Project Overview:");
            println!(
                "   Tasks: {} Total (Done: {}, In Progress: {}, Ready: {}, Planned: {})",
                stats.tasks_total,
                stats.tasks_completed,
                stats.tasks_in_progress,
                stats.tasks_ready,
                stats.tasks_planned
            );
            println!(
                "   Graph: {} Nodes, {} Edges",
                stats.graph_nodes_total, stats.graph_edges_total
            );
            println!(
                "   Sessions: {} | Checkpoints: {}",
                stats.sessions_total, stats.checkpoints_total
            );
            println!();

            println!(
                "{:<20} | {:<10} | {:<12} | {:<12} | {:<15} | {:<10}",
                "Agent Name", "Sessions", "Time Spent", "Checkpoints", "Tasks Done", "Blockers"
            );
            println!(
                "{:-<20}-+-{:-<10}-+-{:-<12}-+-{:-<12}-+-{:-<15}-+-{:-<10}",
                "", "", "", "", "", ""
            );
            for stat in &stats.agent_stats {
                let hours = stat.total_seconds / 3600;
                let minutes = (stat.total_seconds % 3600) / 60;
                let time_str = format!("{}h {}m", hours, minutes);
                println!(
                    "{:<20} | {:<10} | {:<12} | {:<12} | {:<15} | {:<10}",
                    stat.agent_name,
                    stat.total_sessions,
                    time_str,
                    stat.total_checkpoints,
                    stat.tasks_completed,
                    stat.blockers_reported
                );
            }
            print_empty_publication_hint(work_dir, stats);
            return Ok(ExitCode::Success);
        }
    }

    crate::cli::render_and_print("stats", result, is_json, ctx.quiet)
}

/// When the local project has no CarryCtx state but the repository carries an
/// in-repo publication ref (`refs/heads/carryctx-snapshots`, usually visible as
/// `origin/carryctx-snapshots` after a clone), point the reader at the restore
/// path. Hint only: nothing is fetched or imported automatically.
fn print_empty_publication_hint(work_dir: &Path, stats: &ProjectStats) {
    let empty = stats.tasks_total == 0
        && stats.sessions_total == 0
        && stats.checkpoints_total == 0
        && stats.graph_nodes_total == 0
        && stats.graph_edges_total == 0;
    if !empty {
        return;
    }
    if available_publication_ref(work_dir).is_none() {
        return;
    }
    println!();
    println!("Hint: no CarryCtx state found locally, but this repository publishes a");
    println!("      workflow snapshot on origin/carryctx-snapshots. Restore it with:");
    println!();
    println!(
        "        git fetch origin refs/heads/carryctx-snapshots:refs/remotes/origin/carryctx-snapshots"
    );
    println!("        carryctx init --non-interactive");
    println!("        carryctx import --from-git origin/carryctx-snapshots --mode replace --yes");
}
