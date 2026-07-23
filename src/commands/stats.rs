use carryctx::application::runtime::{InvocationContext, OutputFormat};
use carryctx::application::stats::{compute_stats, export_stats_csv, render_stats_markdown};
use carryctx::error::ExitCode;
use clap::Parser;

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
    let work_dir = crate::resolve_work_dir(ctx);
    let agent_filter = args.for_agent.as_deref();

    let result = compute_stats(work_dir, agent_filter);

    match &result {
        Ok(stats) => {
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
                let total = stats.tasks_total;
                let done = stats.tasks_completed;
                let in_prog = stats.tasks_in_progress;
                let ready = stats.tasks_ready;
                let planned = stats.tasks_planned;
                let nodes = stats.graph_nodes_total;
                let edges = stats.graph_edges_total;
                let sessions = stats.sessions_total;
                let chk = stats.checkpoints_total;

                println!("Project Overview:");
                println!(
                    "   Tasks: {total} Total (Done: {done}, In Progress: {in_prog}, Ready: {ready}, Planned: {planned})"
                );
                println!("   Graph: {nodes} Nodes, {edges} Edges");
                println!("   Sessions: {sessions} | Checkpoints: {chk}");
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
                return Ok(ExitCode::Success);
            }
        }
        Err(_) => {}
    }

    crate::render_and_print("stats", result, is_json, ctx.quiet)
}
