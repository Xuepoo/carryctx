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
                Some("text") => OutputFormat::Text,
                Some("markdown") => OutputFormat::Markdown,
                Some(other) => {
                    return Err(CarryCtxError::invalid_arguments(format!(
                        "Unknown format: {other}"
                    )));
                }
                None => OutputFormat::Text,
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

        let explicit_candidate = from_cli
            .or(from_env)
            .or(from_session)
            .or(project_default_name)
            .or(global_default_name);

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

        Err(CarryCtxError::validation_error(
            "Current agent could not be resolved automatically. Specify --agent or set agent.default_name.",
        ))
    }
}
