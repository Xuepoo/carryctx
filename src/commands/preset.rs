use crate::try_open_runtime;
use carryctx::application::runtime::InvocationContext;
use carryctx::error::ExitCode;
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
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    use carryctx::application::preset::PresetManager;
    use std::path::Path;

    let runtime = try_open_runtime(ctx)?;
    let repo_root = runtime.git_project.repository_root.as_path();
    let manager = PresetManager::new(repo_root);

    match &args.command {
        PresetCommand::Install { source } => {
            let source_path = Path::new(source);
            match manager.install_preset(source_path) {
                Ok(entry) => {
                    if !is_json {
                        println!("✅ Successfully installed preset '{}'", entry.source);
                        println!("   Integrity Hash: {}", entry.integrity);
                        println!(
                            "   Permissions: filesystem={}, network={}, env={}",
                            entry.permissions_granted.requires_filesystem,
                            entry.permissions_granted.requires_network,
                            entry.permissions_granted.requires_env.len()
                        );
                        println!("(Saved to .carryctx/presets.lock)");
                    } else {
                        println!(
                            r#"{{"schema_version":1,"command":"preset.install","success":true,"data":{{"status":"installed","name":"{}"}}}}"#,
                            source
                        );
                    }
                    Ok(ExitCode::Success)
                }
                Err(e) => {
                    if !is_json {
                        eprintln!("❌ Failed to install preset: {}", e);
                    } else {
                        println!(
                            r#"{{"schema_version":1,"command":"preset.install","success":false,"error":{{"message":"{}"}}}}"#,
                            e
                        );
                    }
                    Err(ExitCode::StateConflict)
                }
            }
        }
        PresetCommand::Activate { name } | PresetCommand::Apply { name } => {
            match manager.activate_preset(name) {
                Ok(entry) => {
                    if !is_json {
                        println!("✅ Activated preset '{}'", name);
                        println!("   Integrity Hash: {}", entry.integrity);
                        println!("   (Permissions validated against .carryctx/presets.lock)");
                    } else {
                        println!(
                            r#"{{"schema_version":1,"command":"preset.activate","success":true,"data":{{"status":"activated","name":"{}"}}}}"#,
                            name
                        );
                    }
                    Ok(ExitCode::Success)
                }
                Err(e) => {
                    if !is_json {
                        eprintln!("❌ Failed to activate preset '{}': {}", name, e);
                    } else {
                        println!(
                            r#"{{"schema_version":1,"command":"preset.activate","success":false,"error":{{"message":"{}"}}}}"#,
                            e
                        );
                    }
                    Err(ExitCode::StateConflict)
                }
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

            let found = possible_paths.iter().find(|p| p.exists() && p.is_file());

            if let Some(path) = found {
                match std::fs::read_to_string(path) {
                    Ok(content) => {
                        if !is_json {
                            println!("📄 Preset Spec: {}\n", path.display());
                            println!("{}", content);
                        } else {
                            println!(
                                r#"{{"schema_version":1,"command":"preset.show","success":true,"data":{{"path":"{}","content":{:?}}}}}"#,
                                path.display(),
                                content
                            );
                        }
                        Ok(ExitCode::Success)
                    }
                    Err(e) => {
                        eprintln!("❌ Failed to read preset file {}: {}", path.display(), e);
                        Err(ExitCode::General)
                    }
                }
            } else {
                eprintln!("❌ Preset file '{}' not found in .carryctx/", name);
                Err(ExitCode::General)
            }
        }
        PresetCommand::List => match manager.read_lockfile() {
            Ok(lockfile) => {
                if !is_json {
                    println!("📦 Installed Presets (.carryctx/presets.lock):");
                    if lockfile.presets.is_empty() {
                        println!("   (No presets installed)");
                    } else {
                        for (name, entry) in &lockfile.presets {
                            println!(" - {} (v{})", name, entry.version);
                            println!("   Hash: {}", entry.integrity);
                        }
                    }
                } else {
                    println!(
                        r#"{{"schema_version":1,"command":"preset.list","success":true,"data":[]}}"#
                    );
                }
                Ok(ExitCode::Success)
            }
            Err(e) => {
                if !is_json {
                    eprintln!("❌ Failed to read lockfile: {}", e);
                } else {
                    println!(
                        r#"{{"schema_version":1,"command":"preset.list","success":false,"error":{{"message":"{}"}}}}"#,
                        e
                    );
                }
                Err(ExitCode::Database)
            }
        },
    }
}
