use crate::Cli;
use carryctx::error::ExitCode;
use clap::CommandFactory;
use clap::Parser;
use clap_complete::{Shell, generate};

// ── Completions ───────────────────────────────────────────────────────────

/// Generate shell completion scripts for the current shell.
///
/// Print the completion script to stdout. Source it from your shell's config
/// file to enable tab-completion for all carryctx commands and flags.
///
/// # Examples
///
/// ```bash
/// # Bash
/// carryctx completions bash >> ~/.bash_completion.d/carryctx
///
/// # Zsh (add to ~/.zshrc)
/// eval "$(carryctx completions zsh)"
///
/// # Fish
/// carryctx completions fish > ~/.config/fish/completions/carryctx.fish
/// ```
#[derive(Parser, Debug)]
pub struct CompletionsArgs {
    /// The shell to generate completions for.
    #[arg(value_enum)]
    pub shell: Shell,
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: completions
// ═══════════════════════════════════════════════════════════════════════════

pub fn handle_completions(args: &CompletionsArgs) -> Result<ExitCode, ExitCode> {
    let mut cmd = Cli::command();
    let name = cmd.get_name().to_string();
    generate(args.shell, &mut cmd, name, &mut std::io::stdout());
    Ok(ExitCode::Success)
}
