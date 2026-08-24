use crate::open_runtime_or_report;
use carryctx::application::runtime::{InvocationContext, ProjectRuntime};
use carryctx::error::{CarryCtxError, ExitCode};
use clap::Parser;

#[derive(Parser, Debug)]
pub struct PresetArgs {
    #[command(subcommand)]
    pub command: PresetCommand,
}

#[derive(Parser, Debug)]
pub enum PresetCommand {
    /// Install a preset capability pack
    Install {
        /// The name, URL, or local path of the preset to install
        #[arg(index = 1)]
        source: String,
    },
    /// Activate an installed preset for the current project
    Activate {
        /// The name of the installed preset
        #[arg(index = 1)]
        name: String,
    },
    /// Apply an installed preset for the current project (alias for activate)
    Apply {
        /// The name of the installed preset
        #[arg(index = 1)]
        name: String,
    },
    /// Inspect content of an installed preset or template file
    Show {
        /// The name or path of the preset to display
        #[arg(index = 1)]
        name: String,
    },
    /// List installed and active presets
    List,
}

pub fn handle_preset(
    args: &PresetArgs,
    pre_opened: Option<ProjectRuntime>,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    use carryctx::application::preset::PresetManager;
    use std::path::Path;

    // Reuse the dispatcher's pre-opened runtime when available; a second
    // open only happens (and reports) when that failed.
    let runtime = match pre_opened {
        Some(runtime) => runtime,
        None => open_runtime_or_report(ctx, "preset")?,
    };
    let repo_root = runtime.git_project.repository_root.as_path();
    let manager = PresetManager::new(repo_root);

    match &args.command {
        PresetCommand::Install { source } => {
            let source_path = Path::new(source);
            match manager.install_preset(source_path) {
                Ok(entry) => {
                    let data = serde_json::json!({
                        "status": "installed",
                        "name": entry.source,
                        "integrity": entry.integrity,
                        "permissionsGranted": {
                            "filesystem": entry.permissions_granted.requires_filesystem,
                            "network": entry.permissions_granted.requires_network,
                            "env": entry.permissions_granted.requires_env,
                        },
                        "lockfile": ".carryctx/presets.lock",
                    });
                    if is_json {
                        crate::render_and_print("preset.install", Ok(data), true, ctx.quiet)
                    } else {
                        println!("✅ Successfully installed preset '{}'", entry.source);
                        println!("   Integrity Hash: {}", entry.integrity);
                        println!(
                            "   Permissions: filesystem={}, network={}, env={}",
                            entry.permissions_granted.requires_filesystem,
                            entry.permissions_granted.requires_network,
                            entry.permissions_granted.requires_env.len()
                        );
                        println!("(Saved to .carryctx/presets.lock)");
                        Ok(ExitCode::Success)
                    }
                }
                Err(e) => {
                    // Map the underlying CarryCtxError code instead of
                    // collapsing every failure to STATE_CONFLICT.
                    crate::render_and_print::<serde_json::Value>(
                        "preset.install",
                        Err(e),
                        is_json,
                        ctx.quiet,
                    )
                }
            }
        }
        PresetCommand::Activate { name } | PresetCommand::Apply { name } => {
            match manager.activate_preset(name) {
                Ok(entry) => {
                    let data = serde_json::json!({
                        "status": "activated",
                        "name": name,
                        "integrity": entry.integrity,
                        "permissionsGranted": {
                            "filesystem": entry.permissions_granted.requires_filesystem,
                            "network": entry.permissions_granted.requires_network,
                            "env": entry.permissions_granted.requires_env,
                        },
                    });
                    if is_json {
                        crate::render_and_print("preset.activate", Ok(data), true, ctx.quiet)
                    } else {
                        println!("✅ Activated preset '{}'", name);
                        println!("   Integrity Hash: {}", entry.integrity);
                        println!("   (Permissions validated against .carryctx/presets.lock)");
                        Ok(ExitCode::Success)
                    }
                }
                Err(e) => crate::render_and_print::<serde_json::Value>(
                    "preset.activate",
                    Err(e),
                    is_json,
                    ctx.quiet,
                ),
            }
        }
        PresetCommand::Show { name } => {
            let possible_paths = [
                repo_root.join(".carryctx").join(format!("{}.md", name)),
                repo_root.join(".carryctx").join(name),
                runtime
                    .git_project
                    .git_common_dir
                    .parent()
                    .unwrap_or(repo_root)
                    .join(".carryctx")
                    .join(format!("{}.md", name)),
                runtime
                    .git_project
                    .git_common_dir
                    .parent()
                    .unwrap_or(repo_root)
                    .join(".carryctx")
                    .join(name),
                Path::new(name).to_path_buf(),
            ];

            match possible_paths.iter().find(|p| p.exists() && p.is_file()) {
                Some(path) => match std::fs::read_to_string(path) {
                    Ok(content) => {
                        let data = serde_json::json!({
                            "path": path.display().to_string(),
                            "content": content,
                        });
                        if is_json {
                            crate::render_and_print("preset.show", Ok(data), true, ctx.quiet)
                        } else {
                            println!("📄 Preset Spec: {}\n", path.display());
                            println!("{content}");
                            Ok(ExitCode::Success)
                        }
                    }
                    Err(e) => crate::render_and_print::<serde_json::Value>(
                        "preset.show",
                        Err(CarryCtxError::database_error(format!(
                            "Failed to read preset file {}: {e}",
                            path.display()
                        ))),
                        is_json,
                        ctx.quiet,
                    ),
                },
                None => crate::render_and_print::<serde_json::Value>(
                    "preset.show",
                    Err(CarryCtxError::resource_not_found(format!(
                        "Preset file '{name}' not found in .carryctx/"
                    ))),
                    is_json,
                    ctx.quiet,
                ),
            }
        }
        PresetCommand::List => match manager.read_lockfile() {
            Ok(lockfile) => {
                // Serialize the real lockfile contents: `data: []` hardcoded
                // regardless of installed presets starved machine consumers.
                let mut names: Vec<&String> = lockfile.presets.keys().collect();
                names.sort();
                let presets: Vec<serde_json::Value> = names
                    .iter()
                    .filter_map(|name| {
                        lockfile.presets.get(*name).map(|entry| {
                            serde_json::json!({
                                "name": name,
                                "version": entry.version,
                                "source": entry.source,
                                "integrity": entry.integrity,
                                "permissionsGranted": {
                                    "filesystem": entry.permissions_granted.requires_filesystem,
                                    "network": entry.permissions_granted.requires_network,
                                    "env": entry.permissions_granted.requires_env,
                                },
                            })
                        })
                    })
                    .collect();
                let data = serde_json::json!({
                    "lockfile": ".carryctx/presets.lock",
                    "count": presets.len(),
                    "presets": presets,
                });
                if is_json {
                    crate::render_and_print("preset.list", Ok(data), true, ctx.quiet)
                } else {
                    println!("📦 Installed Presets (.carryctx/presets.lock):");
                    if presets.is_empty() {
                        println!("   (No presets installed)");
                    } else {
                        for preset in &presets {
                            println!(
                                " - {} (v{})",
                                preset["name"].as_str().unwrap_or_default(),
                                preset["version"].as_str().unwrap_or_default()
                            );
                            println!(
                                "   Hash: {}",
                                preset["integrity"].as_str().unwrap_or_default()
                            );
                        }
                    }
                    Ok(ExitCode::Success)
                }
            }
            Err(e) => {
                // Preserve the underlying error code (e.g. malformed
                // lockfile vs. unreadable file) instead of hardcoding
                // DATABASE(5).
                crate::render_and_print::<serde_json::Value>(
                    "preset.list",
                    Err(e),
                    is_json,
                    ctx.quiet,
                )
            }
        },
    }
}
