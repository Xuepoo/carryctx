use crate::*;
use carryctx::adapter::unit_of_work::UnitOfWork;
use carryctx::application;
use carryctx::application::runtime::InvocationContext;
use carryctx::error::{CarryCtxError, ExitCode};
use clap::Parser;

// ── Agent ────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub enum AgentCommand {
    /// Register a new agent or sync an existing one into the project state
    Register {
        #[arg(long)]
        name: String,
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        role: Option<String>,
    },
    /// List all agents registered in the project database
    List,
    /// Show detailed metadata and history for a specific agent
    Show { agent_ref: String },
    /// Print the currently active agent based on the environment or global args
    Current,
    /// Rename an existing agent (updates the reference name, preserves the ULID)
    Rename {
        agent_ref: String,
        #[arg(long)]
        name: String,
    },
    /// Mark an agent as inactive so it cannot be assigned new tasks or sessions
    Deactivate { agent_ref: String },
}

#[derive(Parser, Debug)]
pub struct AgentArgs {
    /// Agent subcommand to execute
    #[command(subcommand)]
    pub command: AgentCommand,
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: agent
// ═══════════════════════════════════════════════════════════════════════════

pub fn handle_agent(
    args: &AgentArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    let mut runtime = try_open_runtime(ctx)?;
    let project_id = &runtime.config.project.id;
    let conn = runtime.database.connection_mut();

    match &args.command {
        AgentCommand::Register {
            name,
            provider,
            role,
        } => {
            let tx = conn
                .transaction()
                .map_err(|e| CarryCtxError::database_error(format!("{e}")).exit_code)?;
            let uow = UnitOfWork::new(tx);
            let metadata = if let Some(r) = role {
                serde_json::json!({ "role": r })
            } else {
                serde_json::Value::Null
            };
            let result = application::agent::register_agent(
                project_id,
                name,
                provider.as_deref(),
                metadata,
                &uow,
            );
            let committed = result.and_then(|agent| uow.commit().map(|_| agent));
            render_and_print("agent.register", committed, is_json, ctx.quiet)
        }
        AgentCommand::List => {
            let agents = application::agent::list_agents(
                project_id,
                &AgentFilter {
                    project_id: project_id.to_string(),
                    status: None,
                },
                &UnitOfWork::new(
                    conn.transaction()
                        .map_err(|e| CarryCtxError::database_error(format!("{e}")).exit_code)?,
                ),
            )
            .map_err(|e| e.exit_code)?;
            render_and_print("agent.list", Ok(agents), is_json, ctx.quiet)
        }
        AgentCommand::Show { agent_ref } => {
            let result = application::agent::show_agent(
                project_id,
                agent_ref,
                &UnitOfWork::new(
                    conn.transaction()
                        .map_err(|e| CarryCtxError::database_error(format!("{e}")).exit_code)?,
                ),
            );
            render_and_print("agent.show", result, is_json, ctx.quiet)
        }
        AgentCommand::Current => {
            let tx = conn
                .transaction()
                .map_err(|e| CarryCtxError::database_error(format!("{e}")).exit_code)?;
            let uow = UnitOfWork::new(tx);
            let resolver = application::runtime::CurrentEntityResolver::new(project_id, &uow);
            let agent = resolver.resolve_agent(
                ctx.agent.as_deref(),
                None,
                None,
                runtime.config.agent.default_name.as_deref(),
                runtime.config.agent.default_name.as_deref(),
            );
            match agent {
                Ok(a) => render_and_print("agent.current", Ok(a), is_json, ctx.quiet),
                Err(e) => render_and_print::<serde_json::Value>(
                    "agent.current",
                    Err(e),
                    is_json,
                    ctx.quiet,
                ),
            }
        }
        AgentCommand::Rename { agent_ref, name } => {
            let result = application::agent::rename_agent(
                project_id,
                agent_ref,
                name,
                &UnitOfWork::new(
                    conn.transaction()
                        .map_err(|e| CarryCtxError::database_error(format!("{e}")).exit_code)?,
                ),
            );
            render_and_print("agent.rename", result, is_json, ctx.quiet)
        }
        AgentCommand::Deactivate { agent_ref } => {
            let result = application::agent::deactivate_agent(
                project_id,
                agent_ref,
                &UnitOfWork::new(
                    conn.transaction()
                        .map_err(|e| CarryCtxError::database_error(format!("{e}")).exit_code)?,
                ),
            );
            render_and_print("agent.deactivate", result, is_json, ctx.quiet)
        }
    }
}
