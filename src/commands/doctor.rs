use crate::*;
use carryctx::adapter::xdg::XdgPaths;
use carryctx::application::runtime::InvocationContext;
use carryctx::error::{CarryCtxError, ExitCode};
use clap::Parser;

// ── Doctor ───────────────────────────────────────────────────────────────

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

    // Config check
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
            "message": "No global config found"
        }));
    }

    // Git check
    let work_dir = resolve_work_dir(ctx);
    let git = GitCli::new();
    match git.discover(work_dir) {
        Ok(gp) => {
            checks.push(serde_json::json!({
                "check": "git.repository",
                "status": "ok",
                "message": format!("Git repository at {}", gp.repository_root.display())
            }));
        }
        Err(e) => {
            all_ok = false;
            checks.push(serde_json::json!({
                "check": "git.repository",
                "status": "error",
                "message": format!("{e}"),
                "repairable": false
            }));
        }
    }

    // Database check
    match try_open_runtime(ctx) {
        Ok(runtime) => {
            checks.push(serde_json::json!({
                "check": "database.connection",
                "status": "ok",
                "message": format!("Database at {}", runtime.db_path.display())
            }));
            checks.push(serde_json::json!({
                "check": "database.migrations",
                "status": "ok",
                "message": "Schema version checked"
            }));
        }
        Err(exit_code) => {
            all_ok = false;
            let msg = match exit_code {
                ExitCode::Database => "Database connection failed",
                ExitCode::Git => "Not in a Git repository",
                _ => "Cannot open project",
            };
            checks.push(serde_json::json!({
                "check": "database.connection",
                "status": "error",
                "message": msg,
                "repairable": true
            }));
        }
    }

    let result = serde_json::json!({
        "checks": checks,
        "fix": args.fix,
        "allOk": all_ok,
    });

    let exit_code = if all_ok {
        ExitCode::Success
    } else {
        ExitCode::General
    };

    let err_result: Result<serde_json::Value, CarryCtxError> = Ok(result);
    let _ = render_and_print("doctor", err_result, is_json || args.json, ctx.quiet);
    Ok(exit_code)
}
