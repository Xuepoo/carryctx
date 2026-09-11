//! `carryctx trust` — grant, revoke, list, and status for repository-provided
//! executable policy (CTX-0100). See `design/2026-09-11-project-trust-executable-policy.md`.

use clap::Parser;

use crate::adapter::sqlite_repos::SqliteEventRepository;
use crate::adapter::trust_store::TrustStore;
use crate::adapter::xdg::XdgPaths;
use crate::application::runtime::{InvocationContext, ProjectRuntime};
use crate::application::trust::{evaluate_project, list_payload, registry_path, registry_warning};
use crate::cli::{open_runtime_or_report, render_and_print_with_warnings, resolve_agent_id};
use crate::domain::trust::{TrustEntry, collect_external_policy, policy_fingerprint};
use crate::error::{CarryCtxError, ExitCode};
use crate::repository::{EventRepository, NewEvent};

// ── Trust ────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub enum TrustCommand {
    /// Grant this project permission to run its declared external policy (requires --yes)
    Grant,
    /// Revoke this project's grant (idempotent)
    Revoke,
    /// List every trusted project id in the local registry
    List,
    /// Report the effective trust verdict for the current project
    Status,
}

#[derive(Parser, Debug)]
pub struct TrustArgs {
    /// Trust subcommand to execute
    #[command(subcommand)]
    pub command: TrustCommand,
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: trust
// ═══════════════════════════════════════════════════════════════════════════

pub fn handle_trust(
    args: &TrustArgs,
    pre_opened: Option<ProjectRuntime>,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    match &args.command {
        TrustCommand::List => handle_trust_list(ctx, is_json),
        TrustCommand::Status => handle_trust_status(pre_opened, ctx, is_json),
        TrustCommand::Grant => handle_trust_grant(pre_opened, ctx, is_json),
        TrustCommand::Revoke => handle_trust_revoke(pre_opened, ctx, is_json),
    }
}

fn handle_trust_list(ctx: &InvocationContext, is_json: bool) -> Result<ExitCode, ExitCode> {
    let xdg = XdgPaths::new();
    let load = TrustStore::new(registry_path(&xdg)).load();
    let warnings = registry_warning(&load).into_iter().collect();
    render_and_print_with_warnings::<serde_json::Value>(
        "trust.list",
        Ok(list_payload(&load)),
        is_json,
        ctx.quiet,
        warnings,
    )
}

fn handle_trust_status(
    pre_opened: Option<ProjectRuntime>,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    let runtime = match pre_opened {
        Some(runtime) => runtime,
        None => open_runtime_or_report(ctx, "trust.status")?,
    };
    let load = TrustStore::new(registry_path(&runtime.xdg)).load();
    let status = evaluate_project(
        &runtime.config,
        &runtime.config.project.id,
        &runtime.config.project.name,
        load,
    );
    let warnings: Vec<String> = registry_warning(&status.registry).into_iter().collect();
    render_and_print_with_warnings::<serde_json::Value>(
        "trust.status",
        Ok(status.to_json()),
        is_json,
        ctx.quiet,
        warnings,
    )
}

fn handle_trust_grant(
    pre_opened: Option<ProjectRuntime>,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    // Explicit confirmation is mandatory and is checked before any side
    // effect: a human or CI must pass --yes (design §6.1).
    if !ctx.yes {
        let error = CarryCtxError::invalid_arguments(
            "`trust grant` requires explicit confirmation; re-run with --yes.",
        )
        .with_suggestions([
            "Review the declared external policy with `carryctx trust status`.".into(),
            "Then run `carryctx trust grant --yes`.".into(),
        ]);
        return render_and_print_with_warnings::<serde_json::Value>(
            "trust.grant",
            Err(error),
            is_json,
            ctx.quiet,
            vec![],
        );
    }

    let runtime = match pre_opened {
        Some(runtime) => runtime,
        None => open_runtime_or_report(ctx, "trust.grant")?,
    };
    let store = TrustStore::new(registry_path(&runtime.xdg));
    let load = store.load();
    if !load.state.is_writable() {
        let error = CarryCtxError::configuration_error(format!(
            "Refusing to write the trust registry: {} is {} ({}). Fix or remove it first; CarryCtx never rewrites trust state it cannot parse or safely own.",
            load.path.display(),
            load.state.as_str(),
            load.detail.as_deref().unwrap_or("no detail")
        ))
        .with_suggestions([
            format!("Inspect {}", load.path.display()),
            "Remove the file only if you accept losing existing grants.".into(),
        ]);
        return render_and_print_with_warnings::<serde_json::Value>(
            "trust.grant",
            Err(error),
            is_json,
            ctx.quiet,
            vec![],
        );
    }

    let actor = match resolve_required_actor(&runtime, ctx) {
        Ok(actor) => actor,
        Err(error) => {
            return render_and_print_with_warnings::<serde_json::Value>(
                "trust.grant",
                Err(error),
                is_json,
                ctx.quiet,
                vec![],
            );
        }
    };

    let project_id = runtime.config.project.id.clone();
    let project_name = runtime.config.project.name.clone();
    let policy = collect_external_policy(&runtime.config);
    let fingerprint = policy_fingerprint(&policy);
    let command_count = policy.len();
    let decided_at = chrono::Utc::now().to_rfc3339();
    let mut registry = load.registry;
    registry.trusted.insert(
        project_id.clone(),
        TrustEntry {
            trusted: true,
            project_name: project_name.clone(),
            decided_at: decided_at.clone(),
            decided_by: actor.clone(),
            policy_fingerprint: fingerprint.clone(),
        },
    );

    if ctx.dry_run {
        let data = serde_json::json!({
            "dry_run": true,
            "project_id": project_id,
            "project_name": project_name,
            "trusted": true,
            "policy_fingerprint": fingerprint,
            "command_count": command_count,
            "decided_at": decided_at,
            "decided_by": actor,
            "registry_path": store.path().to_string_lossy(),
        });
        return render_and_print_with_warnings::<serde_json::Value>(
            "trust.grant",
            Ok(data),
            is_json,
            ctx.quiet,
            vec![],
        );
    }

    if let Err(error) = store.save(&registry) {
        return render_and_print_with_warnings::<serde_json::Value>(
            "trust.grant",
            Err(error),
            is_json,
            ctx.quiet,
            vec![],
        );
    }

    let mut warnings = Vec::new();
    let payload = serde_json::json!({
        "policy_fingerprint": fingerprint,
        "command_count": command_count,
    });
    if let Err(error) = append_trust_event(
        &runtime,
        &project_id,
        "project.trust_granted",
        payload,
        Some(&actor),
        ctx,
    ) {
        warnings.push(format!(
            "Trust change persisted, but the audit event could not be written: {}",
            error.message
        ));
    }
    if !runtime.config.security.allow_project_commands {
        warnings.push(
            "Global security gate security.allow_project_commands is false; external policy stays blocked until it is enabled.".into(),
        );
    }

    let data = serde_json::json!({
        "project_id": project_id,
        "project_name": project_name,
        "trusted": true,
        "policy_fingerprint": fingerprint,
        "command_count": command_count,
        "decided_at": decided_at,
        "decided_by": actor,
        "registry_path": store.path().to_string_lossy(),
    });
    render_and_print_with_warnings::<serde_json::Value>(
        "trust.grant",
        Ok(data),
        is_json,
        ctx.quiet,
        warnings,
    )
}

fn handle_trust_revoke(
    pre_opened: Option<ProjectRuntime>,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    let runtime = match pre_opened {
        Some(runtime) => runtime,
        None => open_runtime_or_report(ctx, "trust.revoke")?,
    };
    let store = TrustStore::new(registry_path(&runtime.xdg));
    let load = store.load();
    let project_id = runtime.config.project.id.clone();

    match load.state {
        crate::adapter::trust_store::RegistryState::Absent => {
            let data = serde_json::json!({
                "project_id": project_id,
                "removed": false,
                "registry_state": load.state.as_str(),
                "registry_path": store.path().to_string_lossy(),
            });
            render_and_print_with_warnings::<serde_json::Value>(
                "trust.revoke",
                Ok(data),
                is_json,
                ctx.quiet,
                vec![],
            )
        }
        crate::adapter::trust_store::RegistryState::Ok => {
            let mut registry = load.registry;
            let removed = registry.trusted.remove(&project_id).is_some();
            if !removed {
                let data = serde_json::json!({
                    "project_id": project_id,
                    "removed": false,
                    "registry_state": load.state.as_str(),
                    "registry_path": store.path().to_string_lossy(),
                });
                return render_and_print_with_warnings::<serde_json::Value>(
                    "trust.revoke",
                    Ok(data),
                    is_json,
                    ctx.quiet,
                    vec![],
                );
            }
            if ctx.dry_run {
                let data = serde_json::json!({
                    "dry_run": true,
                    "project_id": project_id,
                    "removed": true,
                    "registry_state": load.state.as_str(),
                    "registry_path": store.path().to_string_lossy(),
                });
                return render_and_print_with_warnings::<serde_json::Value>(
                    "trust.revoke",
                    Ok(data),
                    is_json,
                    ctx.quiet,
                    vec![],
                );
            }
            if let Err(error) = store.save(&registry) {
                return render_and_print_with_warnings::<serde_json::Value>(
                    "trust.revoke",
                    Err(error),
                    is_json,
                    ctx.quiet,
                    vec![],
                );
            }
            let actor = optional_actor(&runtime, ctx);
            let mut warnings = Vec::new();
            if let Err(error) = append_trust_event(
                &runtime,
                &project_id,
                "project.trust_revoked",
                serde_json::json!({}),
                actor.as_deref(),
                ctx,
            ) {
                warnings.push(format!(
                    "Trust change persisted, but the audit event could not be written: {}",
                    error.message
                ));
            }
            let data = serde_json::json!({
                "project_id": project_id,
                "removed": true,
                "registry_state": load.state.as_str(),
                "registry_path": store.path().to_string_lossy(),
            });
            render_and_print_with_warnings::<serde_json::Value>(
                "trust.revoke",
                Ok(data),
                is_json,
                ctx.quiet,
                warnings,
            )
        }
        _ => {
            // The registry is malformed, insecure, or unreadable; the
            // fail-closed load already treats every project as untrusted, so
            // revocation is a no-op. Never silently rewrite unknown state.
            let warnings: Vec<String> = registry_warning(&load).into_iter().collect();
            let data = serde_json::json!({
                "project_id": project_id,
                "removed": false,
                "registry_state": load.state.as_str(),
                "registry_path": store.path().to_string_lossy(),
            });
            render_and_print_with_warnings::<serde_json::Value>(
                "trust.revoke",
                Ok(data),
                is_json,
                ctx.quiet,
                warnings,
            )
        }
    }
}

fn resolve_required_actor(
    runtime: &ProjectRuntime,
    ctx: &InvocationContext,
) -> Result<String, CarryCtxError> {
    let reference = ctx.agent.as_deref().ok_or_else(|| {
        CarryCtxError::invalid_arguments(
            "`trust grant` requires an agent identity; pass --agent <name|ULID> or set CARRYCTX_AGENT.",
        )
    })?;
    resolve_agent_id(
        &runtime.config.project.id,
        reference,
        runtime.database.connection(),
    )
}

fn optional_actor(runtime: &ProjectRuntime, ctx: &InvocationContext) -> Option<String> {
    let reference = ctx.agent.as_deref()?;
    resolve_agent_id(
        &runtime.config.project.id,
        reference,
        runtime.database.connection(),
    )
    .ok()
}

fn append_trust_event(
    runtime: &ProjectRuntime,
    project_id: &str,
    event_type: &str,
    payload: serde_json::Value,
    actor_agent_id: Option<&str>,
    ctx: &InvocationContext,
) -> Result<(), CarryCtxError> {
    let event = NewEvent {
        id: ulid::Ulid::generate().to_string(),
        project_id: project_id.to_string(),
        event_type: event_type.to_string(),
        actor_agent_id: actor_agent_id.map(str::to_owned),
        session_id: ctx.session.clone(),
        task_id: None,
        payload,
        occurred_at: chrono::Utc::now().to_rfc3339(),
    };
    SqliteEventRepository::new(runtime.database.connection())
        .append(&event)
        .map(|_| ())
}
