// Thin binary wrapper (Step2): the CLI surface (`Cli`/`Commands`, dispatcher
// `run`, shared runtime/render/resolve helpers, `commands/*`) now lives in
// the `carryctx-cli` crate. This file only wires the binary entry point.
// Step3 deletes the remaining duplication between here and `cli.rs`.
use carryctx_cli::cli::{Cli, install_broken_pipe_hook, run};
use clap::Parser;

fn main() {
    // Convert broken-pipe panics (e.g. `carryctx ... | head`) into a clean
    // SIGPIPE-style exit instead of printing a panic message on stderr.
    install_broken_pipe_hook();
    // Initialize tracing subscriber for RUST_LOG support
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(true)
        .try_init();

    let cli = Cli::parse();
    let result = run(cli);
    match result {
        Ok(exit_code) => std::process::exit(exit_code as i32),
        Err(code) => std::process::exit(code as i32),
    }
}
