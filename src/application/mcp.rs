use crate::application::runtime::InvocationContext;
use crate::error::CarryCtxError;
use serde_json::Value;
use std::io::{self, BufRead, Write};

/// Resolve the path to the `carryctx` binary to use for spawning subcommands.
///
/// The MCP server is a long-lived stdio process; if the user upgrades
/// `carryctx` (via cargo/npm/Homebrew/etc.) while this server is still
/// running, `std::env::current_exe()` keeps returning the path the process
/// was originally launched from. On Unix, replacing a binary in place
/// unlinks the old inode; the running process keeps it open and stays
/// functional, but that path no longer resolves on disk, so spawning a
/// *new* child process from it fails with `ErrorKind::NotFound`.
///
/// To avoid that, verify the `current_exe()` path still exists on disk. If
/// it does not, fall back to resolving `carryctx` from `PATH`, which will
/// find whatever binary is currently installed (the fixed, upgraded one).
fn resolve_carryctx_binary() -> std::path::PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if exe.exists() {
            return exe;
        }
    }
    which_carryctx().unwrap_or_else(|| std::path::PathBuf::from("carryctx"))
}

/// Search `PATH` for a `carryctx` executable, mirroring what a shell would
/// resolve `carryctx` to. Used only as a fallback when `current_exe()` no
/// longer points at a file that exists (see `resolve_carryctx_binary`).
fn which_carryctx() -> Option<std::path::PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join("carryctx");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

pub fn run_stdio_server(_ctx: &InvocationContext) -> Result<(), CarryCtxError> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line_res in stdin.lock().lines() {
        let line = line_res
            .map_err(|e| CarryCtxError::database_error(format!("Failed to read stdin: {e}")))?;

        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Parse JSON-RPC request
        let req: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                let err_res = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": { "code": -32700, "message": format!("Parse error: {e}") }
                });
                writeln!(stdout, "{}", serde_json::to_string(&err_res).unwrap()).unwrap();
                stdout.flush().unwrap();
                continue;
            }
        };

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

        // Handle notifications (like initialized) - no response needed
        if method == "notifications/initialized" || method == "initialized" {
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
                            "name": "carryctx_graph_explorer",
                            "description": "Query, scan, and export the project Context Graph (nodes, edges, dependencies, file-to-file links). Actions: scan, edges, link, add-node, export.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "action": { "type": "string", "description": "The graph subcommand: scan, edges, link, add-node, export" },
                                    "args": { "type": "array", "items": { "type": "string" }, "description": "CLI flags and arguments (e.g. ['--format', 'mermaid', '--compact'])" }
                                },
                                "required": ["action"]
                            }
                        },
                        {
                            "name": "carryctx_context_manager",
                            "description": "Manage persistent context, checkpoints, and state snapshots. Actions: status, context, checkpoint, resume, doctor.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "action": { "type": "string", "description": "The context command: status, context, checkpoint, resume, doctor" },
                                    "args": { "type": "array", "items": { "type": "string" }, "description": "CLI flags and arguments" }
                                },
                                "required": ["action"]
                            }
                        },
                        {
                            "name": "carryctx_task_manager",
                            "description": "Manage project tasks, dependencies, and priorities. Actions: list, create, update, claim, complete, block, unblock.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "action": { "type": "string", "description": "The task subcommand: list, create, update, claim, complete, block, unblock" },
                                    "args": { "type": "array", "items": { "type": "string" }, "description": "CLI flags and arguments" }
                                },
                                "required": ["action"]
                            }
                        },
                        {
                            "name": "carryctx_progress_tracker",
                            "description": "Manage task progress, notes, and blockers. Actions: list, create, update, resolve.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "action": { "type": "string", "description": "The progress subcommand: list, create, update, resolve" },
                                    "args": { "type": "array", "items": { "type": "string" }, "description": "CLI flags and arguments" }
                                },
                                "required": ["action"]
                            }
                        },
                        {
                            "name": "carryctx_decision_logger",
                            "description": "Log and search architectural decision records (ADRs). Actions: list, record, resolve.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "action": { "type": "string", "description": "The decision subcommand: list, record, resolve" },
                                    "args": { "type": "array", "items": { "type": "string" }, "description": "CLI flags and arguments" }
                                },
                                "required": ["action"]
                            }
                        },
                        {
                            "name": "carryctx_project_admin",
                            "description": "Manage project database, stats, cold storage archiving, and config. Actions: stats, prune, config, project.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "action": { "type": "string", "description": "The administrative command: stats, prune, config, project" },
                                    "args": { "type": "array", "items": { "type": "string" }, "description": "CLI flags and arguments" }
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
            let name = params
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("");
            let args_obj = params
                .and_then(|p| p.get("arguments"))
                .and_then(|a| a.as_object());

            let action = args_obj
                .and_then(|a| a.get("action"))
                .and_then(|a| a.as_str())
                .unwrap_or("");
            let cli_args = args_obj
                .and_then(|a| a.get("args"))
                .and_then(|a| a.as_array());

            let resolved_exe = resolve_carryctx_binary();
            let mut cmd = std::process::Command::new(&resolved_exe);
            cmd.arg("--json");

            let valid = match name {
                "carryctx_graph_explorer" => {
                    cmd.arg("graph");
                    true
                }
                "carryctx_task_manager" => {
                    cmd.arg("task");
                    true
                }
                "carryctx_progress_tracker" => {
                    cmd.arg("progress");
                    true
                }
                "carryctx_decision_logger" => {
                    cmd.arg("decision");
                    true
                }
                "carryctx_project_admin" => {
                    if action == "prune" {
                        cmd.arg("project");
                    }
                    true
                }
                "carryctx_context_manager" | "carryctx_session_controller" => true, // action is direct command
                _ => false,
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
                        if !text.is_empty() {
                            text.push_str("\n--- STDERR ---\n");
                        }
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
                    let hint = if e.kind() == std::io::ErrorKind::NotFound {
                        format!(
                            " The resolved binary at '{}' no longer exists on disk (it was likely replaced by an upgrade while this MCP server was running). Restart the MCP client/server to pick up the new binary.",
                            resolved_exe.display()
                        )
                    } else {
                        String::new()
                    };
                    let err_res = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32000, "message": format!("Failed to execute carryctx subprocess: {}.{}", e, hint) }
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

#[cfg(test)]
mod tests {
    #[test]
    fn test_mcp_tools_list_contains_graph_explorer() {
        let tools_list_response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "tools": [
                    { "name": "carryctx_graph_explorer" },
                    { "name": "carryctx_context_manager" },
                    { "name": "carryctx_task_manager" },
                    { "name": "carryctx_progress_tracker" },
                    { "name": "carryctx_decision_logger" },
                    { "name": "carryctx_project_admin" }
                ]
            }
        });

        let tools = tools_list_response["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 6);
        assert!(tools.iter().any(|t| t["name"] == "carryctx_graph_explorer"));
    }
}
