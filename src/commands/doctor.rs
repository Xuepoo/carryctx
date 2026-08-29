use crate::*;
use carryctx::adapter::git::GitCli;
use carryctx::adapter::sqlite_repos::{
    SqliteSessionRepository, SqliteTaskRepository, SqliteWorktreeRepository,
};
use carryctx::adapter::xdg::XdgPaths;
use carryctx::application::runtime::{InvocationContext, ProjectRuntime};
use carryctx::domain::session::SessionState;
use carryctx::domain::task::TaskStatus;
use carryctx::error::{CarryCtxError, ExitCode};
use carryctx::repository::{SessionRepository, WorktreeRepository};
use clap::Parser;

// ── Doctor ───────────────────────────────────────────────────────────────

/// Diagnose and automatically fix potential issues with the project's SQLite state database.
///
/// Checks Git repository health, database connectivity, schema version, orphaned
/// tasks (tasks with non-existent owners), stale active sessions, and git hook
/// installation status.
///
/// Exit codes reflect finding severity (CTX-0083): exit 0 when findings are
/// info/warning only, exit 1 when any check reports an error/critical
/// finding or diagnostics could not run. The rendered report is identical
/// either way — warnings still carry their status and fix hints.
#[derive(Parser, Debug)]
pub struct DoctorArgs {
    /// Automatically attempt to fix detected anomalies in the database and configuration.
    #[arg(long)]
    pub fix: bool,

    /// Remove registered worktrees whose directories are missing. This never deletes files.
    #[arg(long)]
    pub prune_stale_worktrees: bool,

    /// Output the diagnostic results in JSON format.
    #[arg(long)]
    pub json: bool,
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: doctor
// ═══════════════════════════════════════════════════════════════════════════

pub fn handle_doctor(
    args: &DoctorArgs,
    pre_opened: Option<ProjectRuntime>,
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

    // ── 3b. Jujutsu (jj) colocation ─────────────────────────────────────────
    if let Some(gp) = &git_project {
        if carryctx::adapter::git::detect_jj_colocation(&gp.git_common_dir) {
            checks.push(serde_json::json!({
                "check": "vcs.jj_colocation",
                "status": "info",
                "message": "jj colocation detected (.jj/ alongside .git/). CarryCtx reads Git state directly; some data (e.g. checkpoint staged/unstaged split) may be less precise under jj. See carryctx-docs/plans/2026-07-25-jujutsu-compatibility.md."
            }));
        }
    }

    // ── 4. Database connection + schema ───────────────────────────────────
    // Reuse the dispatcher's pre-opened runtime when available; a fresh open
    // only happens when that failed. Doctor keeps its own diagnostic
    // envelope: an open failure is reported as a failed `database.connection`
    // check inside the report, not as a global error.
    let opened = match pre_opened {
        Some(runtime) => Ok(runtime),
        None => try_open_runtime(ctx),
    };
    let runtime = match opened {
        Ok(rt) => {
            checks.push(serde_json::json!({
                "check": "database.connection",
                "status": "ok",
                "message": format!("Database at {}", rt.db_path.display())
            }));
            let pending = rt.database.pending_migrations().unwrap_or_default();
            if pending.is_empty() {
                checks.push(serde_json::json!({
                    "check": "database.schema",
                    "status": "ok",
                    "message": "Schema version up to date"
                }));
            } else {
                all_ok = false;
                checks.push(serde_json::json!({
                    "check": "database.schema",
                    "status": "error",
                    "message": format!(
                        "{} pending migration(s) not applied: {}",
                        pending.len(),
                        pending.iter().map(|m| m.name.as_str()).collect::<Vec<_>>().join(", ")
                    ),
                    "repairable": true,
                    "fix_command": "carryctx project migrate"
                }));
            }
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
    // Diagnostics must see every task, so they read through exact
    // COUNT(*)-style queries instead of the capped listing (CTX-0080).
    if let Some(ref rt) = runtime {
        let conn = rt.database.connection();
        let project_id = &rt.config.project.id;
        let repository_root = &rt.git_project.repository_root;
        let task_repo = SqliteTaskRepository::new(conn);
        let worktree_repo = SqliteWorktreeRepository::new(conn);
        let cleanup_repo = carryctx::adapter::sqlite_repos::SqliteCleanupRepository::new(conn);

        match task_repo.list_orphaned_owner_refs(project_id) {
            Ok(orphaned) => {
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
                        "message": format!(
                            "{} task(s) have deleted owners: {}",
                            orphaned.len(),
                            orphaned
                                .iter()
                                .map(|(display_id, title)| format!("{display_id} ({title})"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                        "note": "Use `carryctx task unclaim <id>` to release ownership"
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

        match task_repo.count_by_status(project_id, &TaskStatus::InProgress) {
            Ok(in_progress_count) if in_progress_count > 0 => {
                let display_ids = task_repo
                    .list_display_ids_by_status(project_id, &TaskStatus::InProgress)
                    .unwrap_or_default();
                checks.push(serde_json::json!({
                    "check": "tasks.in_progress",
                    "status": "info",
                    "message": format!("{} task(s) currently in progress", in_progress_count),
                    "tasks": display_ids
                }));
            }
            Ok(_) => {}
            Err(e) => {
                checks.push(serde_json::json!({
                    "check": "tasks.in_progress",
                    "status": "warning",
                    "message": format!("Could not check in-progress tasks: {e}")
                }));
            }
        }

        // ── 6. Active sessions ──────────────────────────────────────────────
        let session_repo = SqliteSessionRepository::new(conn);
        let audit_session_id = ctx.session.clone().or_else(|| {
            session_repo
                .list(project_id)
                .ok()?
                .into_iter()
                .find(|session| matches!(session.state, SessionState::Active))
                .map(|session| session.id)
        });
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

        match cleanup_repo.list(project_id, None) {
            Ok(requests) => {
                let outstanding: Vec<_> = requests
                    .iter()
                    .filter(|request| {
                        matches!(
                            request.state,
                            carryctx::domain::cleanup::CleanupState::Pending
                                | carryctx::domain::cleanup::CleanupState::Blocked
                                | carryctx::domain::cleanup::CleanupState::Failed
                        )
                    })
                    .collect();
                if outstanding.is_empty() {
                    checks.push(serde_json::json!({
                        "check": "worktrees.cleanup",
                        "status": "ok",
                        "message": "No pending or failed worktree cleanups"
                    }));
                } else {
                    checks.push(serde_json::json!({
                        "check": "worktrees.cleanup",
                        "status": "warning",
                        "message": format!("{} worktree cleanup request(s) require attention", outstanding.len()),
                        "requests": outstanding.iter().map(|request| serde_json::json!({
                            "id": request.id,
                            "status": request.state,
                            "reason": request.reason,
                            "attempt_count": request.attempt_count,
                            "blocked_reason": request.blocked_reason,
                        })).collect::<Vec<_>>(),
                        "fix_command": "carryctx worktree cleanup run"
                    }));
                }
            }
            Err(e) => checks.push(serde_json::json!({
                "check": "worktrees.cleanup",
                "status": "warning",
                "message": format!("Could not check worktree cleanups: {e}")
            })),
        }

        if args.prune_stale_worktrees && ctx.dry_run {
            // Detection remains read-only; dry-run reports the same plan without writing.
        } else if args.prune_stale_worktrees && !ctx.yes {
            return render_and_print::<serde_json::Value>(
                "doctor",
                Err(CarryCtxError::permission_scope(
                    "Pruning stale worktrees requires explicit confirmation with --yes.",
                )),
                is_json || args.json,
                ctx.quiet,
            );
        }
        let stale_result = if args.prune_stale_worktrees && !ctx.dry_run {
            // An unresolvable actor must render the standard error envelope,
            // not collapse to a bare exit code with no output.
            let actor = match ctx
                .agent
                .as_deref()
                .map(|agent| resolve_agent_id(project_id, agent, conn))
                .transpose()
            {
                Ok(actor) => actor,
                Err(error) => {
                    return render_and_print::<serde_json::Value>(
                        "doctor",
                        Err(error),
                        is_json || args.json,
                        ctx.quiet,
                    );
                }
            };
            worktree_repo.prune_stale(
                project_id,
                repository_root,
                actor.as_deref(),
                audit_session_id.as_deref(),
                &chrono::Utc::now().to_rfc3339(),
            )
        } else {
            carryctx::application::worktree::stale_worktrees(
                &worktree_repo,
                project_id,
                repository_root,
            )
        };
        match stale_result {
            Ok(stale) if stale.is_empty() => checks.push(serde_json::json!({
                "check": "worktrees.stale",
                "status": "ok",
                "message": "No registered worktrees have missing directories"
            })),
            Ok(stale) => {
                if !args.prune_stale_worktrees {
                    all_ok = false;
                }
                checks.push(serde_json::json!({
                    "check": "worktrees.stale",
                    "status": if args.prune_stale_worktrees && !ctx.dry_run { "ok" } else { "warning" },
                    "message": if args.prune_stale_worktrees && !ctx.dry_run {
                        format!("Pruned {} stale worktree registration(s)", stale.len())
                    } else if args.prune_stale_worktrees {
                        format!("Would prune {} stale worktree registration(s)", stale.len())
                    } else {
                        format!("{} registered worktree(s) point to missing directories", stale.len())
                    },
                    "count": stale.len(),
                    "worktrees": stale.iter().map(|w| &w.path).collect::<Vec<_>>(),
                    "fix_command": "carryctx doctor --prune-stale-worktrees"
                }));
            }
            Err(e) => {
                return render_and_print::<serde_json::Value>(
                    "doctor",
                    Err(e),
                    is_json || args.json,
                    ctx.quiet,
                );
            }
        }
    }

    // ── Output ────────────────────────────────────────────────────────────
    let summary = if all_ok { "healthy" } else { "issues_found" };
    let result = serde_json::json!({
        "summary": summary,
        "checks": checks,
        "fix_requested": args.fix,
        "all_ok": all_ok,
    });

    // CTX-0083: exit codes reflect severity, not the mere presence of
    // findings. Info/warning-only reports (e.g. a stale worktree
    // registration) exit 0 so scripts can distinguish "nothing broken"
    // from real trouble; error/critical findings keep exit 1. The rendered
    // report is unchanged — `all_ok` still summarizes every non-ok check.
    let has_blocking_findings = checks
        .iter()
        .any(|check| matches!(check["status"].as_str(), Some("error") | Some("critical")));
    let exit_code = if has_blocking_findings {
        ExitCode::General
    } else {
        ExitCode::Success
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
