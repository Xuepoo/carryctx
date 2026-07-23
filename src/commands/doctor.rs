use crate::*;
use carryctx::adapter::git::GitCli;
use carryctx::adapter::sqlite_repos::{
    SqliteAgentRepository, SqliteSessionRepository, SqliteTaskRepository,
};
use carryctx::adapter::xdg::XdgPaths;
use carryctx::application::runtime::InvocationContext;
use carryctx::domain::session::SessionState;
use carryctx::domain::task::TaskStatus;
use carryctx::error::{CarryCtxError, ExitCode};
use carryctx::repository::{AgentRepository, SessionRepository, TaskFilter, TaskRepository};
use clap::Parser;

// ── Doctor ───────────────────────────────────────────────────────────────

/// Diagnose and automatically fix potential issues with the project's SQLite state database.
///
/// Checks Git repository health, database connectivity, schema version, orphaned
/// tasks (tasks with non-existent owners), stale active sessions, and git hook
/// installation status.
#[derive(Parser, Debug)]
pub struct DoctorArgs {
    /// Automatically attempt to fix detected anomalies in the database and configuration.
    #[arg(long)]
    pub fix: bool,

    /// Output the diagnostic results in JSON format.
    #[arg(long)]
    pub json: bool,
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: doctor
// ═══════════════════════════════════════════════════════════════════════════

pub fn handle_doctor(
    args: &DoctorArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    let mut checks: Vec<serde_json::Value> = Vec::new();
    let mut all_ok = true;

    // ── 1. Global config ─────────────────────────────────────────────────
    let xdg = XdgPaths::new();
    let global_config = xdg.global_config();
    if global_config.exists() {
        match std::fs::read_to_string(&global_config) {
            Ok(content) => {
                match toml::from_str::<carryctx::domain::config::CarryCtxConfig>(&content) {
                    Ok(_) => checks.push(serde_json::json!({
                        "check": "config.global",
                        "status": "ok",
                        "message": "Global config is valid"
                    })),
                    Err(e) => {
                        all_ok = false;
                        checks.push(serde_json::json!({
                            "check": "config.global",
                            "status": "error",
                            "message": format!("Invalid global config: {e}"),
                            "repairable": false
                        }));
                    }
                }
            }
            Err(e) => {
                checks.push(serde_json::json!({
                    "check": "config.global",
                    "status": "warning",
                    "message": format!("Cannot read global config: {e}"),
                    "repairable": false
                }));
            }
        }
    } else {
        checks.push(serde_json::json!({
            "check": "config.global",
            "status": "info",
            "message": "No global config found (using defaults)"
        }));
    }

    // ── 2. Git repository ─────────────────────────────────────────────────
    let work_dir = resolve_work_dir(ctx);
    let git = GitCli::new();
    let git_project = match git.discover(work_dir) {
        Ok(gp) => {
            checks.push(serde_json::json!({
                "check": "git.repository",
                "status": "ok",
                "message": format!("Git repository at {}", gp.repository_root.display())
            }));
            Some(gp)
        }
        Err(e) => {
            all_ok = false;
            checks.push(serde_json::json!({
                "check": "git.repository",
                "status": "error",
                "message": format!("{e}"),
                "repairable": false
            }));
            None
        }
    };

    // ── 3. Git hooks ──────────────────────────────────────────────────────
    if let Some(ref gp) = git_project {
        let hooks_dir = gp.git_common_dir.join("hooks");
        let managed_hooks: Vec<&str> = ["post-commit", "prepare-commit-msg"]
            .iter()
            .filter(|&&name| {
                let p = hooks_dir.join(name);
                if !p.exists() {
                    return false;
                }
                std::fs::read_to_string(p)
                    .unwrap_or_default()
                    .contains("CarryCtx")
            })
            .copied()
            .collect();

        if managed_hooks.is_empty() {
            checks.push(serde_json::json!({
                "check": "git.hooks",
                "status": "info",
                "message": "No CarryCtx git hooks installed. Run `carryctx hooks install` to enable auto-checkpoint on commit.",
                "fix_command": "carryctx hooks install"
            }));
        } else {
            checks.push(serde_json::json!({
                "check": "git.hooks",
                "status": "ok",
                "message": format!("CarryCtx hooks installed: {}", managed_hooks.join(", "))
            }));
        }
    }

    // ── 4. Database connection + schema ───────────────────────────────────
    let runtime = match try_open_runtime(ctx) {
        Ok(rt) => {
            checks.push(serde_json::json!({
                "check": "database.connection",
                "status": "ok",
                "message": format!("Database at {}", rt.db_path.display())
            }));
            checks.push(serde_json::json!({
                "check": "database.schema",
                "status": "ok",
                "message": "Schema version up to date"
            }));
            Some(rt)
        }
        Err(exit_code) => {
            all_ok = false;
            let msg = match exit_code {
                ExitCode::Database => {
                    "Database connection failed — try `carryctx init` to reinitialise"
                }
                ExitCode::Git => "Not in a Git repository",
                _ => "Cannot open project (not initialised? Run `carryctx init`)",
            };
            checks.push(serde_json::json!({
                "check": "database.connection",
                "status": "error",
                "message": msg,
                "repairable": true,
                "fix_command": "carryctx init"
            }));
            None
        }
    };

    // ── 5. Orphaned tasks + in-progress state ──────────────────────────────
    if let Some(ref rt) = runtime {
        let conn = rt.database.connection();
        let project_id = &rt.config.project.id;
        let task_repo = SqliteTaskRepository::new(conn);
        let agent_repo = SqliteAgentRepository::new(conn);

        let filter = TaskFilter {
            project_id: project_id.to_string(),
            status: None,
            owner_agent_id: None,
            ready: false,
            blocked: false,
            mine: None,
        };

        match task_repo.list(&filter) {
            Ok(tasks) => {
                let mut orphaned: Vec<String> = Vec::new();
                for task in &tasks {
                    if let Some(owner_id) = &task.owner_agent_id {
                        if agent_repo
                            .find_by_id(project_id, owner_id)
                            .ok()
                            .flatten()
                            .is_none()
                        {
                            orphaned.push(format!("{} ({})", task.display_id, task.title));
                        }
                    }
                }
                if orphaned.is_empty() {
                    checks.push(serde_json::json!({
                        "check": "tasks.orphaned",
                        "status": "ok",
                        "message": "No orphaned tasks (all owners exist)"
                    }));
                } else {
                    all_ok = false;
                    checks.push(serde_json::json!({
                        "check": "tasks.orphaned",
                        "status": "warning",
                        "message": format!("{} task(s) have deleted owners: {}", orphaned.len(), orphaned.join(", ")),
                        "note": "Use `carryctx task unclaim <id>` to release ownership"
                    }));
                }

                let in_progress: Vec<_> = tasks
                    .iter()
                    .filter(|t| t.status == TaskStatus::InProgress)
                    .collect();
                if !in_progress.is_empty() {
                    checks.push(serde_json::json!({
                        "check": "tasks.in_progress",
                        "status": "info",
                        "message": format!("{} task(s) currently in progress", in_progress.len()),
                        "tasks": in_progress.iter().map(|t| t.display_id.as_str()).collect::<Vec<_>>()
                    }));
                }
            }
            Err(e) => {
                checks.push(serde_json::json!({
                    "check": "tasks.orphaned",
                    "status": "warning",
                    "message": format!("Could not check tasks: {e}")
                }));
            }
        }

        // ── 6. Active sessions ──────────────────────────────────────────────
        let session_repo = SqliteSessionRepository::new(conn);
        match session_repo.list(project_id) {
            Ok(sessions) => {
                let active: Vec<_> = sessions
                    .iter()
                    .filter(|s| matches!(s.state, SessionState::Active))
                    .collect();
                if !active.is_empty() {
                    checks.push(serde_json::json!({
                        "check": "sessions.active",
                        "status": "ok",
                        "message": format!("{} active session(s)", active.len())
                    }));
                } else {
                    checks.push(serde_json::json!({
                        "check": "sessions.active",
                        "status": "info",
                        "message": "No active session. Run `carryctx session start` to begin."
                    }));
                }
            }
            Err(e) => {
                checks.push(serde_json::json!({
                    "check": "sessions.active",
                    "status": "warning",
                    "message": format!("Could not check sessions: {e}")
                }));
            }
        }
    }

    // ── Output ────────────────────────────────────────────────────────────
    let summary = if all_ok { "healthy" } else { "issues_found" };
    let result = serde_json::json!({
        "summary": summary,
        "checks": checks,
        "fix_requested": args.fix,
        "allOk": all_ok,
    });

    let exit_code = if all_ok {
        ExitCode::Success
    } else {
        ExitCode::General
    };

    if !is_json && !args.json && !ctx.quiet {
        println!("CarryCtx Doctor\n");
        for check in result["checks"].as_array().unwrap() {
            let status = check["status"].as_str().unwrap_or("?");
            let message = check["message"].as_str().unwrap_or("");
            let icon = match status {
                "ok" => "✓",
                "error" => "✗",
                "warning" => "⚠",
                _ => "·",
            };
            println!("  {icon} {message}");
            if let Some(fix_cmd) = check["fix_command"].as_str() {
                println!("      → Fix: {fix_cmd}");
            }
        }
        println!();
        if all_ok {
            println!("Everything looks good!");
        } else {
            println!("Issues detected. Some may be fixed with `carryctx doctor --fix`.");
        }
        return Ok(exit_code);
    }

    let err_result: Result<serde_json::Value, CarryCtxError> = Ok(result);
    let _ = render_and_print("doctor", err_result, is_json || args.json, ctx.quiet);
    Ok(exit_code)
}
