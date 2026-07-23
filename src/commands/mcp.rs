use carryctx::application::runtime::InvocationContext;
use carryctx::error::ExitCode;
use clap::Parser;

#[derive(Parser, Debug)]
pub struct McpArgs {
    /// Accept stdio flag for compatibility; stdio is always the transport
    #[arg(long, hide = true)]
    pub stdio: bool,
}

pub fn handle_mcp(_args: &McpArgs, ctx: &InvocationContext) -> Result<ExitCode, ExitCode> {
    // stdio is the only supported transport; --stdio flag is accepted for compatibility
    use carryctx::application::mcp::run_stdio_server;
    run_stdio_server(ctx).map_err(|e| {
        eprintln!("MCP Server Error: {}", e);
        ExitCode::General
    })?;

    Ok(ExitCode::Success)
}
