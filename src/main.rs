// Binary entry (Step3): root package is binary-only, no lib target.
// All CLI surface lives in the `carryctx-cli` crate.
use carryctx_cli::cli::{Cli, install_broken_pipe_hook, run};
use clap::Parser;

fn main() {
    install_broken_pipe_hook();
    let code = match run(Cli::parse()) {
        Ok(code) => code,
        Err(code) => code,
    };
    std::process::exit(code as i32);
}
