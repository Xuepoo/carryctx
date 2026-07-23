use std::path::Path;

use crate::adapter::config::{find_project_config_dir, ConfigLoader};
use crate::adapter::filesystem;
use crate::adapter::xdg::XdgPaths;
use crate::domain::config::CarryCtxConfig;
use crate::error::CarryCtxError;

#[derive(Debug, Clone, serde::Serialize)]
pub struct ConfigSource {
    pub layer: String,
    pub path: Option<String>,
    pub exists: bool,
    pub keys: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ConfigValidation {
    pub valid: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

fn get_all_keys(config: &CarryCtxConfig) -> Vec<String> {
    let mut keys = Vec::new();
    keys.push(format!("project.id = {}", config.project.id));
    keys.push(format!("project.name = {}", config.project.name));
    keys.push(format!(
        "project.task_prefix = {}",
        config.project.task_prefix
    ));
    keys.push(format!("git.main_branch = {}", config.git.main_branch));
    keys.push(format!(
        "git.worktree_root = {:?}",
        config.git.worktree_root
    ));
    keys.push(format!(
        "git.branch_template = {}",
        config.git.branch_template
    ));
    keys.push(format!(
        "session.stale_after = {}",
        config.session.stale_after
    ));
    keys.push(format!(
        "session.single_active_session_per_agent = {}",
        config.session.single_active_session_per_agent
    ));
    keys.push(format!(
        "task.single_active_task_per_agent = {}",
        config.task.single_active_task_per_agent
    ));
    keys.push(format!(
        "task.strict_completion = {}",
        config.task.strict_completion
    ));
    keys.push(format!(
        "context.default_mode = {}",
        config.context.default_mode
    ));
    keys.push(format!(
        "context.max_events = {}",
        config.context.max_events
    ));
    keys.push(format!("context.lookback = {}", config.context.lookback));
    keys.push(format!(
        "context.include_git_status = {}",
        config.context.include_git_status
    ));
    keys.push(format!(
        "checkpoint.require_before_session_end = {}",
        config.checkpoint.require_before_session_end
    ));
    keys.push(format!(
        "checkpoint.capture_diff_stats = {}",
        config.checkpoint.capture_diff_stats
    ));
    keys.push(format!(
        "checkpoint.capture_untracked_files = {}",
        config.checkpoint.capture_untracked_files
    ));
    keys.push(format!("output.color = {}", config.output.color));
    keys.push(format!("output.unicode = {}", config.output.unicode));
    if let Some(ref name) = config.agent.default_name {
        keys.push(format!("agent.default_name = {name}"));
    }
    if let Some(ref provider) = config.agent.default_provider {
        keys.push(format!("agent.default_provider = {provider}"));
    }
    keys
}

pub fn get_config_value(
    key: &str,
    project_path: Option<&Path>,
) -> Result<Option<String>, CarryCtxError> {
    let xdg = XdgPaths::new();
    let loader = ConfigLoader::new(xdg);
    let config = loader.load(project_path)?;

    let value = match key {
        "project.id" => Some(config.project.id),
        "project.name" => Some(config.project.name),
        "project.task_prefix" => Some(config.project.task_prefix),
        "git.main_branch" => Some(config.git.main_branch),
        "git.worktree_root" => config.git.worktree_root,
        "git.branch_template" => Some(config.git.branch_template),
        "session.stale_after" => Some(config.session.stale_after),
        "session.single_active_session_per_agent" => {
            Some(config.session.single_active_session_per_agent.to_string())
        }
        "task.single_active_task_per_agent" => {
            Some(config.task.single_active_task_per_agent.to_string())
        }
        "task.strict_completion" => Some(config.task.strict_completion.to_string()),
        "context.default_mode" => Some(config.context.default_mode),
        "context.max_events" => Some(config.context.max_events.to_string()),
        "context.lookback" => Some(config.context.lookback),
        "context.include_git_status" => Some(config.context.include_git_status.to_string()),
        "checkpoint.require_before_session_end" => {
            Some(config.checkpoint.require_before_session_end.to_string())
        }
        "checkpoint.capture_diff_stats" => Some(config.checkpoint.capture_diff_stats.to_string()),
        "checkpoint.capture_untracked_files" => {
            Some(config.checkpoint.capture_untracked_files.to_string())
        }
        "output.color" => Some(config.output.color),
        "output.unicode" => Some(config.output.unicode.to_string()),
        "agent.default_name" => config.agent.default_name,
        "agent.default_provider" => config.agent.default_provider,
        _ => {
            return Err(CarryCtxError::invalid_arguments(format!(
                "Unknown config key: {key}"
            )));
        }
    };

    Ok(value)
}

pub fn set_config_value(key: &str, value: &str, project_path: &Path) -> Result<(), CarryCtxError> {
    let config_dir = find_project_config_dir(project_path).ok_or_else(|| {
        CarryCtxError::resource_not_found("No .carryctx configuration directory found in project.")
    })?;

    let config_path = config_dir.join(".carryctx").join("config.toml");

    let xdg = XdgPaths::new();
    let loader = ConfigLoader::new(xdg);
    let mut config = loader.load(Some(&config_dir)).unwrap_or_default();

    match key {
        "project.name" => config.project.name = value.to_string(),
        "project.task_prefix" => config.project.task_prefix = value.to_string(),
        "git.main_branch" => config.git.main_branch = value.to_string(),
        "git.worktree_root" => config.git.worktree_root = Some(value.to_string()),
        "git.branch_template" => config.git.branch_template = value.to_string(),
        "session.stale_after" => config.session.stale_after = value.to_string(),
        "session.single_active_session_per_agent" => {
            config.session.single_active_session_per_agent = value.parse().unwrap_or(true);
        }
        "task.single_active_task_per_agent" => {
            config.task.single_active_task_per_agent = value.parse().unwrap_or(true);
        }
        "task.strict_completion" => {
            config.task.strict_completion = value.parse().unwrap_or(false);
        }
        "context.default_mode" => config.context.default_mode = value.to_string(),
        "context.max_events" => {
            config.context.max_events = value.parse().unwrap_or(10);
        }
        "context.lookback" => config.context.lookback = value.to_string(),
        "context.include_git_status" => {
            config.context.include_git_status = value.parse().unwrap_or(true);
        }
        "checkpoint.require_before_session_end" => {
            config.checkpoint.require_before_session_end = value.parse().unwrap_or(true);
        }
        "checkpoint.capture_diff_stats" => {
            config.checkpoint.capture_diff_stats = value.parse().unwrap_or(true);
        }
        "checkpoint.capture_untracked_files" => {
            config.checkpoint.capture_untracked_files = value.parse().unwrap_or(true);
        }
        "output.color" => config.output.color = value.to_string(),
        "output.unicode" => config.output.unicode = value.parse().unwrap_or(true),
        "agent.default_name" => config.agent.default_name = Some(value.to_string()),
        "agent.default_provider" => config.agent.default_provider = Some(value.to_string()),
        _ => {
            return Err(CarryCtxError::invalid_arguments(format!(
                "Unknown or read-only config key: {key}"
            )));
        }
    }

    let toml_str = toml::to_string_pretty(&config).map_err(|e| {
        CarryCtxError::configuration_error(format!("Failed to serialize config: {e}"))
    })?;
    filesystem::write_atomic(&config_path, toml_str.as_bytes())?;

    Ok(())
}

pub fn unset_config_value(key: &str, project_path: &Path) -> Result<(), CarryCtxError> {
    let config_dir = find_project_config_dir(project_path).ok_or_else(|| {
        CarryCtxError::resource_not_found("No .carryctx configuration directory found in project.")
    })?;

    let config_path = config_dir.join(".carryctx").join("config.toml");

    let xdg = XdgPaths::new();
    let loader = ConfigLoader::new(xdg);
    let mut config = loader.load(Some(&config_dir)).unwrap_or_default();

    match key {
        "git.worktree_root" => config.git.worktree_root = None,
        "agent.default_name" => config.agent.default_name = None,
        "agent.default_provider" => config.agent.default_provider = None,
        _ => {
            return Err(CarryCtxError::invalid_arguments(format!(
                "Config key '{key}' cannot be unset or is read-only."
            )));
        }
    }

    let toml_str = toml::to_string_pretty(&config).map_err(|e| {
        CarryCtxError::configuration_error(format!("Failed to serialize config: {e}"))
    })?;
    filesystem::write_atomic(&config_path, toml_str.as_bytes())?;

    Ok(())
}

pub fn list_config_sources(
    project_path: Option<&Path>,
) -> Result<Vec<ConfigSource>, CarryCtxError> {
    let xdg = XdgPaths::new();
    let mut sources = Vec::new();

    // Global config
    let global_path = xdg.global_config();
    let global_exists = global_path.exists();
    let global_keys = load_and_get_keys(&xdg, None);

    sources.push(ConfigSource {
        layer: "global".into(),
        path: Some(global_path.to_string_lossy().to_string()),
        exists: global_exists,
        keys: global_keys,
    });

    // Project config
    if let Some(proj_path) = project_path {
        let project_config = proj_path.join(".carryctx").join("config.toml");
        let proj_exists = project_config.exists();
        let proj_keys = load_and_get_keys(&xdg, Some(proj_path));

        sources.push(ConfigSource {
            layer: "project".into(),
            path: Some(project_config.to_string_lossy().to_string()),
            exists: proj_exists,
            keys: proj_keys,
        });
    }

    // Environment overrides
    let env_keys: Vec<String> = std::env::vars()
        .filter(|(k, _)| k.starts_with("CARRYCTX_"))
        .map(|(k, v)| format!("{k} = {v}"))
        .collect();

    sources.push(ConfigSource {
        layer: "environment".into(),
        path: None,
        exists: !env_keys.is_empty(),
        keys: env_keys,
    });

    // Defaults
    let defaults = CarryCtxConfig::default();
    let default_keys = get_all_keys(&defaults);

    sources.push(ConfigSource {
        layer: "defaults".into(),
        path: None,
        exists: true,
        keys: default_keys,
    });

    Ok(sources)
}

fn load_and_get_keys(xdg: &XdgPaths, project_path: Option<&Path>) -> Vec<String> {
    let loader = ConfigLoader::new(xdg.clone());
    match loader.load(project_path) {
        Ok(config) => get_all_keys(&config),
        Err(_) => vec![],
    }
}

pub fn validate_config(project_path: Option<&Path>) -> Result<ConfigValidation, CarryCtxError> {
    let xdg = XdgPaths::new();
    let loader = ConfigLoader::new(xdg);

    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    match loader.load(project_path) {
        Ok(config) => {
            if config.project.name.is_empty() {
                warnings.push("Project name is not set.".into());
            }

            if config.project.task_prefix.is_empty() {
                errors.push("Task prefix cannot be empty.".into());
            } else if config.project.task_prefix.len() > 10 {
                errors.push("Task prefix must be 10 characters or fewer.".into());
            }

            if config.session.stale_after.is_empty() {
                errors.push("Session stale_after cannot be empty.".into());
            }

            if config.git.main_branch.is_empty() {
                errors.push("Git main_branch cannot be empty.".into());
            }

            if config.context.max_events == 0 {
                warnings.push("context.max_events is 0; events will be suppressed.".into());
            }
        }
        Err(e) => {
            errors.push(format!("Failed to load configuration: {e}"));
        }
    }

    Ok(ConfigValidation {
        valid: errors.is_empty(),
        errors,
        warnings,
    })
}
