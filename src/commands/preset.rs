
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
    /// List installed and active presets
    List,
}

pub fn handle_preset(args: &PresetArgs, _ctx: &InvocationContext, is_json: bool) -> Result<ExitCode, ExitCode> {
    match &args.command {
        PresetCommand::Install { source } => {
            if !is_json {
                println!("Installing preset from '{}'...", source);
                eprintln!("warning: Preset installation is not yet fully implemented.");
            } else {
                println!(r#"{{"schema_version":1,"command":"preset.install","success":true,"data":{{"status":"not_implemented","source":"{}"}}}}"#, source);
            }
            Ok(ExitCode::Success)
        }
        PresetCommand::Activate { name } => {
            if !is_json {
                println!("Activating preset '{}'...", name);
                eprintln!("warning: Preset activation is not yet fully implemented.");
            } else {
                println!(r#"{{"schema_version":1,"command":"preset.activate","success":true,"data":{{"status":"not_implemented","name":"{}"}}}}"#, name);
            }
            Ok(ExitCode::Success)
        }
        PresetCommand::List => {
            if !is_json {
                println!("Installed Presets:");
                eprintln!("warning: Preset listing is not yet fully implemented.");
            } else {
                println!(r#"{{"schema_version":1,"command":"preset.list","success":true,"data":[]}}"#);
            }
            Ok(ExitCode::Success)
        }
    }
}
