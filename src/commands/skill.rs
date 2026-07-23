use crate::*;
use carryctx::adapter::xdg::XdgPaths;
use carryctx::application::runtime::InvocationContext;
use carryctx::error::ExitCode;
use clap::Parser;

// ── Skill ────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub enum SkillCommand {
    /// Install a new executable skill from a local path or repository
    Install { source: String },
    /// List all installed skills available to agents
    List,
    /// Print the directory path where skills are stored
    Path,
    /// Diagnose and repair issues with installed skills
    Doctor,
}

#[derive(Parser, Debug)]
pub struct SkillArgs {
    /// Skill subcommand to execute
    #[command(subcommand)]
    pub command: SkillCommand,
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: skill
// ═══════════════════════════════════════════════════════════════════════════

pub fn handle_skill(
    args: &SkillArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    let xdg = XdgPaths::new();
    let skills_dir = xdg.data_home.join("carryctx").join("skills");

    match &args.command {
        SkillCommand::Install { source } => {
            let data = serde_json::json!({
                "source": source,
                "target": skills_dir.to_string_lossy(),
                "status": "placeholder",
            });
            render_and_print("skill.install", Ok(data), is_json, ctx.quiet)
        }
        SkillCommand::List => {
            let installed = if skills_dir.exists() {
                let entries: Vec<String> = std::fs::read_dir(&skills_dir)
                    .map(|rd| {
                        rd.filter_map(|e| e.ok())
                            .filter(|e| e.path().is_dir())
                            .filter_map(|e| e.file_name().into_string().ok())
                            .collect()
                    })
                    .unwrap_or_default();
                entries
            } else {
                Vec::new()
            };
            let data = serde_json::json!({
                "skillsDir": skills_dir.to_string_lossy(),
                "installed": installed,
            });
            render_and_print("skill.list", Ok(data), is_json, ctx.quiet)
        }
        SkillCommand::Path => {
            let data = serde_json::json!({ "path": skills_dir.to_string_lossy() });
            render_and_print("skill.path", Ok(data), is_json, ctx.quiet)
        }
        SkillCommand::Doctor => {
            let exists = skills_dir.exists();
            let entries = if exists {
                std::fs::read_dir(&skills_dir)
                    .map(|rd| {
                        rd.filter_map(|e| e.ok())
                            .filter(|e| e.path().is_dir())
                            .count()
                    })
                    .unwrap_or(0)
            } else {
                0
            };
            let data = serde_json::json!({
                "exists": exists,
                "path": skills_dir.to_string_lossy(),
                "installedCount": entries,
                "status": if exists { "ok" } else { "not_found" },
            });
            render_and_print("skill.doctor", Ok(data), is_json, ctx.quiet)
        }
    }
}
