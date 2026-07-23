use crate::*;
use carryctx::application::runtime::InvocationContext;
use carryctx::error::ExitCode;
use clap::{Parser, Subcommand};
use std::fs;
use std::os::unix::fs::PermissionsExt;

// ── Hooks ─────────────────────────────────────────────────────────────────

/// Install and manage Git hooks that integrate with CarryCtx.
///
/// Git hooks let CarryCtx automatically capture context on commit,
/// validate task state, and embed checkpoint metadata in commit messages.
#[derive(Parser, Debug)]
pub struct HooksArgs {
    #[command(subcommand)]
    pub subcommand: HooksCommand,
}

#[derive(Subcommand, Debug)]
pub enum HooksCommand {
    /// Install CarryCtx git hooks into the current repository's .git/hooks directory.
    ///
    /// Installs a post-commit hook that auto-records the commit SHA into the
    /// active CarryCtx checkpoint and a prepare-commit-msg hook that prepends
    /// the active task ID to every commit message.
    Install(HooksInstallArgs),
    /// Remove all CarryCtx-managed git hooks from the repository.
    Uninstall(HooksUninstallArgs),
    /// Show which CarryCtx hooks are currently installed and their status.
    Status(HooksStatusArgs),
}

#[derive(Parser, Debug)]
pub struct HooksInstallArgs {
    /// Only install the post-commit hook (skip prepare-commit-msg).
    #[arg(long)]
    pub post_commit_only: bool,
    /// Overwrite existing hooks if they already exist (backs up originals with .bak).
    #[arg(long)]
    pub force: bool,
}

#[derive(Parser, Debug)]
pub struct HooksUninstallArgs {
    /// Restore original hooks from .bak backups if present.
    #[arg(long)]
    pub restore: bool,
}

#[derive(Parser, Debug)]
pub struct HooksStatusArgs {
    /// Output in JSON format.
    #[arg(long)]
    pub json: bool,
}

const POST_COMMIT_HOOK: &str = r#"#!/bin/sh
# CarryCtx post-commit hook
# Records the latest commit SHA into the active checkpoint.
if command -v carryctx >/dev/null 2>&1; then
    COMMIT=$(git rev-parse HEAD 2>/dev/null)
    carryctx checkpoint --note "Auto-checkpoint after commit $COMMIT" --quiet 2>/dev/null || true
fi
"#;

const PREPARE_COMMIT_MSG_HOOK: &str = r#"#!/bin/sh
# CarryCtx prepare-commit-msg hook
# Prepends the active task ID to the commit message.
COMMIT_MSG_FILE=$1
COMMIT_SOURCE=$2

if [ "$COMMIT_SOURCE" = "merge" ] || [ "$COMMIT_SOURCE" = "squash" ]; then
    exit 0
fi

if command -v carryctx >/dev/null 2>&1; then
    TASK_ID=$(carryctx context --quiet --format json 2>/dev/null | grep -o '"displayId":"[^"]*"' | head -1 | cut -d'"' -f4)
    if [ -n "$TASK_ID" ]; then
        ORIG=$(cat "$COMMIT_MSG_FILE")
        # Only prepend if not already present
        if ! echo "$ORIG" | grep -q "^\[$TASK_ID\]"; then
            printf '[%s] %s' "$TASK_ID" "$ORIG" > "$COMMIT_MSG_FILE"
        fi
    fi
fi
"#;

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: hooks
// ═══════════════════════════════════════════════════════════════════════════

pub fn handle_hooks(
    args: &HooksArgs,
    ctx: &InvocationContext,
    _is_json: bool,
) -> Result<ExitCode, ExitCode> {
    match &args.subcommand {
        HooksCommand::Install(a) => handle_hooks_install(a, ctx),
        HooksCommand::Uninstall(a) => handle_hooks_uninstall(a, ctx),
        HooksCommand::Status(a) => handle_hooks_status(a, ctx),
    }
}

fn git_hooks_dir(ctx: &InvocationContext) -> Result<std::path::PathBuf, ExitCode> {
    use carryctx::adapter::git::GitCli;
    let work_dir = resolve_work_dir(ctx);
    let git = GitCli::new();
    let gp = git.discover(work_dir).map_err(|e| e.exit_code)?;
    Ok(gp.git_common_dir.join("hooks"))
}

fn handle_hooks_install(
    args: &HooksInstallArgs,
    ctx: &InvocationContext,
) -> Result<ExitCode, ExitCode> {
    let hooks_dir = git_hooks_dir(ctx)?;
    fs::create_dir_all(&hooks_dir).map_err(|e| {
        eprintln!("Failed to create hooks dir: {e}");
        ExitCode::General
    })?;

    let to_install: &[(&str, &str)] = if args.post_commit_only {
        &[("post-commit", POST_COMMIT_HOOK)]
    } else {
        &[
            ("post-commit", POST_COMMIT_HOOK),
            ("prepare-commit-msg", PREPARE_COMMIT_MSG_HOOK),
        ]
    };

    for (name, content) in to_install {
        let path = hooks_dir.join(name);
        if path.exists() && !args.force {
            eprintln!("Hook '{}' already exists. Use --force to overwrite.", name);
            return Err(ExitCode::General);
        }
        if path.exists() && args.force {
            let bak = hooks_dir.join(format!("{name}.bak"));
            fs::rename(&path, &bak).ok();
        }
        fs::write(&path, content).map_err(|e| {
            eprintln!("Failed to write hook {name}: {e}");
            ExitCode::General
        })?;
        // chmod +x
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).ok();
        if !ctx.quiet {
            println!("✓ Installed hook: {name}");
        }
    }
    Ok(ExitCode::Success)
}

fn handle_hooks_uninstall(
    args: &HooksUninstallArgs,
    ctx: &InvocationContext,
) -> Result<ExitCode, ExitCode> {
    let hooks_dir = git_hooks_dir(ctx)?;
    for name in &["post-commit", "prepare-commit-msg"] {
        let path = hooks_dir.join(name);
        if path.exists() {
            // Only remove hooks we own (check for our marker comment)
            let content = fs::read_to_string(&path).unwrap_or_default();
            if !content.contains("CarryCtx") {
                if !ctx.quiet {
                    println!("Skipping '{name}' — not a CarryCtx hook.");
                }
                continue;
            }
            if args.restore {
                let bak = hooks_dir.join(format!("{name}.bak"));
                if bak.exists() {
                    fs::rename(&bak, &path).ok();
                    if !ctx.quiet {
                        println!("✓ Restored original hook: {name}");
                    }
                    continue;
                }
            }
            fs::remove_file(&path).map_err(|e| {
                eprintln!("Failed to remove hook {name}: {e}");
                ExitCode::General
            })?;
            if !ctx.quiet {
                println!("✓ Removed hook: {name}");
            }
        }
    }
    Ok(ExitCode::Success)
}

fn handle_hooks_status(
    args: &HooksStatusArgs,
    ctx: &InvocationContext,
) -> Result<ExitCode, ExitCode> {
    let hooks_dir = match git_hooks_dir(ctx) {
        Ok(d) => d,
        Err(e) => return Err(e),
    };

    let hook_names = ["post-commit", "prepare-commit-msg"];
    let mut statuses = Vec::new();

    for name in &hook_names {
        let path = hooks_dir.join(name);
        let (installed, managed) = if path.exists() {
            let content = fs::read_to_string(&path).unwrap_or_default();
            (true, content.contains("CarryCtx"))
        } else {
            (false, false)
        };
        statuses.push(serde_json::json!({
            "hook": name,
            "installed": installed,
            "managed_by_carryctx": managed,
            "path": path.display().to_string(),
        }));
    }

    let result = serde_json::json!({ "hooks": statuses });
    let err_result: Result<serde_json::Value, carryctx::error::CarryCtxError> = Ok(result);
    render_and_print("hooks status", err_result, args.json, ctx.quiet)
}
