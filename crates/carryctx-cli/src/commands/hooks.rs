use crate::application::runtime::InvocationContext;
use crate::cli::{render_and_print, resolve_work_dir};
use crate::error::ExitCode;
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
    /// Dispatch a Git hook event via Rust (thin shim target).
    ///
    /// Internal plumbing for the thin shim installed by `hooks install`.
    /// Direct invocation is supported for debugging; `--json` emits the standard envelope.
    Dispatch(HooksDispatchArgs),
}

#[derive(Parser, Debug)]
pub struct HooksInstallArgs {
    /// Only install the post-commit hook (skip prepare-commit-msg).
    #[arg(long)]
    pub post_commit_only: bool,
    /// Overwrite existing hooks if they already exist (backs up originals with .bak).
    #[arg(long)]
    pub force: bool,
    /// Compose with an existing foreign hook instead of overwriting (foreign-first chaining).
    #[arg(long)]
    pub compose: bool,
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

#[derive(Parser, Debug)]
pub struct HooksDispatchArgs {
    /// Event to dispatch (git.post-commit, git.prepare-commit-msg).
    pub event: String,
    /// Additional arguments forwarded from the git hook (e.g. commit-msg file).
    #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
    pub args: Vec<String>,
    /// Output in JSON format.
    #[arg(long)]
    pub json: bool,
}

const POST_COMMIT_SHIM: &str = r#"#!/bin/sh
# CarryCtx post-commit shim (managed by `carryctx hooks`)
# Thin shim: all logic lives in `carryctx hooks dispatch git.post-commit`.
exec carryctx hooks dispatch git.post-commit "$@"
"#;

const PREPARE_COMMIT_MSG_SHIM: &str = r#"#!/bin/sh
# CarryCtx prepare-commit-msg shim (managed by `carryctx hooks`)
# Thin shim: all logic lives in `carryctx hooks dispatch git.prepare-commit-msg`.
exec carryctx hooks dispatch git.prepare-commit-msg "$@"
"#;

// Legacy fat hooks (kept for migration detection and tests that guarded the
// display_id grep). Install now writes shims; legacy content is only used to
// classify existing installations for status/composition.
const LEGACY_MARKER: &str = "carryctx context";
const SHIM_MARKER: &str = "hooks dispatch";

// Composition markers
const COMPOSE_BEGIN_MARKER: &str = "# === BEGIN foreign hook (preserved by carryctx --compose) ===";
const COMPOSE_END_MARKER: &str = "# === END foreign hook ===";
const COMPOSE_CARRYCTX_MARKER: &str = "# === CarryCtx shim (managed by carryctx hooks) ===";

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: hooks
// ═══════════════════════════════════════════════════════════════════════════

/// Report a hooks failure through the standard error path in JSON mode (error
/// envelope on stderr) while keeping the historical human-readable line and
/// exit code untouched in text mode.
fn report_hooks_error(
    command: &str,
    error: crate::error::CarryCtxError,
    text_line: &str,
    exit_code: ExitCode,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    if is_json {
        crate::cli::render_and_print::<serde_json::Value>(command, Err(error), true, ctx.quiet)
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
        let result: Result<serde_json::Value, crate::error::CarryCtxError> = Ok(data);
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
        HooksCommand::Dispatch(a) => handle_hooks_dispatch(a, ctx, is_json),
    }
}

fn git_hooks_dir(ctx: &InvocationContext) -> Result<std::path::PathBuf, ExitCode> {
    use crate::adapter::git::GitCli;
    let work_dir = resolve_work_dir(ctx);
    let git = GitCli::new();
    let gp = git.discover(work_dir).map_err(|e| e.exit_code)?;
    Ok(gp.git_common_dir.join("hooks"))
}

/// Detect foreign manager for a hook path when a foreign hook is present.
fn detect_foreign_manager(ctx: &InvocationContext, hook_content: &str) -> &'static str {
    if hook_content.contains("lefthook") {
        return "lefthook";
    }
    if hook_content.contains("husky") || hook_content.contains(".husky") {
        return "husky";
    }
    // File-system signals (only when we can resolve work_dir)
    let work_dir = resolve_work_dir(ctx);
    if work_dir.join("lefthook.yml").exists() || work_dir.join("lefthook.yaml").exists() {
        return "lefthook";
    }
    if work_dir.join(".husky").is_dir() {
        return "husky";
    }
    "custom"
}

fn is_shim(content: &str) -> bool {
    content.contains(SHIM_MARKER)
}

fn is_legacy_fat(content: &str) -> bool {
    content.contains("CarryCtx") && content.contains(LEGACY_MARKER)
}

fn is_composed(content: &str) -> bool {
    content.contains(COMPOSE_BEGIN_MARKER) && content.contains(SHIM_MARKER)
}

fn shim_for(name: &str) -> &'static str {
    match name {
        "post-commit" => POST_COMMIT_SHIM,
        "prepare-commit-msg" => PREPARE_COMMIT_MSG_SHIM,
        _ => unreachable!(),
    }
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
    use crate::adapter::git::{GitCli, detect_jj_colocation};
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
            crate::error::CarryCtxError::validation_error(
                "This repository is jj-colocated (.jj/ alongside .git/); CarryCtx git hooks would never fire under jj. Run `carryctx checkpoint` manually after `jj commit`/`jj describe` instead.",
            ),
            JJ_COLOCATION_TEXT,
            ExitCode::Validation,
            ctx,
            is_json,
        );
    }
    if args.compose && args.force {
        // Both flags together would be ambiguous (compose chains, force displaces).
        // Prefer compose when explicitly requested; warn and continue with compose semantics.
        if !ctx.quiet && !is_json {
            eprintln!(
                "warning: --compose and --force both supplied; composing instead of displacing."
            );
        }
    }
    let hooks_dir = gp.git_common_dir.join("hooks");
    if let Err(e) = fs::create_dir_all(&hooks_dir) {
        return report_hooks_error(
            "hooks.install",
            crate::error::CarryCtxError::io_error(format!("Failed to create hooks dir: {e}")),
            &format!("Failed to create hooks dir: {e}"),
            ExitCode::General,
            ctx,
            is_json,
        );
    }

    let to_install: &[&str] = if args.post_commit_only {
        &["post-commit"]
    } else {
        &["post-commit", "prepare-commit-msg"]
    };

    let mut installed: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    for name in to_install {
        let shim_content = shim_for(name);
        let path = hooks_dir.join(name);
        if path.exists() {
            let content = fs::read_to_string(&path).unwrap_or_default();
            let managed = content.contains("CarryCtx");
            let composed = is_composed(&content);
            let foreign = !managed;

            if foreign && !args.force && !args.compose {
                return report_hooks_error(
                    "hooks.install",
                    crate::error::CarryCtxError::new(
                        "HOOK_EXISTS",
                        format!(
                            "Hook '{name}' already exists. Use --force to overwrite or --compose to chain."
                        ),
                        ExitCode::General,
                    ),
                    &format!(
                        "Hook '{name}' already exists. Use --force to overwrite or --compose to chain."
                    ),
                    ExitCode::General,
                    ctx,
                    is_json,
                );
            }
            if foreign && args.compose {
                // Compose: preserve foreign content verbatim, then append shim.
                let foreign_body = content.clone();
                let composed_content = format!(
                    "{COMPOSE_BEGIN_MARKER}\n{foreign_body}\n{COMPOSE_END_MARKER}\n{COMPOSE_CARRYCTX_MARKER}\n{shim_content}"
                );
                if let Err(e) =
                    crate::adapter::filesystem::write_atomic(&path, composed_content.as_bytes())
                {
                    return report_hooks_error(
                        "hooks.install",
                        crate::error::CarryCtxError::io_error(format!(
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
                    if let Err(e) = fs::set_permissions(&path, fs::Permissions::from_mode(0o755)) {
                        return report_hooks_error(
                            "hooks.install",
                            crate::error::CarryCtxError::io_error(format!(
                                "Failed to make hook {name} executable: {e}"
                            )),
                            &format!("Failed to make hook {name} executable: {e}"),
                            ExitCode::General,
                            ctx,
                            is_json,
                        );
                    }
                }
                let mgr = detect_foreign_manager(ctx, &foreign_body);
                warnings.push(format!(
                    "Composed with foreign {mgr} hook '{name}' (foreign-first chaining)."
                ));
                if !ctx.quiet && !is_json {
                    println!(
                        "✓ Composed hook: {name} (foreign {mgr} preserved, CarryCtx shim chained)"
                    );
                }
                installed.push((*name).to_string());
                continue;
            }
            if foreign && args.force {
                let bak = hooks_dir.join(format!("{name}.bak"));
                fs::rename(&path, &bak).ok();
                warnings.push(format!(
                    "Foreign hook '{name}' displaced to {}",
                    bak.display()
                ));
                if !ctx.quiet && !is_json {
                    eprintln!(
                        "warning: Foreign hook '{name}' displaced to {}",
                        bak.display()
                    );
                }
            } else if managed && !args.force && !args.compose {
                // Managed but no force/compose and not foreign: this is a re-install of our own hook.
                // Allow re-install as shim (idempotent) even without --force if it's already managed,
                // but treat legacy fat hook as needing upgrade (still requires --force? No, allow upgrade).
                // For phase 1, allow managed re-install without --force: just rewrite shim atomically.
                // Exception: if it's already a shim with identical content, skip write.
                if is_shim(&content) && content.trim() == shim_content.trim() && !composed {
                    if !ctx.quiet && !is_json {
                        println!("✓ Hook already installed: {name}");
                    }
                    installed.push((*name).to_string());
                    continue;
                }
                // Legacy fat hook: upgrade path without --force is allowed (same owner).
                // Compose files: re-compose is idempotent, already handled.
                if composed {
                    // Already composed and reinstall without --compose: keep composition, just ensure shim segment present.
                    if content.contains(shim_content.trim()) {
                        if !ctx.quiet && !is_json {
                            println!("✓ Hook already composed: {name}");
                        }
                        installed.push((*name).to_string());
                        continue;
                    }
                }
                // For managed non-shim without force, still allow upgrade but backup legacy.
                let bak = hooks_dir.join(format!("{name}.bak"));
                // Only backup legacy once; don't overwrite an existing .bak from a previous upgrade.
                if !bak.exists() {
                    fs::rename(&path, &bak).ok();
                }
            } else if managed && args.force {
                let bak = hooks_dir.join(format!("{name}.bak"));
                fs::rename(&path, &bak).ok();
            } else if composed && (args.force || args.compose) {
                // Re-compose or force over composed: extract foreign part and recompose.
                // For simplicity, treat as foreign compose again: extract between markers.
                let foreign_body = if let Some(start) = content.find(COMPOSE_BEGIN_MARKER) {
                    let after_begin = &content[start + COMPOSE_BEGIN_MARKER.len()..];
                    if let Some(end) = after_begin.find(COMPOSE_END_MARKER) {
                        after_begin[..end].trim().to_string()
                    } else {
                        after_begin.to_string()
                    }
                } else {
                    // Fallback: strip shim segment
                    content.replace(shim_content, "").trim().to_string()
                };
                let composed_content = format!(
                    "{COMPOSE_BEGIN_MARKER}\n{foreign_body}\n{COMPOSE_END_MARKER}\n{COMPOSE_CARRYCTX_MARKER}\n{shim_content}"
                );
                if let Err(e) =
                    crate::adapter::filesystem::write_atomic(&path, composed_content.as_bytes())
                {
                    return report_hooks_error(
                        "hooks.install",
                        crate::error::CarryCtxError::io_error(format!(
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
                    if let Err(e) = fs::set_permissions(&path, fs::Permissions::from_mode(0o755)) {
                        return report_hooks_error(
                            "hooks.install",
                            crate::error::CarryCtxError::io_error(format!(
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
                    println!("✓ Re-composed hook: {name}");
                }
                installed.push((*name).to_string());
                continue;
            }
        }
        // Install atomically (tmp file + rename): a `git commit` racing the
        // install must never execute a truncated or partially written hook.
        if let Err(e) = crate::adapter::filesystem::write_atomic(&path, shim_content.as_bytes()) {
            return report_hooks_error(
                "hooks.install",
                crate::error::CarryCtxError::io_error(format!("Failed to write hook {name}: {e}")),
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
                    crate::error::CarryCtxError::io_error(format!(
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
            println!("✓ Installed hook: {name} (shim)");
        }
        installed.push((*name).to_string());
    }
    // Warnings for foreign displacement are already collected; surface via envelope warnings in JSON.
    if is_json && !warnings.is_empty() {
        let data = serde_json::json!({
            "installed": installed,
            "hooksDir": hooks_dir.display().to_string(),
            "warnings": warnings,
        });
        let result: Result<serde_json::Value, crate::error::CarryCtxError> = Ok(data);
        return render_and_print("hooks.install", result, true, ctx.quiet);
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
                crate::error::CarryCtxError::git_error("Not inside a Git repository."),
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
            let content = fs::read_to_string(&path).unwrap_or_default();
            let managed = content.contains("CarryCtx");
            let composed = is_composed(&content);
            if !managed {
                if !ctx.quiet && !is_json {
                    println!("Skipping '{name}' — not a CarryCtx hook.");
                }
                continue;
            }
            if composed {
                // Remove only the CarryCtx segment, restore foreign body byte-for-byte.
                let foreign_body = if let Some(start) = content.find(COMPOSE_BEGIN_MARKER) {
                    let after_begin = &content[start + COMPOSE_BEGIN_MARKER.len()..];
                    if let Some(end) = after_begin.find(COMPOSE_END_MARKER) {
                        after_begin[..end].trim().to_string()
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                };
                if foreign_body.is_empty() {
                    // Fallback: remove shim markers and leave rest
                    let stripped = content
                        .replace(COMPOSE_BEGIN_MARKER, "")
                        .replace(COMPOSE_END_MARKER, "")
                        .replace(COMPOSE_CARRYCTX_MARKER, "")
                        .replace(POST_COMMIT_SHIM, "")
                        .replace(PREPARE_COMMIT_MSG_SHIM, "")
                        .trim()
                        .to_string();
                    if stripped.is_empty() {
                        fs::remove_file(&path).ok();
                    } else {
                        crate::adapter::filesystem::write_atomic(&path, stripped.as_bytes()).ok();
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).ok();
                        }
                    }
                } else if foreign_body.trim().is_empty() {
                    fs::remove_file(&path).ok();
                } else {
                    // Restore foreign body verbatim
                    if let Err(e) =
                        crate::adapter::filesystem::write_atomic(&path, foreign_body.as_bytes())
                    {
                        return report_hooks_error(
                            "hooks.uninstall",
                            crate::error::CarryCtxError::io_error(format!(
                                "Failed to restore foreign hook {name}: {e}"
                            )),
                            &format!("Failed to restore foreign hook {name}: {e}"),
                            ExitCode::General,
                            ctx,
                            is_json,
                        );
                    }
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).ok();
                    }
                    if !ctx.quiet && !is_json {
                        println!("✓ Restored foreign hook: {name}");
                    }
                    continue;
                }
                if !ctx.quiet && !is_json {
                    println!("✓ Removed CarryCtx segment from composed hook: {name}");
                }
                removed.push((*name).to_string());
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
                    crate::error::CarryCtxError::io_error(format!(
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
        let (installed, managed, shim, legacy, composed, foreign_manager) = if path.exists() {
            let content = fs::read_to_string(&path).unwrap_or_default();
            let managed = content.contains("CarryCtx");
            let shim_flag = is_shim(&content);
            let legacy_flag = is_legacy_fat(&content);
            let composed_flag = is_composed(&content);
            let fm = if !managed {
                detect_foreign_manager(ctx, &content)
            } else if composed_flag {
                // For composed files, detect manager from preserved foreign segment
                let foreign_segment = if let Some(start) = content.find(COMPOSE_BEGIN_MARKER) {
                    let after = &content[start + COMPOSE_BEGIN_MARKER.len()..];
                    if let Some(end) = after.find(COMPOSE_END_MARKER) {
                        &after[..end]
                    } else {
                        after
                    }
                } else {
                    &content
                };
                detect_foreign_manager(ctx, foreign_segment)
            } else {
                "none"
            };
            (true, managed, shim_flag, legacy_flag, composed_flag, fm)
        } else {
            (false, false, false, false, false, "none")
        };
        statuses.push(serde_json::json!({
            "hook": name,
            "installed": installed,
            "managed_by_carryctx": managed,
            "shim": shim,
            "legacy": legacy,
            "composed": composed,
            "foreign_manager": foreign_manager,
            "path": path.display().to_string(),
        }));
    }

    let result = serde_json::json!({ "hooks": statuses });
    let err_result: Result<serde_json::Value, crate::error::CarryCtxError> = Ok(result);
    render_and_print("hooks.status", err_result, args.json, ctx.quiet)
}

fn handle_hooks_dispatch(
    args: &HooksDispatchArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    let emit_json = is_json || args.json;
    let event = args.event.trim();
    match event {
        "git.post-commit" => dispatch_post_commit(ctx, emit_json),
        "git.prepare-commit-msg" => dispatch_prepare_commit_msg(ctx, emit_json, &args.args),
        other => {
            let err = crate::error::CarryCtxError::validation_error(format!(
                "Unknown hook event '{other}'. Supported: git.post-commit, git.prepare-commit-msg"
            ));
            crate::cli::render_and_print::<serde_json::Value>(
                "hooks.dispatch",
                Err(err),
                emit_json,
                ctx.quiet,
            )
        }
    }
}

fn dispatch_post_commit(ctx: &InvocationContext, emit_json: bool) -> Result<ExitCode, ExitCode> {
    // Dry-run never fires hooks (§0, constraint 6)
    if ctx.dry_run {
        if emit_json {
            return crate::cli::render_and_print::<serde_json::Value>(
                "hooks.dispatch",
                Ok(
                    serde_json::json!({"event":"git.post-commit","dispatched":false,"reason":"dry_run","hooks_skipped":true}),
                ),
                true,
                ctx.quiet,
            );
        }
        return Ok(ExitCode::Success);
    }

    // Open runtime; hook context failures are soft (git commit must not abort).
    let mut runtime = match crate::cli::open_runtime(ctx) {
        Ok(rt) => rt,
        Err(e) => {
            // Soft failure: report but don't abort git commit. In JSON mode, emit error envelope.
            if emit_json {
                return crate::cli::render_and_print::<serde_json::Value>(
                    "hooks.dispatch",
                    Err(e),
                    true,
                    ctx.quiet,
                );
            }
            // Non-JSON hook invocation: silently succeed so git commit proceeds.
            return Ok(ExitCode::Success);
        }
    };

    let project_id = runtime.config.project.id.clone();
    let conn = runtime.database.connection_mut();
    // Isolate git env for internal Git calls (hook runners set GIT_DIR etc.)
    // GitCli already env_clears, so no extra action needed here.

    let uow = match crate::adapter::unit_of_work::UnitOfWork::begin(conn) {
        Ok(u) => u,
        Err(e) => {
            if emit_json {
                return crate::cli::render_and_print::<serde_json::Value>(
                    "hooks.dispatch",
                    Err(e),
                    true,
                    ctx.quiet,
                );
            }
            return Ok(ExitCode::Success);
        }
    };

    let resolver = crate::application::runtime::CurrentEntityResolver::new(&project_id, &uow);
    let agent_id = resolver
        .resolve_agent(
            ctx.agent.as_deref(),
            None,
            None,
            runtime.config.agent.default_name.as_deref(),
            runtime.config.agent.default_name.as_deref(),
        )
        .ok()
        .map(|a| a.id);

    let task = resolver
        .resolve_task(None, Some(&ctx.cwd.to_string_lossy()), agent_id.as_deref())
        .unwrap_or_default();

    let Some(task) = task else {
        uow.commit()
            .map_err(|e| {
                if emit_json {
                    crate::cli::render_and_print::<serde_json::Value>(
                        "hooks.dispatch",
                        Err(e),
                        true,
                        ctx.quiet,
                    )
                    .err()
                    .unwrap_or(ExitCode::Database)
                } else {
                    ExitCode::Database
                }
            })
            .ok();
        if emit_json {
            return crate::cli::render_and_print::<serde_json::Value>(
                "hooks.dispatch",
                Ok(
                    serde_json::json!({"event":"git.post-commit","dispatched":false,"reason":"no_active_task"}),
                ),
                true,
                ctx.quiet,
            );
        }
        return Ok(ExitCode::Success);
    };

    // Resolve commit SHA for note (soft, don't fail checkpoint if unavailable)
    let head = runtime.git_project.head.clone().or_else(|| {
        // Fallback: try git rev-parse HEAD directly, isolated from ambient GIT_*.
        let mut cmd = std::process::Command::new("git");
        cmd.arg("-C")
            .arg(&runtime.git_project.repository_root)
            .arg("rev-parse")
            .arg("HEAD");
        crate::adapter::git::isolate_git_env(&mut cmd);
        cmd.output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
                } else {
                    None
                }
            })
            .filter(|s| !s.is_empty())
    });

    let note = head
        .as_deref()
        .map(|sha| format!("Auto-checkpoint after commit {sha}"))
        .unwrap_or_else(|| "Auto-checkpoint after commit".to_string());

    let checkpoint_repo =
        crate::adapter::sqlite_repos::SqliteCheckpointRepository::new(uow.connection());
    let event_repo = crate::adapter::sqlite_repos::SqliteEventRepository::new(uow.connection());
    let graph_repo = crate::repository::graph::GraphRepository::new(uow.connection());
    let git_cli = crate::adapter::git::GitCli::new();

    let input = crate::application::checkpoint::CreateCheckpointInput {
        project_id: project_id.clone(),
        task_id: task.id.clone(),
        session_id: ctx.session.clone(),
        agent_id: agent_id.clone(),
        worktree_id: None,
        branch: runtime.git_project.branch.clone(),
        head: runtime.git_project.head.clone(),
        done: vec![],
        remaining: vec![],
        blockers: vec![],
        risks: vec![],
        next_actions: vec![],
        notes: vec![note],
        repo_path: Some(
            runtime
                .git_project
                .repository_root
                .to_string_lossy()
                .to_string(),
        ),
    };

    let now = chrono::Utc::now().to_rfc3339();
    let result = crate::application::checkpoint::create_checkpoint(
        &checkpoint_repo,
        &event_repo,
        Some(&graph_repo),
        &git_cli,
        &input,
        &now,
    );

    match result {
        Ok(cp) => {
            if let Err(e) = uow.commit() {
                if emit_json {
                    return crate::cli::render_and_print::<serde_json::Value>(
                        "hooks.dispatch",
                        Err(e),
                        true,
                        ctx.quiet,
                    );
                }
                return Ok(ExitCode::Success);
            }
            if emit_json {
                crate::cli::render_and_print::<serde_json::Value>(
                    "hooks.dispatch",
                    Ok(
                        serde_json::json!({"event":"git.post-commit","dispatched":true,"checkpoint_id":cp.id,"task_id":task.display_id}),
                    ),
                    true,
                    ctx.quiet,
                )
            } else {
                Ok(ExitCode::Success)
            }
        }
        Err(e) => {
            // Rollback (UoW drops)
            if emit_json {
                crate::cli::render_and_print::<serde_json::Value>(
                    "hooks.dispatch",
                    Err(e),
                    true,
                    ctx.quiet,
                )
            } else {
                // Hook soft-fail: don't abort git commit
                Ok(ExitCode::Success)
            }
        }
    }
}

fn dispatch_prepare_commit_msg(
    ctx: &InvocationContext,
    emit_json: bool,
    hook_args: &[String],
) -> Result<ExitCode, ExitCode> {
    if hook_args.is_empty() {
        let err = crate::error::CarryCtxError::invalid_arguments(
            "git.prepare-commit-msg requires the commit message file path as first argument",
        );
        return crate::cli::render_and_print::<serde_json::Value>(
            "hooks.dispatch",
            Err(err),
            emit_json,
            ctx.quiet,
        );
    }
    let commit_msg_file = &hook_args[0];
    let commit_source = hook_args.get(1).map(|s| s.as_str()).unwrap_or("");

    if commit_source == "merge" || commit_source == "squash" {
        if emit_json {
            return crate::cli::render_and_print::<serde_json::Value>(
                "hooks.dispatch",
                Ok(
                    serde_json::json!({"event":"git.prepare-commit-msg","dispatched":false,"reason":"merge_or_squash"}),
                ),
                true,
                ctx.quiet,
            );
        }
        return Ok(ExitCode::Success);
    }

    if ctx.dry_run {
        if emit_json {
            return crate::cli::render_and_print::<serde_json::Value>(
                "hooks.dispatch",
                Ok(
                    serde_json::json!({"event":"git.prepare-commit-msg","dispatched":false,"reason":"dry_run","hooks_skipped":true}),
                ),
                true,
                ctx.quiet,
            );
        }
        return Ok(ExitCode::Success);
    }

    // Resolve task
    let mut runtime = match crate::cli::open_runtime(ctx) {
        Ok(rt) => rt,
        Err(e) => {
            if emit_json {
                return crate::cli::render_and_print::<serde_json::Value>(
                    "hooks.dispatch",
                    Err(e),
                    true,
                    ctx.quiet,
                );
            }
            return Ok(ExitCode::Success);
        }
    };
    let project_id = runtime.config.project.id.clone();
    // Need a read UoW to resolve task; use UnitOfWork begin then commit immediately
    let task_display_id = {
        let conn = runtime.database.connection_mut();
        let uow = match crate::adapter::unit_of_work::UnitOfWork::begin(conn) {
            Ok(u) => u,
            Err(e) => {
                if emit_json {
                    return crate::cli::render_and_print::<serde_json::Value>(
                        "hooks.dispatch",
                        Err(e),
                        true,
                        ctx.quiet,
                    );
                }
                return Ok(ExitCode::Success);
            }
        };
        let resolver = crate::application::runtime::CurrentEntityResolver::new(&project_id, &uow);
        let agent_id = resolver
            .resolve_agent(
                ctx.agent.as_deref(),
                None,
                None,
                runtime.config.agent.default_name.as_deref(),
                runtime.config.agent.default_name.as_deref(),
            )
            .ok()
            .map(|a| a.id);
        let task = resolver
            .resolve_task(None, Some(&ctx.cwd.to_string_lossy()), agent_id.as_deref())
            .ok()
            .flatten();
        let _ = uow.commit();
        task.map(|t| t.display_id)
    };

    let Some(task_id) = task_display_id else {
        if emit_json {
            return crate::cli::render_and_print::<serde_json::Value>(
                "hooks.dispatch",
                Ok(
                    serde_json::json!({"event":"git.prepare-commit-msg","dispatched":false,"reason":"no_active_task"}),
                ),
                true,
                ctx.quiet,
            );
        }
        return Ok(ExitCode::Success);
    };

    // Read, check prefix, write back atomically
    let orig = match std::fs::read_to_string(commit_msg_file) {
        Ok(s) => s,
        Err(e) => {
            let err = crate::error::CarryCtxError::io_error(format!(
                "Failed to read commit message file: {e}"
            ));
            return crate::cli::render_and_print::<serde_json::Value>(
                "hooks.dispatch",
                Err(err),
                emit_json,
                ctx.quiet,
            );
        }
    };
    if orig.starts_with(&format!("[{task_id}]")) {
        if emit_json {
            return crate::cli::render_and_print::<serde_json::Value>(
                "hooks.dispatch",
                Ok(
                    serde_json::json!({"event":"git.prepare-commit-msg","dispatched":false,"reason":"already_prefixed","task_id":task_id}),
                ),
                true,
                ctx.quiet,
            );
        }
        return Ok(ExitCode::Success);
    }
    let new_content = format!("[{task_id}] {orig}");
    // Atomic write: tmp + rename so git doesn't see a torn file
    if let Err(e) = crate::adapter::filesystem::write_atomic(
        std::path::Path::new(commit_msg_file),
        new_content.as_bytes(),
    ) {
        let err = crate::error::CarryCtxError::io_error(format!(
            "Failed to write commit message file: {e}"
        ));
        return crate::cli::render_and_print::<serde_json::Value>(
            "hooks.dispatch",
            Err(err),
            emit_json,
            ctx.quiet,
        );
    }
    if emit_json {
        crate::cli::render_and_print::<serde_json::Value>(
            "hooks.dispatch",
            Ok(
                serde_json::json!({"event":"git.prepare-commit-msg","dispatched":true,"task_id":task_id}),
            ),
            true,
            ctx.quiet,
        )
    } else {
        Ok(ExitCode::Success)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shim_templates_use_hooks_dispatch_and_not_context_grep() {
        for (name, shim) in [
            ("post-commit", POST_COMMIT_SHIM),
            ("prepare-commit-msg", PREPARE_COMMIT_MSG_SHIM),
        ] {
            assert!(
                shim.contains("hooks dispatch"),
                "shim {name} must route via `hooks dispatch`"
            );
            assert!(
                shim.contains("CarryCtx"),
                "shim {name} must contain CarryCtx marker for managed detection"
            );
            assert!(
                shim.contains(&format!("git.{name}")),
                "shim {name} must dispatch its own event"
            );
            // Shims must not contain the legacy context grep pipeline
            assert!(
                !shim.contains("carryctx context"),
                "shim {name} must not contain legacy `carryctx context` pipeline"
            );
            assert!(
                !shim.contains("displayId"),
                "shim {name} must not contain legacy displayId grep"
            );
            assert!(
                shim.contains("exec carryctx hooks dispatch"),
                "shim {name} must exec dispatch for correct signal forwarding"
            );
        }
    }

    #[test]
    fn legacy_detection_distinguishes_fat_from_shim() {
        let fat = r#"#!/bin/sh
# CarryCtx post-commit hook
TASK_ID=$(carryctx context --format json 2>/dev/null | grep -o '"display_id":"[^"]*"' | head -1 | cut -d'"' -f4)
"#;
        assert!(is_legacy_fat(fat));
        assert!(!is_shim(fat));
        assert!(!is_composed(fat));

        let shim = POST_COMMIT_SHIM;
        assert!(is_shim(shim));
        assert!(!is_legacy_fat(shim));
        assert!(!is_composed(shim));

        let composed = format!(
            "{COMPOSE_BEGIN_MARKER}\n#!/bin/sh\necho foreign\n{COMPOSE_END_MARKER}\n{COMPOSE_CARRYCTX_MARKER}\n{shim}"
        );
        assert!(is_composed(&composed));
        assert!(is_shim(&composed));
    }

    /// The legacy fat hooks used to grep display_id with a shell pipeline.
    /// That pipeline is retired by the shim, but the legacy string still
    /// validates correctly when read back for migration detection.
    #[test]
    fn legacy_marker_still_identifiable_for_migration() {
        assert!(LEGACY_MARKER.contains("carryctx context"));
    }
}
