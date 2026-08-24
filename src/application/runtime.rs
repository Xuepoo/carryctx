use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::adapter::filesystem::AdmissionLock;
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
    /// Entity fields to keep in output (from `--fields`); overrides the
    /// per-command `[output.fields]` configuration for this invocation.
    pub fields: Option<Vec<String>>,
    pub read_only: bool,
    pub admission_lock: Option<Arc<AdmissionLock>>,
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
            fields: None,
            read_only: false,
            admission_lock: None,
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
        fields: Option<Vec<String>>,
        config_compat: ConfigCompatMode,
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
            config_compat,
            no_color,
            quiet,
            verbose,
            dry_run,
            yes,
            interactive,
            fields,
            read_only: false,
            admission_lock: None,
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
    pub admission_lock: Option<Arc<AdmissionLock>>,
}

/// Fully-populated configuration instance used to harvest the known key
/// surface: `Option` and empty-map fields disappear from TOML serialization
/// when left at their defaults, so they are filled here before deriving.
fn probe_config() -> CarryCtxConfig {
    let mut config = CarryCtxConfig::default();
    config.agent.default_name = Some(String::new());
    config.agent.default_provider = Some(String::new());
    config.output.fields.insert("_probe".into(), vec![]);
    config.git.worktree_root = Some(String::new());
    config.verification.commands = vec![String::new()];
    config
}

/// Known configuration keys, derived from the domain model's defaults so the
/// list cannot drift from [`CarryCtxConfig`]: the populated instance is
/// serialized to TOML and its section/field names harvested. Depth is two
/// (`section.field`), matching the documented configuration surface; map-like
/// leaves (e.g. `output.fields`) are never descended into.
fn known_config_keys() -> std::collections::BTreeSet<String> {
    let defaults =
        toml::Value::try_from(probe_config()).expect("the probe config must serialize to TOML");
    let mut keys = std::collections::BTreeSet::new();
    if let Some(table) = defaults.as_table() {
        for (section, value) in table {
            match value.as_table() {
                Some(fields) => {
                    for field in fields.keys() {
                        if field == "_probe" {
                            continue;
                        }
                        keys.insert(format!("{section}.{field}"));
                    }
                }
                None => {
                    keys.insert(section.clone());
                }
            }
        }
    }
    keys
}

/// Unknown top-level or `section.field` keys present in a raw config file.
/// The file itself must still parse (type errors fail in the loader); this
/// pass only detects keys the domain model does not know.
pub fn unknown_config_keys(raw_toml: &str) -> Result<Vec<String>, CarryCtxError> {
    let parsed: toml::Value = toml::from_str(raw_toml)
        .map_err(|e| CarryCtxError::configuration_error(format!("Invalid config TOML: {e}")))?;
    let known = known_config_keys();
    let mut unknown = Vec::new();
    if let Some(table) = parsed.as_table() {
        for (key, value) in table {
            match value.as_table() {
                Some(fields) => {
                    for field in fields.keys() {
                        let dotted = format!("{key}.{field}");
                        if !known.contains(&dotted) {
                            unknown.push(dotted);
                        }
                    }
                }
                None => {
                    if !known.contains(key) {
                        unknown.push(key.clone());
                    }
                }
            }
        }
    }
    Ok(unknown)
}

/// Closest known key by edit distance, for the "Did you mean" hint.
fn closest_known_key(target: &str) -> Option<String> {
    fn levenshtein(a: &str, b: &str) -> usize {
        let a: Vec<char> = a.chars().collect();
        let b: Vec<char> = b.chars().collect();
        let mut prev: Vec<usize> = (0..=b.len()).collect();
        let mut cur = vec![0usize; b.len() + 1];
        for i in 1..=a.len() {
            cur[0] = i;
            for j in 1..=b.len() {
                let cost = usize::from(a[i - 1] != b[j - 1]);
                cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
            }
            std::mem::swap(&mut prev, &mut cur);
        }
        prev[b.len()]
    }

    known_config_keys()
        .into_iter()
        .map(|key| (levenshtein(target, &key), key))
        .filter(|(distance, _)| *distance <= 6)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, key)| key)
}

/// Enforce the configured compatibility mode against on-disk config files
/// (issue #105: `--config-compat` was parsed but never consumed).
///
/// In `Error` mode any unknown key fails with a `CONFIGURATION_ERROR` listing
/// every offender and a "Did you mean" hint where one exists. In `Warn` mode
/// (the default, preserving shipped behavior) offenders are logged via
/// `tracing::warn!` without failing the command.
pub fn validate_config_compat(
    mode: ConfigCompatMode,
    files: &[(&Path, &str)],
) -> Result<(), CarryCtxError> {
    let mut offenders: Vec<(String, String)> = Vec::new();
    for (path, label) in files {
        if !path.exists() {
            continue;
        }
        let raw = std::fs::read_to_string(path)
            .map_err(|e| CarryCtxError::io_error(format!("Failed to read {label} config: {e}")))?;
        for key in unknown_config_keys(&raw)? {
            offenders.push((key, (*label).to_string()));
        }
    }
    if offenders.is_empty() {
        return Ok(());
    }
    match mode {
        ConfigCompatMode::Warn => {
            for (key, label) in &offenders {
                tracing::warn!("Unknown configuration key: {key} ({label} config)");
            }
            Ok(())
        }
        ConfigCompatMode::Error => {
            let listed = offenders
                .iter()
                .map(|(key, label)| format!("{key} ({label})"))
                .collect::<Vec<_>>()
                .join(", ");
            let suggestion = offenders
                .iter()
                .map(|(key, _)| key.as_str())
                .find_map(closest_known_key)
                .map(|key| format!("\nDid you mean: {key}?"))
                .unwrap_or_default();
            Err(CarryCtxError::configuration_error(format!(
                "Unknown configuration key(s): {listed}.{suggestion}"
            )))
        }
    }
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
            admission_lock: None,
        })
    }
}

/// Resolve the current agent from command-line flags, environment variables,
/// configuration defaults, and database state.
pub struct CurrentEntityResolver<'a> {
    pub project_id: &'a str,
    pub uow: &'a UnitOfWork<'a>,
}

/// Component-wise containment check: `child` is inside (or equal to) `base`.
///
/// Unlike `str::starts_with`, `Path::starts_with` compares whole path
/// components, so `/repo/wt-x` does NOT match base `/repo/wt` while
/// `/repo/wt/sub` does. An empty worktree path never matches anything.
fn cwd_within_worktree(cwd: &str, worktree_path: &str) -> bool {
    if worktree_path.trim().is_empty() {
        return false;
    }
    std::path::Path::new(cwd).starts_with(std::path::Path::new(worktree_path))
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
                if let Some(wt) = wts.into_iter().find(|w| cwd_within_worktree(cwd, &w.path)) {
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

        // Deactivated agents must not resolve and act, even though their rows
        // are still returned by the name/id lookups.
        fn require_active(agent: Agent) -> Result<Agent, CarryCtxError> {
            if agent.status == crate::domain::agent::AgentStatus::Active {
                Ok(agent)
            } else {
                Err(CarryCtxError::permission_scope(format!(
                    "Agent '{}' is deactivated and cannot act.",
                    agent.name
                )))
            }
        }

        // 1. Explicit overrides (CLI, ENV, Active Session, or Project Config)
        let explicit_candidate = from_cli
            .or(from_env)
            .or(from_session)
            .or(project_default_name);

        if let Some(candidate) = explicit_candidate {
            if !candidate.is_empty() {
                let by_name = repo.find_by_name(self.project_id, candidate)?;
                if let Some(agent) = by_name {
                    return require_active(agent);
                }
                match repo.find_by_id(self.project_id, candidate)? {
                    Some(agent) => return require_active(agent),
                    None => {
                        return Err(CarryCtxError::resource_not_found(format!(
                            "Agent '{candidate}' was not found or is not active."
                        )));
                    }
                }
            }
        }

        // 2. Try global default candidate if provided
        if let Some(def_name) = global_default_name {
            if !def_name.is_empty()
                && let Ok(Some(agent)) = repo.find_by_name(self.project_id, def_name)
            {
                // A deactivated default must not silently fall through to
                // auto-registering a duplicate name; surface the real cause.
                return require_active(agent);
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
                None,
                None,
                serde_json::Value::Null,
                self.uow,
            );
        }

        // 5. Multiple candidates remain: surface the available agents so the
        //    caller can pick with `--agent <name>`.
        let names = active_agents
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        Err(CarryCtxError::validation_error(format!(
            "Current agent could not be resolved automatically. Multiple agents exist ({}); specify --agent <name>.",
            names
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::cwd_within_worktree;

    #[test]
    fn matches_exact_worktree_path() {
        assert!(cwd_within_worktree("/repo/wt", "/repo/wt"));
    }

    #[test]
    fn matches_nested_paths_under_worktree() {
        assert!(cwd_within_worktree("/repo/wt/src/bin", "/repo/wt"));
        // Trailing separators on either side must not matter.
        assert!(cwd_within_worktree("/repo/wt/", "/repo/wt/"));
    }

    #[test]
    fn rejects_prefix_collisions_without_component_boundary() {
        // The old string-prefix match resolved /repo/wt-x to the worktree at
        // /repo/wt; component-wise comparison must reject it.
        assert!(!cwd_within_worktree("/repo/wt-x", "/repo/wt"));
        assert!(!cwd_within_worktree("/repository", "/repo"));
        assert!(!cwd_within_worktree("/repo/foo", "/repo/f"));
    }

    #[test]
    fn rejects_empty_or_relative_bases() {
        assert!(!cwd_within_worktree("/repo", ""));
        assert!(!cwd_within_worktree("repo/wt", "/repo"));
    }
}

#[cfg(test)]
mod config_compat_tests {
    use super::*;

    #[test]
    fn known_keys_follow_the_domain_model() {
        let known = known_config_keys();
        assert!(known.contains("schema_version"));
        assert!(known.contains("session.stale_after"));
        assert!(known.contains("task.list_limit"));
        assert!(known.contains("agent.default_name"));
        assert!(known.contains("output.fields"));
        assert!(!known.contains("session.stale_minutes"));
        assert!(!known.contains("brand_new_section.foo"));
    }

    #[test]
    fn unknown_keys_are_detected_at_both_depths() {
        let raw = "stale_minutes = 5\n\n[brand_new_section]\nfoo = 1\n";
        let unknown = unknown_config_keys(raw).expect("parse must succeed");
        assert_eq!(unknown, vec!["brand_new_section.foo", "stale_minutes"]);
    }

    #[test]
    fn valid_configs_report_no_unknowns() {
        let defaults = toml::to_string_pretty(&CarryCtxConfig::default()).unwrap();
        assert!(unknown_config_keys(&defaults).unwrap().is_empty());
    }

    #[test]
    fn broken_toml_is_a_configuration_error() {
        let err = unknown_config_keys("not [valid").unwrap_err();
        assert_eq!(err.code, "CONFIGURATION_ERROR");
    }

    #[test]
    fn suggestion_matches_near_misses() {
        assert_eq!(
            closest_known_key("session.stale_minutes").as_deref(),
            Some("session.stale_after")
        );
        assert_eq!(closest_known_key("zzzzzzzz.yyyyyy"), None);
    }

    #[test]
    fn error_mode_lists_offenders_warn_mode_passes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[brand_new_section]\nfoo = 1\n").unwrap();
        let files = [(path.as_path(), "project")];
        let err = validate_config_compat(ConfigCompatMode::Error, &files).unwrap_err();
        assert_eq!(err.code, "CONFIGURATION_ERROR");
        assert!(
            err.message.contains("brand_new_section.foo"),
            "{}",
            err.message
        );

        assert!(validate_config_compat(ConfigCompatMode::Warn, &files).is_ok());
        // Missing files are skipped entirely.
        let missing_path = dir.path().join("absent.toml");
        let missing = [(missing_path.as_path(), "global")];
        assert!(validate_config_compat(ConfigCompatMode::Error, &missing).is_ok());
    }
}
