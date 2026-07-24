use std::path::{Path, PathBuf};

use crate::adapter::git::GitCli;
use crate::adapter::unit_of_work::UnitOfWork;
use crate::adapter::xdg::XdgPaths;
use crate::domain::agent::Agent;
use crate::domain::config::CarryCtxConfig;
use crate::error::CarryCtxError;
use crate::repository::agent::AgentRepository;

/// Output format
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Text,
    Json,
    Markdown,
}

/// How to handle config compatibility issues
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigCompatMode {
    Error,
    Warn,
}

/// Context of the current invocation
pub struct InvocationContext {
    pub cwd: PathBuf,
    pub project: Option<String>,
    pub config: Option<String>,
    pub profile: Option<String>,
    pub agent: Option<String>,
    pub session: Option<String>,
    pub task: Option<String>,
    pub format: OutputFormat,
    pub config_compat: ConfigCompatMode,
    pub no_color: bool,
    pub quiet: bool,
    pub verbose: bool,
    pub dry_run: bool,
    pub yes: bool,
    pub interactive: bool,
}

impl Default for InvocationContext {
    fn default() -> Self {
        Self {
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            project: None,
            config: None,
            profile: None,
            agent: None,
            session: None,
            task: None,
            format: OutputFormat::Text,
            config_compat: ConfigCompatMode::Warn,
            no_color: false,
            quiet: false,
            verbose: false,
            dry_run: false,
            yes: false,
            interactive: false,
        }
    }
}

impl InvocationContext {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cwd: PathBuf,
        project: Option<String>,
        config: Option<String>,
        profile: Option<String>,
        agent: Option<String>,
        session: Option<String>,
        task: Option<String>,
        format: Option<String>,
        json: bool,
        no_color: bool,
        quiet: bool,
        verbose: bool,
        dry_run: bool,
        yes: bool,
        interactive: bool,
    ) -> Result<Self, CarryCtxError> {
        if quiet && verbose {
            return Err(CarryCtxError::invalid_arguments(
                "Cannot use both --quiet and --verbose.",
            ));
        }
        let fmt = if json {
            OutputFormat::Json
        } else {
            match format.as_deref() {
                Some("json") => OutputFormat::Json,
                Some("markdown") => OutputFormat::Markdown,
                _ => OutputFormat::Text,
            }
        };
        Ok(Self {
            cwd,
            project,
            config,
            profile,
            agent,
            session,
            task,
            format: fmt,
            config_compat: ConfigCompatMode::Warn,
            no_color,
            quiet,
            verbose,
            dry_run,
            yes,
            interactive,
        })
    }
}

/// Runtime for a discovered project
pub struct ProjectRuntime {
    pub git_project: crate::adapter::git::GitProject,
    pub database: crate::adapter::sqlite::ProjectDatabase,
    pub config: CarryCtxConfig,
    pub xdg: XdgPaths,
    pub db_path: PathBuf,
}

impl ProjectRuntime {
    pub fn open(cwd: &Path, config: CarryCtxConfig, xdg: &XdgPaths) -> Result<Self, CarryCtxError> {
        let git = GitCli::new();
        let git_project = git.discover(cwd)?;
        let db_path = xdg.project_db(&git_project.git_common_dir);
        let database = crate::adapter::sqlite::ProjectDatabase::open(&db_path)?;
        Ok(Self {
            git_project,
            database,
            config,
            xdg: XdgPaths::new(),
            db_path,
        })
    }
}

/// Resolve the current agent from command-line flags, environment variables,
/// configuration defaults, and database state.
pub struct CurrentEntityResolver<'a> {
    pub project_id: &'a str,
    pub uow: &'a UnitOfWork<'a>,
}

impl<'a> CurrentEntityResolver<'a> {
    pub fn new(project_id: &'a str, uow: &'a UnitOfWork) -> Self {
        Self { project_id, uow }
    }

    pub fn resolve_task(
        &self,
        from_ctx: Option<&str>,
        work_dir: Option<&str>,
        agent_id: Option<&str>,
    ) -> Result<Option<crate::repository::task::TaskRecord>, CarryCtxError> {
        use crate::adapter::sqlite_repos::{
            SqliteSessionRepository, SqliteTaskRepository, SqliteWorktreeRepository,
        };
        use crate::repository::session::SessionRepository;
        use crate::repository::task::TaskRepository;
        use crate::repository::worktree::WorktreeRepository;

        let conn = self.uow.connection();
        let task_repo = SqliteTaskRepository::new(conn);

        if let Some(candidate) = from_ctx {
            if !candidate.is_empty() {
                if let Some(task) = task_repo.find_by_display_id(self.project_id, candidate)? {
                    return Ok(Some(task));
                }
                if let Some(task) = task_repo.find_by_id(self.project_id, candidate)? {
                    return Ok(Some(task));
                }
                return Err(CarryCtxError::resource_not_found(format!(
                    "Task '{candidate}' not found."
                )));
            }
        }

        let session_repo = SqliteSessionRepository::new(conn);
        if let Ok(sessions) = session_repo.list(self.project_id) {
            if let Some(active) = sessions
                .into_iter()
                .find(|s| s.state == crate::domain::session::SessionState::Active)
            {
                if let Some(tid) = active.task_id {
                    if let Ok(Some(task)) = task_repo.find_by_id(self.project_id, &tid) {
                        return Ok(Some(task));
                    }
                }
            }
        }

        if let Some(cwd) = work_dir {
            let worktree_repo = SqliteWorktreeRepository::new(conn);
            if let Ok(wts) = worktree_repo.list(self.project_id) {
                if let Some(wt) = wts.into_iter().find(|w| cwd.starts_with(&w.path)) {
                    if let Some(tid) = wt.task_id {
                        if let Ok(Some(task)) = task_repo.find_by_id(self.project_id, &tid) {
                            return Ok(Some(task));
                        }
                    }
                }
            }
        }

        if let Some(agent) = agent_id {
            let filter = crate::repository::task::TaskFilter {
                project_id: self.project_id.to_string(),
                status: Some(crate::domain::task::TaskStatus::InProgress),
                owner_agent_id: Some(agent.to_string()),
                ready: false,
                blocked: false,
                mine: None,
            };
            if let Ok(mut tasks) = task_repo.list(&filter) {
                if tasks.len() == 1 {
                    return Ok(Some(tasks.pop().unwrap()));
                }
            }
        }

        Ok(None)
    }

    pub fn resolve_agent(
        &self,
        from_cli: Option<&str>,
        from_env: Option<&str>,
        from_session: Option<&str>,
        project_default_name: Option<&str>,
        global_default_name: Option<&str>,
    ) -> Result<Agent, CarryCtxError> {
        use crate::adapter::sqlite_repos::SqliteAgentRepository;

        let conn = self.uow.connection();
        let repo = SqliteAgentRepository::new(conn);

        // 1. Explicit overrides (CLI, ENV, Active Session, or Project Config)
        let explicit_candidate = from_cli
            .or(from_env)
            .or(from_session)
            .or(project_default_name);

        if let Some(candidate) = explicit_candidate {
            if !candidate.is_empty() {
                let by_name = repo.find_by_name(self.project_id, candidate)?;
                let found = if let Some(agent) = by_name {
                    Some(agent)
                } else {
                    repo.find_by_id(self.project_id, candidate)?
                };
                return found.ok_or_else(|| {
                    CarryCtxError::resource_not_found(format!(
                        "Agent '{candidate}' was not found or is not active."
                    ))
                });
            }
        }

        // 2. Try global default candidate if provided
        if let Some(def_name) = global_default_name {
            if !def_name.is_empty() {
                if let Ok(Some(agent)) = repo.find_by_name(self.project_id, def_name) {
                    return Ok(agent);
                }
            }
        }

        // 3. Fallback to single active agent in the project database
        let active_agents = repo
            .list(&crate::repository::AgentFilter {
                project_id: self.project_id.to_string(),
                status: Some(crate::domain::agent::AgentStatus::Active),
            })?
            .into_iter()
            .filter(|a| a.status == crate::domain::agent::AgentStatus::Active)
            .collect::<Vec<_>>();

        if active_agents.len() == 1 {
            return Ok(active_agents.into_iter().next().unwrap());
        }

        // 4. Auto-register default fallback agent if database has 0 agents
        if active_agents.is_empty() {
            let fallback_name = global_default_name.unwrap_or("default");
            return crate::application::agent::register_agent(
                self.project_id,
                fallback_name,
                Some("carryctx-cli"),
                serde_json::Value::Null,
                self.uow,
            );
        }

        Err(CarryCtxError::validation_error(
            "Current agent could not be resolved automatically. Multiple agents exist; specify --agent <name>.",
        ))
    }
}
