use carryctx::application::runtime::InvocationContext;
use carryctx::error::ExitCode;
use clap::Parser;

// ── Stats ────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub struct StatsArgs {
    /// Agent to show stats for (optional, shows all if not provided)
    #[arg(long)]
    pub for_agent: Option<String>,
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

    let result = carryctx::application::stats::compute_stats(work_dir, agent_filter);

    if !is_json && !ctx.quiet {
        if let Ok(stats) = &result {
            println!(
                "{:<20} | {:<10} | {:<12} | {:<12} | {:<15} | {:<10}",
                "Agent Name", "Sessions", "Time Spent", "Checkpoints", "Tasks Done", "Blockers"
            );
            println!(
                "{:-<20}-+-{:-<10}-+-{:-<12}-+-{:-<12}-+-{:-<15}-+-{:-<10}",
                "", "", "", "", "", ""
            );
            for stat in stats {
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

    crate::render_and_print("stats", result, is_json, ctx.quiet)
}
