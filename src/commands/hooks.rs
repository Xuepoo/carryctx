use crate::*;
use carryctx::application::runtime::InvocationContext;
use carryctx::error::ExitCode;
use clap::{Parser, Subcommand};
use std::fs;

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
# Creates a checkpoint for the current commit, if a task is active.
if command -v carryctx >/dev/null 2>&1; then
    COMMIT=$(git rev-parse HEAD 2>/dev/null)
    TASK_ID=$(carryctx context --format json 2>/dev/null | grep -o '"display_id":"[^"]*"' | head -1 | cut -d'"' -f4)
    if [ -n "$TASK_ID" ]; then
        carryctx checkpoint --task "$TASK_ID" --note "Auto-checkpoint after commit $COMMIT" --quiet 2>/dev/null || true
    fi
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
    TASK_ID=$(carryctx context --format json 2>/dev/null | grep -o '"display_id":"[^"]*"' | head -1 | cut -d'"' -f4)
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

/// Report a hooks failure through the standard error path in JSON mode (error
/// envelope on stderr) while keeping the historical human-readable line and
/// exit code untouched in text mode.
fn report_hooks_error(
    command: &str,
    error: carryctx::error::CarryCtxError,
    text_line: &str,
    exit_code: ExitCode,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    if is_json {
        crate::render_and_print::<serde_json::Value>(command, Err(error), true, ctx.quiet)
    } else {
        if !ctx.quiet {
            eprintln!("{text_line}");
        }
        Err(exit_code)
    }
}

/// Emit the success envelope for install/uninstall under `--json`; the caller
/// has already printed the human-readable lines in text mode.
fn render_hooks_success(
    command: &str,
    data: serde_json::Value,
    is_json: bool,
    quiet: bool,
) -> Result<ExitCode, ExitCode> {
    if is_json {
        let result: Result<serde_json::Value, carryctx::error::CarryCtxError> = Ok(data);
        render_and_print(command, result, true, quiet)
    } else {
        Ok(ExitCode::Success)
    }
}

pub fn handle_hooks(
    args: &HooksArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    match &args.subcommand {
        HooksCommand::Install(a) => handle_hooks_install(a, ctx, is_json),
        HooksCommand::Uninstall(a) => handle_hooks_uninstall(a, ctx, is_json),
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

/// Text-mode explanation for the jj-colocation refusal (unchanged wording).
const JJ_COLOCATION_TEXT: &str = concat!(
    "This repository is jj-colocated (.jj/ alongside .git/). CarryCtx's git hooks ",
    "(post-commit, prepare-commit-msg) auto-checkpoint on `git commit`, but jj writes ",
    "commits directly to the Git object store via `jj git export` and never runs ",
    "`git commit` or any Git hook — installing them here would silently never fire. ",
    "Run `carryctx checkpoint` manually after `jj commit`/`jj describe` instead. See ",
    "carryctx-docs/plans/2026-07-25-jujutsu-compatibility.md."
);

fn handle_hooks_install(
    args: &HooksInstallArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    use carryctx::adapter::git::{GitCli, detect_jj_colocation};
    let work_dir = resolve_work_dir(ctx);
    let git = GitCli::new();
    let gp = match git.discover(work_dir) {
        Ok(gp) => gp,
        Err(e) => {
            return report_hooks_error(
                "hooks.install",
                e,
                &format!("Error [{}]: not a Git repository", ExitCode::Git as i32),
                ExitCode::General,
                ctx,
                is_json,
            );
        }
    };
    if detect_jj_colocation(&gp.git_common_dir) {
        return report_hooks_error(
            "hooks.install",
            carryctx::error::CarryCtxError::validation_error(
                "This repository is jj-colocated (.jj/ alongside .git/); CarryCtx git hooks would never fire under jj. Run `carryctx checkpoint` manually after `jj commit`/`jj describe` instead.",
            ),
            JJ_COLOCATION_TEXT,
            ExitCode::Validation,
            ctx,
            is_json,
        );
    }
    let hooks_dir = gp.git_common_dir.join("hooks");
    if let Err(e) = fs::create_dir_all(&hooks_dir) {
        return report_hooks_error(
            "hooks.install",
            carryctx::error::CarryCtxError::io_error(format!("Failed to create hooks dir: {e}")),
            &format!("Failed to create hooks dir: {e}"),
            ExitCode::General,
            ctx,
            is_json,
        );
    }

    let to_install: &[(&str, &str)] = if args.post_commit_only {
        &[("post-commit", POST_COMMIT_HOOK)]
    } else {
        &[
            ("post-commit", POST_COMMIT_HOOK),
            ("prepare-commit-msg", PREPARE_COMMIT_MSG_HOOK),
        ]
    };

    let mut installed: Vec<String> = Vec::new();
    for (name, content) in to_install {
        let path = hooks_dir.join(name);
        if path.exists() && !args.force {
            return report_hooks_error(
                "hooks.install",
                carryctx::error::CarryCtxError::new(
                    "HOOK_EXISTS",
                    format!("Hook '{name}' already exists. Use --force to overwrite."),
                    ExitCode::General,
                ),
                &format!("Hook '{name}' already exists. Use --force to overwrite."),
                ExitCode::General,
                ctx,
                is_json,
            );
        }
        if path.exists() && args.force {
            let bak = hooks_dir.join(format!("{name}.bak"));
            fs::rename(&path, &bak).ok();
        }
        // Install atomically (tmp file + rename): a `git commit` racing the
        // install must never execute a truncated or partially written hook.
        if let Err(e) = carryctx::adapter::filesystem::write_atomic(&path, content.as_bytes()) {
            return report_hooks_error(
                "hooks.install",
                carryctx::error::CarryCtxError::io_error(format!(
                    "Failed to write hook {name}: {e}"
                )),
                &format!("Failed to write hook {name}: {e}"),
                ExitCode::General,
                ctx,
                is_json,
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Fail loudly: a non-executable hook would silently never run.
            if let Err(e) = fs::set_permissions(&path, fs::Permissions::from_mode(0o755)) {
                return report_hooks_error(
                    "hooks.install",
                    carryctx::error::CarryCtxError::io_error(format!(
                        "Failed to make hook {name} executable: {e}"
                    )),
                    &format!("Failed to make hook {name} executable: {e}"),
                    ExitCode::General,
                    ctx,
                    is_json,
                );
            }
        }
        if !ctx.quiet && !is_json {
            println!("✓ Installed hook: {name}");
        }
        installed.push((*name).to_string());
    }
    render_hooks_success(
        "hooks.install",
        serde_json::json!({
            "installed": installed,
            "hooksDir": hooks_dir.display().to_string(),
        }),
        is_json,
        ctx.quiet,
    )
}

fn handle_hooks_uninstall(
    args: &HooksUninstallArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    let hooks_dir = match git_hooks_dir(ctx) {
        Ok(dir) => dir,
        Err(code) => {
            return report_hooks_error(
                "hooks.uninstall",
                carryctx::error::CarryCtxError::git_error("Not inside a Git repository."),
                &format!("Error [{}]: not a Git repository", code as i32),
                code,
                ctx,
                is_json,
            );
        }
    };
    let mut removed: Vec<String> = Vec::new();
    for name in &["post-commit", "prepare-commit-msg"] {
        let path = hooks_dir.join(name);
        if path.exists() {
            // Only remove hooks we own (check for our marker comment)
            let content = fs::read_to_string(&path).unwrap_or_default();
            if !content.contains("CarryCtx") {
                if !ctx.quiet && !is_json {
                    println!("Skipping '{name}' — not a CarryCtx hook.");
                }
                continue;
            }
            if args.restore {
                let bak = hooks_dir.join(format!("{name}.bak"));
                if bak.exists() {
                    fs::rename(&bak, &path).ok();
                    if !ctx.quiet && !is_json {
                        println!("✓ Restored original hook: {name}");
                    }
                    continue;
                }
            }
            if let Err(e) = fs::remove_file(&path) {
                return report_hooks_error(
                    "hooks.uninstall",
                    carryctx::error::CarryCtxError::io_error(format!(
                        "Failed to remove hook {name}: {e}"
                    )),
                    &format!("Failed to remove hook {name}: {e}"),
                    ExitCode::General,
                    ctx,
                    is_json,
                );
            }
            if !ctx.quiet && !is_json {
                println!("✓ Removed hook: {name}");
            }
            removed.push((*name).to_string());
        }
    }
    render_hooks_success(
        "hooks.uninstall",
        serde_json::json!({ "removed": removed }),
        is_json,
        ctx.quiet,
    )
}

fn handle_hooks_status(
    args: &HooksStatusArgs,
    ctx: &InvocationContext,
) -> Result<ExitCode, ExitCode> {
    let hooks_dir = git_hooks_dir(ctx)?;

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
    render_and_print("hooks.status", err_result, args.json, ctx.quiet)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hook templates extract the active task id from
    /// `carryctx context --format json` with a grep for the raw JSON key.
    /// `TaskRecord` serializes snake_case, so the templates must grep
    /// `"display_id"` — the old camelCase `"displayId"` never matched.
    /// The templates also must not pass `--quiet`: quiet suppresses stdout
    /// entirely, leaving the grep with nothing to match.
    #[test]
    fn hook_templates_grep_snake_case_display_id() {
        for template in [POST_COMMIT_HOOK, PREPARE_COMMIT_MSG_HOOK] {
            assert!(
                template.contains(r#""display_id":"[^"]*""#),
                "template must grep the snake_case display_id key"
            );
            assert!(
                !template.contains("displayId"),
                "template must not grep the camelCase key that never matches"
            );
            let context_line = template
                .lines()
                .find(|l| l.contains("carryctx context"))
                .expect("template must invoke carryctx context");
            assert!(
                !context_line.contains("--quiet"),
                "quiet mode suppresses context JSON output, starving the grep"
            );
        }
    }
}
