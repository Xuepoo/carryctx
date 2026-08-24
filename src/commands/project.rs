use crate::*;
use carryctx::adapter::xdg::XdgPaths;
use carryctx::application::runtime::{InvocationContext, ProjectRuntime};
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
        #[arg(long, alias = "older-than", default_value = "30")]
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
    mut pre_opened: Option<ProjectRuntime>,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    // Runtime-backed arms reuse the dispatcher's pre-opened runtime when
    // available; a second open only happens (and reports) when that failed.
    // Registry-only and restore arms never open the runtime, matching their
    // historical behavior of working outside an initialized project.
    match &args.command {
        ProjectCommand::Show => {
            let runtime = match pre_opened.take() {
                Some(runtime) => runtime,
                None => open_runtime_or_report(ctx, "project.show")?,
            };
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
        ProjectCommand::List => {
            let xdg = XdgPaths::new();
            let registry_path = xdg.registry_db();
            // A corrupted or unreadable registry must surface as an error,
            // not silently render as an empty project list.
            let projects: Result<Vec<serde_json::Value>, CarryCtxError> = if registry_path.exists()
            {
                std::fs::read_to_string(&registry_path)
                    .map_err(|e| {
                        CarryCtxError::database_error(format!(
                            "Failed to read project registry: {e}"
                        ))
                    })
                    .and_then(|content| {
                        serde_json::from_str(&content).map_err(|e| {
                            CarryCtxError::database_error(format!(
                                "Project registry is corrupted: {e}"
                            ))
                        })
                    })
            } else {
                Ok(Vec::new())
            };
            render_and_print("project.list", projects, is_json, ctx.quiet)
        }
        ProjectCommand::Register { path } => {
            // No global registration flow exists yet. Reporting a fake
            // success ("needs_init") made scripts believe a mutation
            // happened; fail honestly instead.
            render_and_print::<serde_json::Value>(
                "project.register",
                Err(CarryCtxError::unsupported_operation(format!(
                    "'project register' is not implemented yet; run 'carryctx init' in '{path}' to initialize and register the project."
                ))),
                is_json,
                ctx.quiet,
            )
        }
        ProjectCommand::Unregister { project_id } => render_and_print::<serde_json::Value>(
            "project.unregister",
            Err(CarryCtxError::unsupported_operation(format!(
                "'project unregister' is not implemented yet; project '{project_id}' was not removed."
            ))),
            is_json,
            ctx.quiet,
        ),
        ProjectCommand::Migrate => {
            let mut runtime = match pre_opened.take() {
                Some(runtime) => runtime,
                None => open_runtime_or_report(ctx, "project.migrate")?,
            };
            let result = runtime.database.migrate().map(|applied| {
                serde_json::json!({
                    "appliedMigrations": applied.iter().map(|m| m.name.clone()).collect::<Vec<_>>()
                })
            });
            render_and_print("project.migrate", result, is_json, ctx.quiet)
        }
        ProjectCommand::Backup => {
            let mut runtime = match pre_opened.take() {
                Some(runtime) => runtime,
                None => open_runtime_or_report(ctx, "project.backup")?,
            };
            let uow = runtime
                .database
                .begin_unit_of_work()
                .map_err(|e| e.exit_code)?;
            // A failed commit (BUSY, disk full) must fail the command
            // instead of reporting success for unpersisted writes.
            let result = carryctx::application::project_mgmt::backup_project(
                &runtime.git_project.repository_root,
                &uow,
            )
            .and_then(|backup_path| uow.commit().map(|()| backup_path));
            render_and_print("project.backup", result, is_json, ctx.quiet)
        }
        ProjectCommand::Restore { path } => {
            let result = carryctx::application::project_mgmt::restore_project(
                Path::new(path),
                resolve_work_dir(ctx),
            );
            render_and_print("project.restore", result, is_json, ctx.quiet)
        }
        ProjectCommand::Prune { older_than_days } => {
            let mut runtime = match pre_opened.take() {
                Some(runtime) => runtime,
                None => open_runtime_or_report(ctx, "project.prune")?,
            };
            let archive_path = runtime.xdg.archive_db(&runtime.git_project.git_common_dir);
            // Foreign keys stay enabled: the schema cascades and the
            // prune ordering delete children before parents, so no
            // PRAGMA foreign_keys=OFF window is needed.
            let result = {
                let uow = runtime
                    .database
                    .begin_unit_of_work()
                    .map_err(|e| e.exit_code)?;
                carryctx::application::project_mgmt::prune_project(
                    *older_than_days,
                    Some(&archive_path),
                    &uow,
                )
                .and_then(|value| uow.commit().map(|()| value))
            };
            render_and_print("project.prune", result, is_json, ctx.quiet)
        }
    }
}
