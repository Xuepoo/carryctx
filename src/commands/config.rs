use crate::*;
use carryctx::adapter::config::ConfigLoader;
use carryctx::adapter::xdg::XdgPaths;
use carryctx::application::runtime::InvocationContext;
use carryctx::error::ExitCode;
use clap::Parser;

// ── Config ───────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub enum ConfigCommand {
    /// List all effective configuration values after merging global and project configs
    List {
        /// Only show global configuration values (ignore project-local config)
        #[arg(long)]
        global: bool,
    },
    /// Get the value of a specific configuration key
    Get { key: String },
    /// Set a configuration key to a specific value
    Set {
        key: String,
        value: String,
        /// Set the value in the global configuration file (~/.config/carryctx)
        #[arg(long)]
        global: bool,
        /// Set the value in the project-shared configuration (.carryctx/config.toml)
        #[arg(long)]
        project: bool,
        /// Set the value in the user-local project configuration (.carryctx/local.toml)
        #[arg(long)]
        local: bool,
    },
    /// Remove a configuration key
    Unset {
        key: String,
        /// Remove from the global configuration
        #[arg(long)]
        global: bool,
        /// Remove from the project-shared configuration
        #[arg(long)]
        project: bool,
        /// Remove from the user-local project configuration
        #[arg(long)]
        local: bool,
    },
    /// Validate the current configuration for syntax errors and schema compliance
    Validate,
    /// List the paths of all configuration files currently being merged
    Sources,
    /// Print the absolute path to a specific configuration file
    Path {
        /// Print the path to the global configuration file
        #[arg(long)]
        global: bool,
        /// Print the path to the project-local configuration file
        #[arg(long)]
        project: bool,
    },
}

#[derive(Parser, Debug)]
pub struct ConfigArgs {
    /// Config subcommand to execute
    #[command(subcommand)]
    pub command: ConfigCommand,
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: config
// ═══════════════════════════════════════════════════════════════════════════

pub fn handle_config(
    args: &ConfigArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    let xdg = XdgPaths::new();
    let work_dir = resolve_work_dir(ctx);

    match &args.command {
        ConfigCommand::List { global } => {
            let cfg_path = if *global {
                xdg.global_config()
            } else {
                work_dir.join(".carryctx").join("config.toml")
            };
            let config = if cfg_path.exists() {
                std::fs::read_to_string(&cfg_path).unwrap_or_default()
            } else {
                String::new()
            };
            let data = serde_json::json!({
                "path": cfg_path.to_string_lossy(),
                "content": config,
            });
            render_and_print("config.list", Ok(data), is_json, ctx.quiet)
        }
        ConfigCommand::Get { key } => {
            let cfg_loader = ConfigLoader::new(xdg);
            let config = cfg_loader.load(Some(work_dir)).map_err(|e| e.exit_code)?;
            let value = {
                let config_str = toml::to_string(&config).unwrap_or_default();
                let mut val = "".to_string();
                for line in config_str.lines() {
                    if line.starts_with(key) || line.starts_with(&format!("{key} =")) {
                        val = line.to_string();
                    }
                }
                val
            };
            let data = serde_json::json!({ "key": key, "value": value });
            render_and_print("config.get", Ok(data), is_json, ctx.quiet)
        }
        ConfigCommand::Set {
            key,
            value,
            global,
            project,
            local: _,
        } => {
            let cfg_path = if *global {
                xdg.global_config()
            } else if *project {
                work_dir.join(".carryctx").join("config.toml")
            } else {
                return Err(ExitCode::InvalidArguments);
            };

            if let Some(parent) = cfg_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }

            let mut content = if cfg_path.exists() {
                std::fs::read_to_string(&cfg_path).unwrap_or_default()
            } else {
                String::new()
            };

            // Simple set: replace or append
            let line = format!("{key} = \"{value}\"");
            let key_prefix = format!("{key} ");
            let found = content.lines().any(|l| l.trim().starts_with(&key_prefix));
            if found {
                let mut new_lines: Vec<String> = content
                    .lines()
                    .map(|l| {
                        if l.trim().starts_with(&key_prefix) {
                            line.clone()
                        } else {
                            l.to_string()
                        }
                    })
                    .collect();
                new_lines.push(String::new());
                content = new_lines.join("\n");
            } else {
                if !content.ends_with('\n') {
                    content.push('\n');
                }
                content.push_str(&line);
                content.push('\n');
            }

            std::fs::write(&cfg_path, &content).map_err(|e| {
                eprintln!("Failed to write config: {e}");
                ExitCode::Configuration
            })?;

            let data = serde_json::json!({
                "path": cfg_path.to_string_lossy(),
                "key": key,
                "value": value,
            });
            render_and_print("config.set", Ok(data), is_json, ctx.quiet)
        }
        ConfigCommand::Unset {
            key,
            global,
            project,
            local: _,
        } => {
            let cfg_path = if *global {
                xdg.global_config()
            } else if *project {
                work_dir.join(".carryctx").join("config.toml")
            } else {
                return Err(ExitCode::InvalidArguments);
            };

            if cfg_path.exists() {
                let content = std::fs::read_to_string(&cfg_path).unwrap_or_default();
                let key_prefix = format!("{key} ");
                let new_content: String = content
                    .lines()
                    .filter(|l| !l.trim().starts_with(&key_prefix))
                    .collect::<Vec<_>>()
                    .join("\n");
                std::fs::write(&cfg_path, &new_content).map_err(|e| {
                    eprintln!("Failed to write config: {e}");
                    ExitCode::Configuration
                })?;
            }

            render_and_print(
                "config.unset",
                Ok(serde_json::json!({ "key": key })),
                is_json,
                ctx.quiet,
            )
        }
        ConfigCommand::Validate => {
            let cfg_loader = ConfigLoader::new(xdg);
            let result = cfg_loader.load(Some(work_dir));
            match result {
                Ok(config) => {
                    let data = serde_json::json!({
                        "valid": true,
                        "project": config.project,
                        "sources": ["global", "project", "env"]
                    });
                    render_and_print("config.validate", Ok(data), is_json, ctx.quiet)
                }
                Err(e) => render_and_print::<serde_json::Value>(
                    "config.validate",
                    Err(e),
                    is_json,
                    ctx.quiet,
                ),
            }
        }
        ConfigCommand::Sources => {
            let sources = serde_json::json!([
                { "name": "global", "path": xdg.global_config().to_string_lossy() },
                { "name": "project", "path": work_dir.join(".carryctx/config.toml").to_string_lossy() },
                { "name": "env", "prefix": "CARRYCTX_" },
            ]);
            render_and_print("config.sources", Ok(sources), is_json, ctx.quiet)
        }
        ConfigCommand::Path { global, project } => {
            #[allow(clippy::if_same_then_else)]
            let path = if *global {
                xdg.global_config()
            } else if *project {
                work_dir.join(".carryctx").join("config.toml")
            } else {
                // default to project config
                work_dir.join(".carryctx").join("config.toml")
            };
            let data = serde_json::json!({ "path": path.to_string_lossy() });
            render_and_print("config.path", Ok(data), is_json, ctx.quiet)
        }
    }
}
