use crate::application::runtime::InvocationContext;
use crate::error::CarryCtxError;
use serde_json::Value;
use std::io::{self, BufRead, Write};

pub fn run_stdio_server(_ctx: &InvocationContext) -> Result<(), CarryCtxError> {
    // Currently simply loops until stdin is closed.
    // Future work: Implement full MCP JSON-RPC protocol
    
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line_res in stdin.lock().lines() {
        let line = line_res.map_err(|e| {
            CarryCtxError::database_error(format!("Failed to read stdin: {e}"))
        })?;

        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Parse JSON-RPC request
        let req: Value = serde_json::from_str(line).map_err(|e| {
            CarryCtxError::invalid_arguments(format!("Failed to parse MCP JSON-RPC request: {e}"))
        })?;

        // Extract basic RPC fields
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");

        // Handle initialization
        if method == "initialize" {
            let res = serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05", // Model Context Protocol version
                    "capabilities": {
                        "tools": { "listChanged": true }
                    },
                    "serverInfo": {
                        "name": "carryctx",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }
            });
            writeln!(stdout, "{}", serde_json::to_string(&res).unwrap()).unwrap();
            stdout.flush().unwrap();
            continue;
        }

        // Handle tools/list
        if method == "tools/list" {
            let res = serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "tools": [
                        {
                            "name": "carryctx_task_manager",
                            "description": "Manage CarryCtx project tasks. Actions: list, create, update, claim, block.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "action": { "type": "string", "description": "The task subcommand (list, create, update, etc.)" },
                                    "args": { "type": "array", "items": { "type": "string" }, "description": "Additional flags/arguments" }
                                },
                                "required": ["action"]
                            }
                        },
                        {
                            "name": "carryctx_progress_tracker",
                            "description": "Manage task progress, notes, and blockers.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "action": { "type": "string", "description": "The progress subcommand (create, list, resolve, etc.)" },
                                    "args": { "type": "array", "items": { "type": "string" }, "description": "Additional flags/arguments" }
                                },
                                "required": ["action"]
                            }
                        },
                        {
                            "name": "carryctx_session_controller",
                            "description": "Manage context, sessions, and state snapshots. Actions: status, resume, context, checkpoint.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "action": { "type": "string", "description": "The command (status, resume, context, checkpoint)" },
                                    "args": { "type": "array", "items": { "type": "string" }, "description": "Additional flags/arguments" }
                                },
                                "required": ["action"]
                            }
                        }
                    ]
                }
            });
            writeln!(stdout, "{}", serde_json::to_string(&res).unwrap()).unwrap();
            stdout.flush().unwrap();
            continue;
        }

        // Handle tools/call
        if method == "tools/call" {
            let params = req.get("params").and_then(|p| p.as_object());
            let name = params.and_then(|p| p.get("name")).and_then(|n| n.as_str()).unwrap_or("");
            let args_obj = params.and_then(|p| p.get("arguments")).and_then(|a| a.as_object());
            
            let action = args_obj.and_then(|a| a.get("action")).and_then(|a| a.as_str()).unwrap_or("");
            let cli_args = args_obj.and_then(|a| a.get("args")).and_then(|a| a.as_array());

            let mut cmd = std::process::Command::new(std::env::current_exe().unwrap_or_else(|_| "carryctx".into()));
            cmd.arg("--json");

            let valid = match name {
                "carryctx_task_manager" => { cmd.arg("task"); true }
                "carryctx_progress_tracker" => { cmd.arg("progress"); true }
                "carryctx_session_controller" => { true } // action is the direct command
                _ => false
            };

            if !valid {
                let err_res = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32601, "message": format!("Tool not found: {}", name) }
                });
                writeln!(stdout, "{}", serde_json::to_string(&err_res).unwrap()).unwrap();
                stdout.flush().unwrap();
                continue;
            }

            if !action.is_empty() {
                cmd.arg(action);
            }

            if let Some(arr) = cli_args {
                for a in arr {
                    if let Some(s) = a.as_str() {
                        cmd.arg(s);
                    }
                }
            }

            match cmd.output() {
                Ok(output) => {
                    let stdout_str = String::from_utf8_lossy(&output.stdout).to_string();
                    let stderr_str = String::from_utf8_lossy(&output.stderr).to_string();
                    
                    let mut text = stdout_str.clone();
                    if !stderr_str.is_empty() {
                        text.push_str("\n--- STDERR ---\n");
                        text.push_str(&stderr_str);
                    }

                    let res = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [
                                { "type": "text", "text": text }
                            ],
                            "isError": !output.status.success()
                        }
                    });
                    writeln!(stdout, "{}", serde_json::to_string(&res).unwrap()).unwrap();
                    stdout.flush().unwrap();
                }
                Err(e) => {
                    let err_res = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32000, "message": format!("Failed to execute carryctx subprocess: {}", e) }
                    });
                    writeln!(stdout, "{}", serde_json::to_string(&err_res).unwrap()).unwrap();
                    stdout.flush().unwrap();
                }
            }
            continue;
        }

        // Catch-all MethodNotFound
        let err_res = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32601,
                "message": format!("Method not found: {}", method)
            }
        });
        writeln!(stdout, "{}", serde_json::to_string(&err_res).unwrap()).unwrap();
        stdout.flush().unwrap();
    }

    Ok(())
}
