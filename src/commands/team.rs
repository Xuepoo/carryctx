use crate::*;
use carryctx::adapter::unit_of_work::UnitOfWork;
use carryctx::application;
use carryctx::application::runtime::InvocationContext;
use carryctx::error::{CarryCtxError, ExitCode};
use clap::Parser;

#[derive(Parser, Debug)]
pub enum TeamCommand {
    Status {
        team_ref: Option<String>,
    },
    Context {
        team_ref: Option<String>,
        #[arg(long)]
        agent_for: Option<String>,
        #[arg(long)]
        task: Option<String>,
    },
    Create {
        #[arg(long)]
        name: String,
        #[arg(long)]
        commander: Option<String>,
    },
    Member {
        #[command(subcommand)]
        command: TeamMemberCommand,
    },
    Commander {
        #[command(subcommand)]
        command: TeamCommanderCommand,
    },
}

#[derive(Parser, Debug)]
pub enum TeamMemberCommand {
    Add {
        team_ref: String,
        #[arg(long)]
        agent: String,
        #[arg(long)]
        role: Option<String>,
    },
    Remove {
        team_ref: String,
        #[arg(long)]
        agent: String,
    },
}

#[derive(Parser, Debug)]
pub enum TeamCommanderCommand {
    Set {
        team_ref: String,
        #[arg(long, conflicts_with = "clear", required_unless_present = "clear")]
        agent: Option<String>,
        #[arg(long)]
        clear: bool,
    },
}

#[derive(Parser, Debug)]
pub struct TeamArgs {
    #[command(subcommand)]
    pub command: TeamCommand,
}

#[derive(serde::Serialize)]
struct TeamCreateData {
    team: carryctx::domain::team::Team,
}
#[derive(serde::Serialize)]
struct MemberData {
    member: carryctx::domain::team::TeamMember,
}

pub fn handle_team(
    args: &TeamArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    if !is_json {
        if let Some(result) = check_dry_run(ctx, &format!("team {:?}", args.command)) {
            return result;
        }
    }
    let mut runtime = try_open_runtime(ctx)?;
    let project_id = runtime.config.project.id.clone();
    let verbose = ctx.verbose || runtime.config.output.verbose;
    let conn = runtime.database.connection_mut();
    if let TeamCommand::Context {
        team_ref,
        agent_for,
        task,
    } = &args.command
    {
        let result =
            (|| -> Result<carryctx::domain::team::TeamContextProjection, CarryCtxError> {
                let agent_id = agent_for
                    .as_deref()
                    .map(|reference| resolve_agent_id(&project_id, reference, conn))
                    .transpose()?;
                let task_id = task
                    .as_deref()
                    .map(|reference| resolve_task_id(&project_id, reference, conn))
                    .transpose()?;
                let resolved_team = if let Some(reference) = team_ref {
                    resolve_team_id(&project_id, reference, conn).map_err(|error| {
                        if error.code == "RESOURCE_NOT_FOUND" {
                            CarryCtxError::new(
                                "TEAM_NOT_FOUND",
                                format!("Team '{reference}' not found."),
                                ExitCode::ResourceNotFound,
                            )
                        } else {
                            error
                        }
                    })?
                } else {
                    application::team::resolve_context_team(
                        &project_id,
                        task_id.as_deref(),
                        agent_id.as_deref(),
                        conn,
                    )?
                };
                application::team::context(
                    &project_id,
                    &resolved_team,
                    agent_id.as_deref(),
                    task_id.as_deref(),
                    ctx.session.as_deref(),
                    conn,
                )
            })();
        return render_and_print_entity(
            "team.context",
            result,
            is_json,
            ctx.quiet,
            verbose,
            ctx.fields.as_deref(),
            Some(&runtime.config.output.fields),
        );
    }
    if let TeamCommand::Status { team_ref } = &args.command {
        let result = match team_ref {
            Some(team_ref) => resolve_team_id(&project_id, team_ref, conn)
                .map_err(|error| {
                    if error.code == "RESOURCE_NOT_FOUND" {
                        CarryCtxError::new(
                            "TEAM_NOT_FOUND",
                            format!("Team '{team_ref}' not found."),
                            ExitCode::ResourceNotFound,
                        )
                    } else {
                        error
                    }
                })
                .and_then(|team_id| application::team::status(&project_id, &team_id, conn))
                .map(|projection| {
                    serde_json::json!({
                        "team": projection.team,
                        "members": projection.members,
                        "counts": projection.counts,
                    })
                }),
            None => application::team::list_status(&project_id, conn)
                .map(|teams| serde_json::json!({"teams": teams})),
        };
        return render_and_print_entity(
            "team.status",
            result,
            is_json,
            ctx.quiet,
            verbose,
            ctx.fields.as_deref(),
            Some(&runtime.config.output.fields),
        );
    }
    if ctx.dry_run && is_json {
        let (command, data) = match &args.command {
            TeamCommand::Status { .. } => unreachable!("team status handled above"),
            TeamCommand::Context { .. } => unreachable!("team context handled above"),
            TeamCommand::Create { commander, .. } => (
                "team.create",
                serde_json::json!({
                    "commander_agent_id": commander.as_deref()
                        .map(|reference| resolve_agent_id(&project_id, reference, conn))
                        .transpose()
                        .map_err(|e| e.exit_code)?,
                    "operation": {"applied": false}
                }),
            ),
            TeamCommand::Member {
                command:
                    TeamMemberCommand::Add {
                        team_ref, agent, ..
                    },
            } => (
                "team.member_add",
                serde_json::json!({
                    "team_id": resolve_team_id(&project_id, team_ref, conn).map_err(|e| e.exit_code)?,
                    "agent_id": resolve_agent_id(&project_id, agent, conn).map_err(|e| e.exit_code)?,
                    "operation": {"applied": false}
                }),
            ),
            TeamCommand::Member {
                command: TeamMemberCommand::Remove { team_ref, agent },
            } => (
                "team.member_remove",
                serde_json::json!({
                    "team_id": resolve_team_id(&project_id, team_ref, conn).map_err(|e| e.exit_code)?,
                    "agent_id": resolve_agent_id(&project_id, agent, conn).map_err(|e| e.exit_code)?,
                    "operation": {"applied": false}
                }),
            ),
            TeamCommand::Commander {
                command:
                    TeamCommanderCommand::Set {
                        team_ref,
                        agent,
                        clear,
                    },
            } => (
                "team.commander_set",
                serde_json::json!({
                    "team_id": resolve_team_id(&project_id, team_ref, conn).map_err(|e| e.exit_code)?,
                    "commander_agent_id": if *clear { None::<String> } else {
                        agent.as_deref().map(|reference| resolve_agent_id(&project_id, reference, conn)).transpose().map_err(|e| e.exit_code)?
                    },
                    "operation": {"applied": false}
                }),
            ),
        };
        return render_and_print_entity::<serde_json::Value>(
            command,
            Ok(data),
            true,
            ctx.quiet,
            false,
            None,
            None,
        );
    }
    match &args.command {
        TeamCommand::Status { .. } => unreachable!("team status handled above"),
        TeamCommand::Context { .. } => unreachable!("team context handled above"),
        TeamCommand::Create { name, commander } => {
            let uow = UnitOfWork::begin(conn).map_err(|e| e.exit_code)?;
            let commander_id = match commander
                .as_deref()
                .map(|r| resolve_agent_id(&project_id, r, uow.connection()))
                .transpose()
            {
                Ok(id) => id,
                Err(e) => {
                    return render_and_print_entity::<serde_json::Value>(
                        "team.create",
                        Err(e),
                        is_json,
                        ctx.quiet,
                        verbose,
                        ctx.fields.as_deref(),
                        Some(&runtime.config.output.fields),
                    );
                }
            };
            let result = application::team::create_team(
                &project_id,
                name,
                commander_id.as_deref(),
                ctx.agent.as_deref(),
                &uow,
            )
            .map(|team| TeamCreateData { team });
            let result = result.and_then(|data| uow.commit().map(|_| data));
            render_and_print_entity(
                "team.create",
                result,
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        TeamCommand::Member {
            command:
                TeamMemberCommand::Add {
                    team_ref,
                    agent,
                    role,
                },
        } => {
            let uow = UnitOfWork::begin(conn).map_err(|e| e.exit_code)?;
            let team_id = match resolve_team_id(&project_id, team_ref, uow.connection()) {
                Ok(id) => id,
                Err(e) => {
                    return render_and_print_entity::<serde_json::Value>(
                        "team.member_add",
                        Err(e),
                        is_json,
                        ctx.quiet,
                        verbose,
                        ctx.fields.as_deref(),
                        Some(&runtime.config.output.fields),
                    );
                }
            };
            let agent_id = match resolve_agent_id(&project_id, agent, uow.connection()) {
                Ok(id) => id,
                Err(e) => {
                    return render_and_print_entity::<serde_json::Value>(
                        "team.member_add",
                        Err(e),
                        is_json,
                        ctx.quiet,
                        verbose,
                        ctx.fields.as_deref(),
                        Some(&runtime.config.output.fields),
                    );
                }
            };
            let result = application::team::add_member(
                &project_id,
                &team_id,
                &agent_id,
                role.as_deref(),
                ctx.agent.as_deref(),
                &uow,
            )
            .map(|member| MemberData { member });
            let result = result.and_then(|data| uow.commit().map(|_| data));
            render_and_print_entity(
                "team.member_add",
                result,
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        TeamCommand::Member {
            command: TeamMemberCommand::Remove { team_ref, agent },
        } => {
            let uow = UnitOfWork::begin(conn).map_err(|e| e.exit_code)?;
            let team_id = match resolve_team_id(&project_id, team_ref, uow.connection()) {
                Ok(id) => id,
                Err(e) => {
                    return render_and_print_entity::<serde_json::Value>(
                        "team.member_remove",
                        Err(e),
                        is_json,
                        ctx.quiet,
                        verbose,
                        ctx.fields.as_deref(),
                        Some(&runtime.config.output.fields),
                    );
                }
            };
            let agent_id = match resolve_agent_id(&project_id, agent, uow.connection()) {
                Ok(id) => id,
                Err(e) => {
                    return render_and_print_entity::<serde_json::Value>(
                        "team.member_remove",
                        Err(e),
                        is_json,
                        ctx.quiet,
                        verbose,
                        ctx.fields.as_deref(),
                        Some(&runtime.config.output.fields),
                    );
                }
            };
            let result = application::team::remove_member(&project_id, &team_id, &agent_id, ctx.agent.as_deref(), &uow).map(|_| serde_json::json!({"member": {"team_id": team_id, "agent_id": agent_id}, "operation": {"applied": true}}));
            let result = result.and_then(|data| uow.commit().map(|_| data));
            render_and_print_entity(
                "team.member_remove",
                result,
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
        TeamCommand::Commander {
            command:
                TeamCommanderCommand::Set {
                    team_ref,
                    agent,
                    clear,
                },
        } => {
            let uow = UnitOfWork::begin(conn).map_err(|e| e.exit_code)?;
            let team_id = match resolve_team_id(&project_id, team_ref, uow.connection()) {
                Ok(id) => id,
                Err(e) => {
                    return render_and_print_entity::<serde_json::Value>(
                        "team.commander_set",
                        Err(e),
                        is_json,
                        ctx.quiet,
                        verbose,
                        ctx.fields.as_deref(),
                        Some(&runtime.config.output.fields),
                    );
                }
            };
            let agent_id = match agent
                .as_deref()
                .map(|r| resolve_agent_id(&project_id, r, uow.connection()))
                .transpose()
            {
                Ok(id) => id,
                Err(e) => {
                    return render_and_print_entity::<serde_json::Value>(
                        "team.commander_set",
                        Err(e),
                        is_json,
                        ctx.quiet,
                        verbose,
                        ctx.fields.as_deref(),
                        Some(&runtime.config.output.fields),
                    );
                }
            };
            let selected = if *clear { None } else { agent_id.as_deref() };
            let result = application::team::set_commander(&project_id, &team_id, selected, ctx.agent.as_deref(), &uow).map(|team| serde_json::json!({"team": team, "commander": selected, "operation": {"applied": true}}));
            let result = result.and_then(|data| uow.commit().map(|_| data));
            render_and_print_entity(
                "team.commander_set",
                result,
                is_json,
                ctx.quiet,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )
        }
    }
}

pub fn resolve_team_id(
    project_id: &str,
    team_ref: &str,
    conn: &rusqlite::Connection,
) -> Result<String, CarryCtxError> {
    use carryctx::repository::TeamRepository;
    let repo = carryctx::adapter::sqlite_repos::SqliteTeamRepository::new(conn);
    if let Some(team) = repo.find_by_id(project_id, team_ref)? {
        return Ok(team.id);
    }
    if let Some(team) = repo.find_by_name(project_id, team_ref)? {
        return Ok(team.id);
    }
    Err(CarryCtxError::resource_not_found(format!(
        "Team '{team_ref}' not found."
    )))
}
