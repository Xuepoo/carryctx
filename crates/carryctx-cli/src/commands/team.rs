use super::{print_markdown_result, render_dry_run_error, resolve_or_render, truncate_chars};
use crate::adapter::unit_of_work::UnitOfWork;
use crate::application;
use crate::application::runtime::{InvocationContext, OutputFormat, ProjectRuntime};
use crate::cli::{
    check_dry_run, open_runtime_or_report, render_and_print_entity, resolve_agent_id,
    resolve_task_id,
};
use crate::error::{CarryCtxError, ExitCode};
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
    team: crate::domain::team::Team,
}
#[derive(serde::Serialize)]
struct MemberData {
    member: crate::domain::team::TeamMember,
}

pub fn handle_team(
    args: &TeamArgs,
    pre_opened: Option<ProjectRuntime>,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    if !is_json {
        if let Some(result) = check_dry_run(ctx, &format!("team {:?}", args.command)) {
            return result;
        }
    }
    // Reuse the dispatcher's pre-opened runtime when available; a second
    // open only happens (and reports) when that failed.
    let mut runtime = match pre_opened {
        Some(runtime) => runtime,
        None => open_runtime_or_report(ctx, "team")?,
    };
    let project_id = runtime.config.project.id.clone();
    let verbose = ctx.verbose || runtime.config.output.verbose;
    let conn = runtime.database.connection_mut();
    if let TeamCommand::Context {
        team_ref,
        agent_for,
        task,
    } = &args.command
    {
        let result = (|| -> Result<crate::domain::team::TeamContextProjection, CarryCtxError> {
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
        if ctx.format == OutputFormat::Markdown {
            return print_markdown_result(
                "team.context",
                result,
                |projection| {
                    let value = serde_json::to_value(projection).unwrap_or_default();
                    render_team_context_markdown(&value)
                },
                ctx,
            );
        }
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
        if ctx.format == OutputFormat::Markdown {
            return print_markdown_result(
                "team.status",
                result,
                |value| render_team_status_markdown(&value),
                ctx,
            );
        }
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
                        .map_err(|e| render_dry_run_error("team.create", e, ctx))?,
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
                    "team_id": resolve_team_id(&project_id, team_ref, conn)
                        .map_err(|e| render_dry_run_error("team.member_add", e, ctx))?,
                    "agent_id": resolve_agent_id(&project_id, agent, conn)
                        .map_err(|e| render_dry_run_error("team.member_add", e, ctx))?,
                    "operation": {"applied": false}
                }),
            ),
            TeamCommand::Member {
                command: TeamMemberCommand::Remove { team_ref, agent },
            } => (
                "team.member_remove",
                serde_json::json!({
                    "team_id": resolve_team_id(&project_id, team_ref, conn)
                        .map_err(|e| render_dry_run_error("team.member_remove", e, ctx))?,
                    "agent_id": resolve_agent_id(&project_id, agent, conn)
                        .map_err(|e| render_dry_run_error("team.member_remove", e, ctx))?,
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
                    "team_id": resolve_team_id(&project_id, team_ref, conn)
                        .map_err(|e| render_dry_run_error("team.commander_set", e, ctx))?,
                    "commander_agent_id": if *clear { None::<String> } else {
                        agent.as_deref().map(|reference| resolve_agent_id(&project_id, reference, conn)).transpose().map_err(|e| render_dry_run_error("team.commander_set", e, ctx))?
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
            let team_id = resolve_or_render(
                "team.member_add",
                resolve_team_id(&project_id, team_ref, uow.connection()),
                ctx,
                is_json,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )?;
            let agent_id = resolve_or_render(
                "team.member_add",
                resolve_agent_id(&project_id, agent, uow.connection()),
                ctx,
                is_json,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )?;
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
            let team_id = resolve_or_render(
                "team.member_remove",
                resolve_team_id(&project_id, team_ref, uow.connection()),
                ctx,
                is_json,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )?;
            let agent_id = resolve_or_render(
                "team.member_remove",
                resolve_agent_id(&project_id, agent, uow.connection()),
                ctx,
                is_json,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )?;
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
            let team_id = resolve_or_render(
                "team.commander_set",
                resolve_team_id(&project_id, team_ref, uow.connection()),
                ctx,
                is_json,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )?;
            let agent_id = resolve_or_render(
                "team.commander_set",
                agent
                    .as_deref()
                    .map(|r| resolve_agent_id(&project_id, r, uow.connection()))
                    .transpose(),
                ctx,
                is_json,
                verbose,
                ctx.fields.as_deref(),
                Some(&runtime.config.output.fields),
            )?;
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

/// Escape a value for a single GFM table cell: pipe characters must be
/// escaped and embedded newlines collapsed so the table stays valid.
fn md_cell(value: &str) -> String {
    value.replace(['\n', '\r'], " ").replace('|', "\\|")
}

const TEAM_STATUS_TABLE_HEADER: &str = "| Team | Commander | Members | Commanders | Subagents | Active tasks |\n|---|---|---|---|---|---|\n";

fn commander_display(team: &serde_json::Value, projection: &serde_json::Value) -> String {
    let Some(commander_id) = team
        .get("commander_agent_id")
        .and_then(serde_json::Value::as_str)
    else {
        return "-".to_string();
    };
    if commander_id.is_empty() {
        return "-".to_string();
    }
    if let Some(members) = projection
        .get("members")
        .and_then(serde_json::Value::as_array)
    {
        for member in members {
            if member.get("agent_id").and_then(serde_json::Value::as_str) == Some(commander_id)
                && let Some(name) = member.get("name").and_then(serde_json::Value::as_str)
                && !name.is_empty()
            {
                return md_cell(name);
            }
        }
    }
    md_cell(&truncate_chars(commander_id, 8))
}

fn team_status_row(projection: &serde_json::Value) -> String {
    let team = projection
        .get("team")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let name = md_cell(
        team.get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(""),
    );
    let counts = projection
        .get("counts")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let total = counts
        .get("total")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let commanders = counts
        .get("commanders")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let subagents = counts
        .get("subagents")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let active: u64 = projection
        .get("members")
        .and_then(serde_json::Value::as_array)
        .map(|members| {
            members
                .iter()
                .filter_map(|member| {
                    member
                        .get("active_task_count")
                        .and_then(serde_json::Value::as_u64)
                })
                .sum()
        })
        .unwrap_or(0);
    let commander = commander_display(&team, projection);
    format!("| {name} | {commander} | {total} | {commanders} | {subagents} | {active} |\n")
}

fn render_team_status_markdown(value: &serde_json::Value) -> String {
    let mut out = String::new();
    if let Some(teams) = value.get("teams").and_then(serde_json::Value::as_array) {
        out.push_str("# Teams\n\n");
        out.push_str(TEAM_STATUS_TABLE_HEADER);
        for projection in teams {
            out.push_str(&team_status_row(projection));
        }
    } else {
        out.push_str("# Team status\n\n");
        out.push_str(TEAM_STATUS_TABLE_HEADER);
        out.push_str(&team_status_row(value));
    }
    out
}

fn render_team_context_markdown(value: &serde_json::Value) -> String {
    let team_name = value
        .get("team")
        .and_then(|team| team.get("name"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let view = value
        .get("view")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let rebuild = value
        .get("rebuild")
        .and_then(|rebuild| rebuild.get("source"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let mut out = format!("# Team context: {}\n\n", md_cell(team_name));
    out.push_str(&format!("- view: {}\n", md_cell(view)));
    out.push_str(&format!("- rebuild: {}\n", md_cell(rebuild)));

    let tasks = value.get("tasks").and_then(serde_json::Value::as_array);
    if let Some(members) = value.get("members").and_then(serde_json::Value::as_array)
        && !members.is_empty()
    {
        out.push_str("\n## Members\n\n");
        out.push_str("| Agent | Kind | Role | Active session | Tasks |\n");
        out.push_str("|---|---|---|---|---|\n");
        for member in members {
            let agent_id = member
                .get("agent_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let name = md_cell(
                member
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(""),
            );
            let kind = md_cell(
                member
                    .get("kind")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("-"),
            );
            let role = md_cell(
                member
                    .get("role")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("-"),
            );
            let task_count = tasks
                .map(|tasks| {
                    tasks
                        .iter()
                        .filter(|task| {
                            task.get("owner_agent_id")
                                .and_then(serde_json::Value::as_str)
                                == Some(agent_id)
                        })
                        .count()
                })
                .unwrap_or(0);
            out.push_str(&format!(
                "| {name} | {kind} | {role} | - | {task_count} |\n"
            ));
        }
    }

    if let Some(tasks) = tasks
        && !tasks.is_empty()
    {
        out.push_str("\n## Tasks\n\n");
        out.push_str("| Task | Status | Owner |\n");
        out.push_str("|---|---|---|\n");
        for task in tasks {
            let id = md_cell(
                task.get("display_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(""),
            );
            let status = md_cell(
                task.get("status")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(""),
            );
            let owner = task
                .get("owner_agent_id")
                .and_then(serde_json::Value::as_str)
                .map(|owner| md_cell(&truncate_chars(owner, 8)))
                .unwrap_or_else(|| "-".to_string());
            out.push_str(&format!("| {id} | {status} | {owner} |\n"));
        }
    }

    if let Some(blockers) = value.get("blockers").and_then(serde_json::Value::as_array)
        && !blockers.is_empty()
    {
        out.push_str("\n## Blockers\n\n");
        out.push_str("| Task | Content |\n");
        out.push_str("|---|---|\n");
        for blocker in blockers {
            let task = blocker
                .get("task_id")
                .and_then(serde_json::Value::as_str)
                .map(|id| truncate_chars(id, 8))
                .unwrap_or_default();
            let content = md_cell(
                blocker
                    .get("content")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(""),
            );
            out.push_str(&format!("| {task} | {content} |\n"));
        }
    }

    if let Some(events) = value
        .get("recent_events")
        .and_then(serde_json::Value::as_array)
        && !events.is_empty()
    {
        out.push_str("\n## Recent events\n\n");
        out.push_str("| When | Event | Task |\n");
        out.push_str("|---|---|---|\n");
        for event in events {
            let occurred = event
                .get("occurred_at")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let when = md_cell(&truncate_chars(occurred, 16));
            let event_type = md_cell(
                event
                    .get("event_type")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(""),
            );
            let task = event
                .get("task_id")
                .and_then(serde_json::Value::as_str)
                .map(|id| md_cell(&truncate_chars(id, 8)))
                .unwrap_or_else(|| "-".to_string());
            out.push_str(&format!("| {when} | {event_type} | {task} |\n"));
        }
    }

    out
}

pub fn resolve_team_id(
    project_id: &str,
    team_ref: &str,
    conn: &rusqlite::Connection,
) -> Result<String, CarryCtxError> {
    use crate::repository::TeamRepository;
    let repo = crate::adapter::sqlite_repos::SqliteTeamRepository::new(conn);
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
