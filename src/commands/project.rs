use crate::*;
use carryctx::adapter::xdg::XdgPaths;
use carryctx::application::runtime::InvocationContext;
use carryctx::error::ExitCode;
use clap::Parser;
use std::path::Path;

// ── Project ──────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub enum ProjectCommand {
    /// Show metadata and statistics about the current project
    Show,
    /// List all known CarryCtx projects registered on this machine
    List,
    /// Register the current directory as a known project globally
    Register { path: String },
    /// Remove a project from the global registry
    Unregister { project_id: String },
    /// Run database migrations to upgrade the project state schema
    Migrate,
    /// Create a portable backup of the project's SQLite state database
    Backup,
    /// Restore the project's SQLite state from a backup file
    Restore { path: String },
    /// Archive old completed tasks to keep the primary database lightweight
    Prune {
        /// Prune tasks updated before this many days ago
        #[arg(long, default_value = "30")]
        older_than_days: u32,
    },
}

#[derive(Parser, Debug)]
pub struct ProjectArgs {
    /// Project subcommand to execute
    #[command(subcommand)]
    pub command: ProjectCommand,
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: project
// ═══════════════════════════════════════════════════════════════════════════

pub fn handle_project(
    args: &ProjectArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    match &args.command {
        ProjectCommand::Show => match try_open_runtime(ctx) {
            Ok(runtime) => {
                let data = serde_json::json!({
                    "projectId": runtime.config.project.id,
                    "projectName": runtime.config.project.name,
                    "repositoryRoot": runtime.git_project.repository_root.to_string_lossy(),
                    "gitCommonDir": runtime.git_project.git_common_dir.to_string_lossy(),
                    "dbPath": runtime.db_path.to_string_lossy(),
                    "mainBranch": runtime.config.git.main_branch,
                    "schemaVersion": runtime.config.schema_version,
                });
                render_and_print("project.show", Ok(data), is_json, ctx.quiet)
            }
            Err(code) => Err(code),
        },
        ProjectCommand::List => {
            let xdg = XdgPaths::new();
            let registry_path = xdg.registry_db();
            if registry_path.exists() {
                match std::fs::read_to_string(&registry_path) {
                    Ok(content) => {
                        let projects: Vec<serde_json::Value> =
                            serde_json::from_str(&content).unwrap_or_default();
                        render_and_print("project.list", Ok(projects), is_json, ctx.quiet)
                    }
                    Err(_) => render_and_print(
                        "project.list",
                        Ok(Vec::<serde_json::Value>::new()),
                        is_json,
                        ctx.quiet,
                    ),
                }
            } else {
                render_and_print(
                    "project.list",
                    Ok(Vec::<serde_json::Value>::new()),
                    is_json,
                    ctx.quiet,
                )
            }
        }
        ProjectCommand::Register { path } => {
            let _path = Path::new(path);
            // For now, init-project handles registration.
            // This is a placeholder that shows what would happen.
            let data = serde_json::json!({ "path": path, "status": "needs_init" });
            render_and_print("project.register", Ok(data), is_json, ctx.quiet)
        }
        ProjectCommand::Unregister { project_id } => {
            let data = serde_json::json!({ "projectId": project_id, "status": "unregistered" });
            render_and_print("project.unregister", Ok(data), is_json, ctx.quiet)
        }
        ProjectCommand::Migrate => match try_open_runtime(ctx) {
            Ok(mut runtime) => {
                let result = runtime.database.migrate().map(|applied| {
                    serde_json::json!({
                        "appliedMigrations": applied.iter().map(|m| m.name.clone()).collect::<Vec<_>>()
                    })
                });
                render_and_print("project.migrate", result, is_json, ctx.quiet)
            }
            Err(code) => Err(code),
        },
        ProjectCommand::Backup => match try_open_runtime(ctx) {
            Ok(mut runtime) => {
                let uow = runtime
                    .database
                    .begin_unit_of_work()
                    .map_err(|e| e.exit_code)?;
                let result = carryctx::application::project_mgmt::backup_project(
                    &runtime.git_project.repository_root,
                    &uow,
                );
                if result.is_ok() {
                    let _ = uow.commit();
                }
                render_and_print("project.backup", result, is_json, ctx.quiet)
            }
            Err(code) => Err(code),
        },
        ProjectCommand::Restore { path } => match try_open_runtime(ctx) {
            Ok(mut runtime) => {
                let uow = runtime
                    .database
                    .begin_unit_of_work()
                    .map_err(|e| e.exit_code)?;
                let result = carryctx::application::project_mgmt::restore_project(
                    Path::new(path),
                    &runtime.git_project.repository_root,
                    &uow,
                );
                if result.is_ok() {
                    let _ = uow.commit();
                }
                render_and_print("project.restore", result, is_json, ctx.quiet)
            }
            Err(code) => Err(code),
        },
        ProjectCommand::Prune { older_than_days } => match try_open_runtime(ctx) {
            Ok(mut runtime) => {
                let uow = runtime
                    .database
                    .begin_unit_of_work()
                    .map_err(|e| e.exit_code)?;
                let result =
                    carryctx::application::project_mgmt::prune_project(*older_than_days, &uow);
                if result.is_ok() {
                    let _ = uow.commit();
                }
                render_and_print("project.prune", result, is_json, ctx.quiet)
            }
            Err(code) => Err(code),
        },
    }
}
