use std::collections::HashMap;
use std::path::Path;

use crate::domain::config::CarryCtxConfig;
use crate::error::CarryCtxError;

pub struct ConfigLoader {
    env_overrides: HashMap<String, String>,
    xdg_paths: crate::adapter::xdg::XdgPaths,
}

impl ConfigLoader {
    pub fn new(xdg_paths: crate::adapter::xdg::XdgPaths) -> Self {
        let env_overrides = std::env::vars()
            .filter(|(k, _)| k.starts_with("CARRYCTX_"))
            .collect();
        Self {
            env_overrides,
            xdg_paths,
        }
    }

    pub fn load(&self, project_config_dir: Option<&Path>) -> Result<CarryCtxConfig, CarryCtxError> {
        let mut config = CarryCtxConfig::default();

        let global_path = self.xdg_paths.global_config();
        if global_path.exists() {
            let global_toml = std::fs::read_to_string(&global_path).map_err(|e| {
                CarryCtxError::configuration_error(format!("Failed to read global config: {}", e))
            })?;
            let global: CarryCtxConfig = toml::from_str(&global_toml).map_err(|e| {
                CarryCtxError::configuration_error(format!("Invalid global config: {}", e))
            })?;
            merge_config(&mut config, global);
        }

        if let Some(project_dir) = project_config_dir {
            let project_config = project_dir.join(".carryctx").join("config.toml");
            if project_config.exists() {
                let project_toml = std::fs::read_to_string(&project_config).map_err(|e| {
                    CarryCtxError::configuration_error(format!(
                        "Failed to read project config: {}",
                        e
                    ))
                })?;
                let project: CarryCtxConfig = toml::from_str(&project_toml).map_err(|e| {
                    CarryCtxError::configuration_error(format!("Invalid project config: {}", e))
                })?;
                merge_config(&mut config, project);
            }
        }

        apply_env_overrides(&mut config, &self.env_overrides);

        Ok(config)
    }
}

fn merge_config(base: &mut CarryCtxConfig, overlay: CarryCtxConfig) {
    macro_rules! merge_str {
        ($target:expr, $source:expr, $default:expr) => {
            if $source != $default && !$source.is_empty() {
                $target = $source;
            }
        };
    }
    macro_rules! merge_bool {
        ($target:expr, $source:expr, $default:expr) => {
            if $source != $default {
                $target = $source;
            }
        };
    }

    merge_str!(base.project.id, overlay.project.id, "carryctx");
    merge_str!(base.project.name, overlay.project.name, "");
    merge_str!(base.project.task_prefix, overlay.project.task_prefix, "CTX");
    merge_str!(base.git.main_branch, overlay.git.main_branch, "main");
    if overlay.git.worktree_root.is_some() {
        base.git.worktree_root = overlay.git.worktree_root;
    }
    merge_str!(
        base.git.branch_template,
        overlay.git.branch_template,
        "carryctx/{task_id}-{slug}"
    );
    merge_str!(base.session.stale_after, overlay.session.stale_after, "2h");
    merge_bool!(
        base.session.single_active_session_per_agent,
        overlay.session.single_active_session_per_agent,
        true
    );
    merge_bool!(
        base.task.single_active_task_per_agent,
        overlay.task.single_active_task_per_agent,
        true
    );
    merge_bool!(
        base.task.strict_completion,
        overlay.task.strict_completion,
        false
    );
    merge_str!(
        base.context.default_mode,
        overlay.context.default_mode,
        "compact"
    );
    if overlay.context.max_events != 10 {
        base.context.max_events = overlay.context.max_events;
    }
    merge_str!(base.context.lookback, overlay.context.lookback, "7d");
    merge_bool!(
        base.context.include_git_status,
        overlay.context.include_git_status,
        true
    );
    merge_bool!(
        base.checkpoint.require_before_session_end,
        overlay.checkpoint.require_before_session_end,
        true
    );
    merge_bool!(
        base.checkpoint.capture_diff_stats,
        overlay.checkpoint.capture_diff_stats,
        true
    );
    merge_bool!(
        base.checkpoint.capture_untracked_files,
        overlay.checkpoint.capture_untracked_files,
        true
    );
    merge_str!(base.output.color, overlay.output.color, "auto");
    merge_bool!(base.output.unicode, overlay.output.unicode, true);
    if let Some(v) = overlay.agent.default_name {
        base.agent.default_name = Some(v);
    }
    if let Some(v) = overlay.agent.default_provider {
        base.agent.default_provider = Some(v);
    }
}

fn apply_env_overrides(config: &mut CarryCtxConfig, env: &HashMap<String, String>) {
    for (key, value) in env {
        match key.as_str() {
            "CARRYCTX_PROJECT_ID" => config.project.id = value.clone(),
            "CARRYCTX_PROJECT_NAME" => config.project.name = value.clone(),
            "CARRYCTX_TASK_PREFIX" => config.project.task_prefix = value.clone(),
            "CARRYCTX_MAIN_BRANCH" => config.git.main_branch = value.clone(),
            "CARRYCTX_STALE_AFTER" => config.session.stale_after = value.clone(),
            "CARRYCTX_DEFAULT_MODE" => config.context.default_mode = value.clone(),
            "CARRYCTX_STRICT_COMPLETION" => {
                config.task.strict_completion = value == "true";
            }
            "CARRYCTX_CAPTURE_DIFF_STATS" => {
                config.checkpoint.capture_diff_stats = value == "true";
            }
            _ => {
                if let Some(nested) = key.strip_prefix("CARRYCTX_AGENT__") {
                    match nested {
                        "DEFAULT_NAME" => config.agent.default_name = Some(value.clone()),
                        "DEFAULT_PROVIDER" => config.agent.default_provider = Some(value.clone()),
                        _ => {}
                    }
                }
            }
        }
    }
}

pub fn find_project_config_dir(start_path: &Path) -> Option<std::path::PathBuf> {
    let mut current = Some(start_path.to_path_buf());
    while let Some(dir) = current {
        let config_path = dir.join(".carryctx");
        if config_path.is_dir() {
            return Some(dir);
        }
        current = dir.parent().map(|p| p.to_path_buf());
    }
    None
}
