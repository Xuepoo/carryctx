//! `carryctx conflict` command family (design
//! `2026-09-10-mergeable-git-managed-state.md` §2.4–§2.6).
//!
//! Clap parsing only; the read/write orchestration lives in
//! [`crate::application::merge_conflict`]. `list`/`show` are read-only;
//! `resolve`/`apply`/`abort` are treated as direct-lock commands by the
//! dispatcher and acquire the admission lock inside the application layer.

use clap::Parser;

use crate::adapter::git::GitCli;
use crate::adapter::xdg::XdgPaths;
use crate::application::runtime::InvocationContext;
use crate::cli::{render_and_print, render_and_print_entity, resolve_work_dir};
use crate::error::{CarryCtxError, ExitCode};

#[derive(Parser, Debug)]
pub enum ConflictCommand {
    /// List staged conflicts (open by default; `--all` adds resolved and auto-resolutions)
    List {
        /// Include resolved conflicts and the staged auto-resolutions
        #[arg(long)]
        all: bool,
        /// Select a specific merge session id (default: the single active session)
        #[arg(long, value_name = "ID")]
        merge: Option<String>,
    },
    /// Show the base/ours/theirs rows and policy reason for one conflict
    Show {
        /// Conflict id as reported by `conflict list`
        conflict_id: String,
        /// Select a specific merge session id (default: the single active session)
        #[arg(long, value_name = "ID")]
        merge: Option<String>,
    },
    /// Record a resolution choice for one conflict (does not touch the live database)
    Resolve {
        /// Conflict id as reported by `conflict list`
        conflict_id: String,
        /// Keep the local row
        #[arg(long, conflicts_with = "theirs", required_unless_present = "theirs")]
        ours: bool,
        /// Take the incoming row
        #[arg(long, conflicts_with = "ours", required_unless_present = "ours")]
        theirs: bool,
        /// Select a specific merge session id (default: the single active session)
        #[arg(long, value_name = "ID")]
        merge: Option<String>,
        /// Override one field of the chosen row (repeatable, FIELD=VALUE)
        #[arg(long = "set", value_name = "FIELD=VALUE")]
        set: Vec<String>,
    },
    /// Materialize every resolution and atomically swap the merged state in
    Apply {
        /// Select a specific merge session id (default: the single active session)
        #[arg(long, value_name = "ID")]
        merge: Option<String>,
        /// Settle every remaining open conflict at the local (ours) value
        #[arg(long)]
        skip_open: bool,
    },
    /// Delete the staged merge session without changing the database
    Abort {
        /// Select a specific merge session id (default: the single active session)
        #[arg(long, value_name = "ID")]
        merge: Option<String>,
    },
}

#[derive(Parser, Debug)]
pub struct ConflictArgs {
    #[command(subcommand)]
    pub command: ConflictCommand,
}

fn discover(
    ctx: &InvocationContext,
) -> Result<(crate::adapter::git::GitProject, XdgPaths), CarryCtxError> {
    let gp = GitCli::new().discover(resolve_work_dir(ctx))?;
    Ok((gp, XdgPaths::new()))
}

pub fn handle_conflict(
    args: &ConflictArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    match &args.command {
        ConflictCommand::List { all, merge } => {
            let result = discover(ctx).and_then(|(gp, xdg)| {
                crate::application::merge_conflict::list_conflicts(
                    &gp,
                    &xdg,
                    merge.as_deref(),
                    *all,
                )
            });
            render_and_print("conflict.list", result, is_json, ctx.quiet)
        }
        ConflictCommand::Show { conflict_id, merge } => {
            let result = discover(ctx).and_then(|(gp, xdg)| {
                crate::application::merge_conflict::show_conflict(
                    &gp,
                    &xdg,
                    conflict_id,
                    merge.as_deref(),
                )
            });
            render_show(result, ctx, is_json)
        }
        ConflictCommand::Resolve {
            conflict_id,
            ours,
            merge,
            set,
            ..
        } => {
            let choice = if *ours { "ours" } else { "theirs" };
            let result = discover(ctx).and_then(|(gp, xdg)| {
                crate::application::merge_conflict::resolve_conflict(
                    &gp,
                    &xdg,
                    merge.as_deref(),
                    conflict_id,
                    choice,
                    set,
                    ctx.agent.as_deref(),
                    ctx.dry_run,
                )
            });
            render_and_print("conflict.resolve", result, is_json, ctx.quiet)
        }
        ConflictCommand::Apply { merge, skip_open } => {
            let result = discover(ctx).and_then(|(gp, xdg)| {
                let db_path = xdg.project_db(&gp.git_common_dir);
                crate::application::merge_conflict::apply_conflicts(
                    &gp,
                    &xdg,
                    &db_path,
                    merge.as_deref(),
                    *skip_open,
                    ctx.dry_run,
                    ctx.agent.as_deref(),
                    ctx.session.as_deref(),
                )
            });
            render_and_print("conflict.apply", result, is_json, ctx.quiet)
        }
        ConflictCommand::Abort { merge } => {
            let result = discover(ctx).and_then(|(gp, xdg)| {
                crate::application::merge_conflict::abort_conflicts(
                    &gp,
                    &xdg,
                    merge.as_deref(),
                    ctx.dry_run,
                )
            });
            render_and_print("conflict.abort", result, is_json, ctx.quiet)
        }
    }
}

/// `show` renders the standard JSON envelope in JSON mode and a readable
/// base/ours/theirs document otherwise (`--format markdown` included).
fn render_show(
    result: Result<serde_json::Value, CarryCtxError>,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    match result {
        Ok(data) => {
            if is_json {
                return render_and_print_entity(
                    "conflict.show",
                    Ok(data),
                    true,
                    ctx.quiet,
                    false,
                    None,
                    None,
                );
            }
            if !ctx.quiet {
                print!("{}", render_conflict_text(&data));
            }
            Ok(ExitCode::Success)
        }
        Err(error) => {
            render_and_print::<serde_json::Value>("conflict.show", Err(error), is_json, ctx.quiet)
        }
    }
}

fn render_conflict_text(data: &serde_json::Value) -> String {
    let conflict = match data.get("conflict") {
        Some(conflict) => conflict,
        None => return serde_json::to_string_pretty(data).unwrap_or_default(),
    };
    let field = |key: &str| {
        conflict
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
    };
    let mut out = String::new();
    out.push_str(&format!(
        "Conflict {} [{}] on {}\n",
        field("id"),
        field("kind"),
        field("table")
    ));
    out.push_str(&format!("  key:    {}\n", field("key")));
    out.push_str(&format!("  reason: {}\n", field("reason")));
    match conflict.get("resolution").filter(|value| !value.is_null()) {
        Some(resolution) => {
            out.push_str(&format!(
                "  resolution: {} by {} at {}\n",
                resolution
                    .get("choice")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("?"),
                resolution
                    .get("resolvedBy")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown"),
                resolution
                    .get("resolvedAt")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(""),
            ));
        }
        None => out.push_str("  resolution: open\n"),
    }
    for side in ["base", "ours", "theirs"] {
        let rendered =
            serde_json::to_string_pretty(conflict.get(side).unwrap_or(&serde_json::Value::Null))
                .unwrap_or_else(|_| "null".to_string());
        out.push_str(&format!("\n{side}:\n{rendered}\n"));
    }
    out
}
