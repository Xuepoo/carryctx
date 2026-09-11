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

    /// Test constructor with explicit env overrides, so loader tests never
    /// depend on the ambient process environment.
    #[cfg(test)]
    fn with_env(
        xdg_paths: crate::adapter::xdg::XdgPaths,
        env_overrides: HashMap<String, String>,
    ) -> Self {
        Self {
            env_overrides,
            xdg_paths,
        }
    }

    pub fn load(&self, project_config_dir: Option<&Path>) -> Result<CarryCtxConfig, CarryCtxError> {
        let mut config =
            toml::Value::try_from(CarryCtxConfig::default()).expect("default config serializes");

        let global_path = self.xdg_paths.global_config();
        let mut global_security: Option<toml::Value> = None;
        if global_path.exists() {
            let global_toml = std::fs::read_to_string(&global_path).map_err(|e| {
                CarryCtxError::configuration_error(format!("Failed to read global config: {}", e))
            })?;
            let global: toml::Value = toml::from_str(&global_toml).map_err(|e| {
                CarryCtxError::configuration_error(format!("Invalid global config: {}", e))
            })?;
            global_security = global.get("security").cloned();
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
                // Security is global-only (CTX-0100): a repository-provided
                // [security] table can never loosen or tighten the user's
                // posture. Ignore it with a visible warning.
                if project.get("security").is_some() {
                    tracing::warn!(
                        "Ignoring [security] in {}/.carryctx/config.toml: security settings are global-only.",
                        project_dir.display()
                    );
                }
                merge_config_value(&mut config, project);
            }
        }

        // Drop any project-provided security table, then apply the global one
        // (or the deny-by-default value when the global file omits it).
        if let Some(table) = config.as_table_mut() {
            table.remove("security");
        }
        let security: crate::domain::config::SecurityConfig = match global_security {
            Some(value) => value.try_into().map_err(|e| {
                CarryCtxError::configuration_error(format!(
                    "Invalid [security] table in global config: {e}"
                ))
            })?,
            None => crate::domain::config::SecurityConfig::default(),
        };

        let mut config: CarryCtxConfig = config.try_into().map_err(|e| {
            CarryCtxError::configuration_error(format!("Invalid merged config: {e}"))
        })?;
        config.security = security;
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
            // Security gate (CTX-0100): only explicit truthy values enable it;
            // every other value (including a typo) keeps the deny-by-default
            // posture.
            "CARRYCTX_ALLOW_PROJECT_COMMANDS" => {
                config.security.allow_project_commands = matches!(
                    value.to_ascii_lowercase().as_str(),
                    "true" | "1" | "yes" | "on"
                );
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

    #[test]
    fn global_security_gate_wins_over_project_security_table() {
        let dir = tempfile::tempdir().unwrap();
        let xdg = crate::adapter::xdg::XdgPaths {
            data_home: dir.path().join("data"),
            config_home: dir.path().join("config"),
            state_home: dir.path().join("state"),
            cache_home: dir.path().join("cache"),
        };
        std::fs::create_dir_all(xdg.config_home.join("carryctx")).unwrap();
        std::fs::write(
            xdg.global_config(),
            "[security]\nallow_project_commands = true\n",
        )
        .unwrap();

        let project = dir.path().join("proj");
        std::fs::create_dir_all(project.join(".carryctx")).unwrap();
        std::fs::write(
            project.join(".carryctx").join("config.toml"),
            "[security]\nallow_project_commands = false\n[verification]\ncommands = [\"x\"]\n",
        )
        .unwrap();

        let config = ConfigLoader::with_env(xdg, HashMap::new())
            .load(Some(&project))
            .unwrap();
        assert!(
            config.security.allow_project_commands,
            "the global security section must win over the project one"
        );
        assert_eq!(config.verification.commands, vec!["x"]);
    }

    #[test]
    fn missing_global_security_defaults_to_deny() {
        let dir = tempfile::tempdir().unwrap();
        let xdg = crate::adapter::xdg::XdgPaths {
            data_home: dir.path().join("data"),
            config_home: dir.path().join("config"),
            state_home: dir.path().join("state"),
            cache_home: dir.path().join("cache"),
        };
        let project = dir.path().join("proj");
        std::fs::create_dir_all(project.join(".carryctx")).unwrap();
        std::fs::write(
            project.join(".carryctx").join("config.toml"),
            "[security]\nallow_project_commands = true\n",
        )
        .unwrap();

        let config = ConfigLoader::with_env(xdg, HashMap::new())
            .load(Some(&project))
            .unwrap();
        assert!(
            !config.security.allow_project_commands,
            "a project cannot enable the global gate"
        );
    }

    #[test]
    fn env_gate_override_only_enables_on_truthy_values() {
        let mut config = CarryCtxConfig::default();
        let mut env = HashMap::new();
        for value in ["true", "1", "yes", "on"] {
            env.insert(
                "CARRYCTX_ALLOW_PROJECT_COMMANDS".to_string(),
                value.to_string(),
            );
            apply_env_overrides(&mut config, &env);
            assert!(config.security.allow_project_commands, "{value}");
        }
        for value in ["false", "0", "no", "banana", ""] {
            config.security.allow_project_commands = true;
            env.insert(
                "CARRYCTX_ALLOW_PROJECT_COMMANDS".to_string(),
                value.to_string(),
            );
            apply_env_overrides(&mut config, &env);
            assert!(!config.security.allow_project_commands, "{value:?}");
        }
    }
}
