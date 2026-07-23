use carryctx::application::runtime::InvocationContext;
use carryctx::error::ExitCode;
use clap::Parser;

#[derive(Parser, Debug)]
pub struct McpArgs {
    #[arg(long, help = "Run MCP server using stdio transport")]
    pub stdio: bool,
}

pub fn handle_mcp(args: &McpArgs, ctx: &InvocationContext) -> Result<ExitCode, ExitCode> {
    if !args.stdio {
        eprintln!("Currently only --stdio transport is supported");
        return Err(ExitCode::InvalidArguments);
    }

    use carryctx::application::mcp::run_stdio_server;
    run_stdio_server(ctx).map_err(|e| {
        eprintln!("MCP Server Error: {}", e);
        ExitCode::General
    })?;

    Ok(ExitCode::Success)
}
