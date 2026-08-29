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
        let mut config =
            toml::Value::try_from(CarryCtxConfig::default()).expect("default config serializes");

        let global_path = self.xdg_paths.global_config();
        if global_path.exists() {
            let global_toml = std::fs::read_to_string(&global_path).map_err(|e| {
                CarryCtxError::configuration_error(format!("Failed to read global config: {}", e))
            })?;
            let global: toml::Value = toml::from_str(&global_toml).map_err(|e| {
                CarryCtxError::configuration_error(format!("Invalid global config: {}", e))
            })?;
            merge_config_value(&mut config, global);
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
                let project: toml::Value = toml::from_str(&project_toml).map_err(|e| {
                    CarryCtxError::configuration_error(format!("Invalid project config: {}", e))
                })?;
                merge_config_value(&mut config, project);
            }
        }

        let mut config: CarryCtxConfig = config.try_into().map_err(|e| {
            CarryCtxError::configuration_error(format!("Invalid merged config: {e}"))
        })?;
        apply_env_overrides(&mut config, &self.env_overrides);

        Ok(config)
    }
}

fn merge_config_value(base: &mut toml::Value, overlay: toml::Value) {
    fn merge(base: &mut toml::Value, overlay: toml::Value) {
        match (base, overlay) {
            (toml::Value::Table(base), toml::Value::Table(overlay)) => {
                for (key, value) in overlay {
                    if let Some(existing) = base.get_mut(&key) {
                        merge(existing, value);
                    } else {
                        base.insert(key, value);
                    }
                }
            }
            (base, overlay) => *base = overlay,
        }
    }
    merge(base, overlay);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omitted_overlay_fields_preserve_lower_precedence_values() {
        let mut config = toml::Value::try_from(CarryCtxConfig::default()).unwrap();
        config
            .get_mut("context")
            .and_then(toml::Value::as_table_mut)
            .unwrap()
            .insert("max_events".into(), toml::Value::Integer(25));
        merge_config_value(
            &mut config,
            toml::from_str("[context]\nlookback = \"14d\"\n").unwrap(),
        );
        let config: CarryCtxConfig = config.try_into().unwrap();
        assert_eq!(config.context.max_events, 25);
        assert_eq!(config.context.lookback, "14d");
    }
}
