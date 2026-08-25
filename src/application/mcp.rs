use crate::application::runtime::InvocationContext;
use crate::error::CarryCtxError;
use serde_json::Value;
use std::io::{self, BufRead, Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Maximum size of a single JSON-RPC request frame (in bytes).
///
/// A buggy or hostile client can stream arbitrarily long input without a
/// newline; without a cap the long-lived MCP server would buffer it all and
/// eventually OOM. Frames larger than this are rejected with a parse error
/// (-32700) and the remainder of the frame is discarded.
const MAX_FRAME_SIZE: usize = 1024 * 1024;

/// Maximum wall-clock time a spawned `carryctx` child may run before it is
/// killed and the caller receives a JSON-RPC -32000 error.
///
/// The server loop is single-threaded; without a timeout one hung child
/// (git prompt, filesystem stall, ...) would freeze every client request.
const CHILD_TIMEOUT: Duration = Duration::from_secs(60);

/// Grace period for draining child output pipes after the child exits.
const PIPE_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Interval between `try_wait` polls while waiting for a child to finish.
const CHILD_POLL_INTERVAL: Duration = Duration::from_millis(10);

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
    serve(stdin.lock(), &mut stdout)
}

/// Outcome of reading one framed line from the request stream.
enum Frame {
    /// A complete line within the size cap.
    Line(String),
    /// The line exceeded [`MAX_FRAME_SIZE`]; everything up to (and including)
    /// its terminating newline has been discarded.
    Oversize,
    /// End of stream reached with no pending bytes.
    Eof,
}

/// Read one newline-terminated frame from `reader` into a bounded buffer.
///
/// Unlike `BufRead::lines()` this never accumulates more than
/// [`MAX_FRAME_SIZE`] bytes: once the cap is hit the frame enters drain
/// mode, discarding subsequent bytes until the next newline so the server
/// stays responsive to well-formed requests afterwards.
fn read_frame<R: BufRead>(reader: &mut R, buf: &mut Vec<u8>) -> io::Result<Frame> {
    buf.clear();
    let mut oversized = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            // EOF: leftover bytes without a trailing newline still count as
            // a final frame; nothing pending after draining means EOF.
            return Ok(if oversized {
                Frame::Oversize
            } else if buf.is_empty() {
                Frame::Eof
            } else {
                Frame::Line(String::from_utf8_lossy(buf).into_owned())
            });
        }
        match available.iter().position(|&b| b == b'\n') {
            Some(idx) => {
                if !oversized {
                    append_bounded(buf, &available[..idx], &mut oversized);
                }
                reader.consume(idx + 1);
                return Ok(if oversized {
                    Frame::Oversize
                } else {
                    Frame::Line(String::from_utf8_lossy(buf).into_owned())
                });
            }
            None => {
                if !oversized {
                    append_bounded(buf, available, &mut oversized);
                }
                let len = available.len();
                reader.consume(len);
            }
        }
    }
}

/// Append `chunk` unless doing so would exceed the frame cap, flipping
/// `oversized` instead of growing the buffer past [`MAX_FRAME_SIZE`].
fn append_bounded(buf: &mut Vec<u8>, chunk: &[u8], oversized: &mut bool) {
    if *oversized || buf.len() + chunk.len() > MAX_FRAME_SIZE {
        *oversized = true;
        return;
    }
    buf.extend_from_slice(chunk);
}

/// Serialize and write one JSON-RPC response followed by a newline.
///
/// Any write/flush error (most commonly EPIPE when the client closed the
/// pipe) is returned so the caller can shut the server down cleanly
/// instead of panicking.
fn send_response<W: Write>(writer: &mut W, response: &Value) -> io::Result<()> {
    let mut line = serde_json::to_string(response)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    line.push('\n');
    writer.write_all(line.as_bytes())?;
    writer.flush()
}

/// True when the request carries a real (non-null) `id`, i.e. it expects a
/// response. JSON-RPC 2.0 notifications carry no `id` member and MUST NOT
/// receive one; replying to them corrupts strict client state machines.
fn expects_response(req: &Value) -> bool {
    req.get("id").is_some_and(|id| !id.is_null())
}

fn serve<R: BufRead, W: Write>(mut reader: R, writer: &mut W) -> Result<(), CarryCtxError> {
    let mut raw = Vec::with_capacity(4096);
    loop {
        let frame = read_frame(&mut reader, &mut raw)
            .map_err(|e| CarryCtxError::database_error(format!("Failed to read stdin: {e}")))?;
        let line = match frame {
            Frame::Eof => break,
            Frame::Oversize => {
                // Per JSON-RPC 2.0, undetectable ids are reported as null.
                let res = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": {
                        "code": -32700,
                        "message": format!(
                            "Parse error: request frame exceeds maximum size of {} bytes",
                            MAX_FRAME_SIZE
                        )
                    }
                });
                if send_response(writer, &res).is_err() {
                    break;
                }
                continue;
            }
            Frame::Line(line) => line,
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Parse JSON-RPC request
        let req: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                let res = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": { "code": -32700, "message": format!("Parse error: {e}") }
                });
                if send_response(writer, &res).is_err() {
                    break;
                }
                continue;
            }
        };

        // Notifications (no id member, e.g. `initialized`,
        // `notifications/cancelled`) must never be answered.
        if !expects_response(&req) {
            continue;
        }

        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");

        let response = dispatch_request(&req, id, method);
        if send_response(writer, &response).is_err() {
            // Client closed the pipe or stdout failed; exit cleanly.
            eprintln!("carryctx mcp: client stream closed, shutting down");
            break;
        }
    }

    Ok(())
}

/// Build the JSON-RPC response `Value` for one id-bearing request.
fn dispatch_request(req: &Value, id: Value, method: &str) -> Value {
    if method == "initialize" {
        return serde_json::json!({
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
    }

    if method == "tools/list" {
        return serde_json::json!({
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
    }

    if method == "tools/call" {
        return handle_tools_call(req, id);
    }

    // Liveness probe (CTX-0083): strict clients expect an empty-object
    // result rather than -32601. Id-less pings never reach this branch —
    // `serve` suppresses notifications before dispatch.
    if method == "ping" {
        return serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": {} });
    }

    // Catch-all MethodNotFound
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32601,
            "message": format!("Method not found: {}", method)
        }
    })
}

/// Handle one `tools/call` request by spawning the matching `carryctx`
/// subcommand and capturing its output.
fn handle_tools_call(req: &Value, id: Value) -> Value {
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
    let mut cmd = Command::new(&resolved_exe);
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
        return serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": format!("Tool not found: {}", name) }
        });
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

    match execute_tool_call(&mut cmd, CHILD_TIMEOUT) {
        Ok(ToolOutcome::Completed {
            stdout,
            stderr,
            success,
        }) => {
            let mut text = stdout;
            if !stderr.is_empty() {
                if !text.is_empty() {
                    text.push_str("\n--- STDERR ---\n");
                }
                text.push_str(&stderr);
            }
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [
                        { "type": "text", "text": text }
                    ],
                    "isError": !success
                }
            })
        }
        Ok(ToolOutcome::TimedOut) => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32000,
                "message": format!(
                    "carryctx subprocess timed out after {}s and was killed",
                    CHILD_TIMEOUT.as_secs()
                )
            }
        }),
        Err(e) => {
            let hint = if e.kind() == io::ErrorKind::NotFound {
                format!(
                    " The resolved binary at '{}' no longer exists on disk (it was likely replaced by an upgrade while this MCP server was running). Restart the MCP client/server to pick up the new binary.",
                    resolved_exe.display()
                )
            } else {
                String::new()
            };
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32000, "message": format!("Failed to execute carryctx subprocess: {}.{}", e, hint) }
            })
        }
    }
}

/// Result of a completed (or abandoned) child-tool invocation.
enum ToolOutcome {
    Completed {
        stdout: String,
        stderr: String,
        success: bool,
    },
    /// The child did not exit within the allotted timeout and was killed.
    TimedOut,
}

/// Run `cmd` to completion, enforcing `timeout`.
///
/// Output pipes are drained on dedicated threads so a chatty child can
/// never deadlock on a full pipe buffer while we wait. If the child does
/// not exit within `timeout` it is killed and [`ToolOutcome::TimedOut`] is
/// returned, keeping the single-threaded server loop responsive.
fn execute_tool_call(cmd: &mut Command, timeout: Duration) -> io::Result<ToolOutcome> {
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = cmd.spawn()?;

    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();
    let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>();
    let (err_tx, err_rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(pipe) = out_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut buf);
        }
        let _ = out_tx.send(buf);
    });
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(pipe) = err_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut buf);
        }
        let _ = err_tx.send(buf);
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait()? {
            Some(status) => break Some(status),
            None => {
                if Instant::now() >= deadline {
                    break None;
                }
                std::thread::sleep(CHILD_POLL_INTERVAL);
            }
        }
    };

    match status {
        Some(status) => {
            let stdout = recv_bytes(&out_rx);
            let stderr = recv_bytes(&err_rx);
            Ok(ToolOutcome::Completed {
                stdout,
                stderr,
                success: status.success(),
            })
        }
        None => {
            let _ = child.kill();
            let _ = child.wait();
            Ok(ToolOutcome::TimedOut)
        }
    }
}

/// Collect a drained output buffer, giving up after [`PIPE_DRAIN_TIMEOUT`]
/// should a grandchild process still hold the pipe open.
fn recv_bytes(rx: &mpsc::Receiver<Vec<u8>>) -> String {
    let bytes = rx.recv_timeout(PIPE_DRAIN_TIMEOUT).unwrap_or_default();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Drive `serve` with the given stdin lines and collect every response
    /// line emitted on stdout.
    fn run_serve(input: &str) -> Vec<Value> {
        let mut output: Vec<u8> = Vec::new();
        serve(Cursor::new(input.to_owned().into_bytes()), &mut output).unwrap();
        String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

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

    #[test]
    fn serve_answers_initialize_and_tools_list() {
        let responses = run_serve(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}
{"jsonrpc":"2.0","id":2,"method":"tools/list"}
"#,
        );
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0]["id"], 1);
        assert_eq!(responses[0]["result"]["serverInfo"]["name"], "carryctx");
        assert_eq!(responses[1]["id"], 2);
        assert_eq!(responses[1]["result"]["tools"].as_array().unwrap().len(), 6);
    }

    #[test]
    fn serve_suppresses_responses_for_id_less_notifications() {
        // A notification MUST NOT produce any response, including the
        // catch-all Method-not-found error that used to leak `id:null`.
        let responses = run_serve(
            r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}
{"jsonrpc":"2.0","method":"initialized"}
{"jsonrpc":"2.0","id":"after","method":"tools/list"}
"#,
        );
        // Only the final id-bearing request is answered.
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0]["id"], "after");
        assert_eq!(responses[0]["result"]["tools"].as_array().unwrap().len(), 6);
    }

    #[test]
    fn serve_treats_null_id_as_notification() {
        let responses = run_serve(r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#);
        assert!(
            responses.is_empty(),
            "null-id requests must not be answered"
        );
    }

    /// CTX-0083: strict MCP clients send `ping` as a liveness probe and
    /// expect an empty-object result with the matching id instead of
    /// -32601 Method not found.
    #[test]
    fn serve_answers_ping_with_empty_result() {
        let responses = run_serve(
            r#"{"jsonrpc":"2.0","id":7,"method":"ping"}
{"jsonrpc":"2.0","id":"str-id","method":"ping"}
"#,
        );
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0]["id"], 7);
        assert_eq!(responses[0]["result"], serde_json::json!({}));
        assert!(responses[0]["error"].is_null());
        assert_eq!(responses[1]["id"], "str-id");
        assert_eq!(responses[1]["result"], serde_json::json!({}));
    }

    #[test]
    fn serve_stays_silent_for_id_less_ping() {
        // A notification-shaped ping (no id member) gets no response.
        let responses = run_serve(
            r#"{"jsonrpc":"2.0","method":"ping"}
{"jsonrpc":"2.0","id":1,"method":"ping"}
"#,
        );
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0]["id"], 1);
    }

    #[test]
    fn serve_replies_parse_error_with_null_id_for_garbage_input() {
        let responses = run_serve("this is not json\n");
        assert_eq!(responses.len(), 1);
        assert!(responses[0]["id"].is_null());
        assert_eq!(responses[0]["error"]["code"], -32700);
    }

    #[test]
    fn serve_rejects_oversized_frames_and_keeps_serving() {
        let huge = "x".repeat(MAX_FRAME_SIZE + 1);
        let mut input = format!("{huge}\n");
        input.push_str(r#"{"jsonrpc":"2.0","id":7,"method":"tools/list"}"#);
        input.push('\n');

        let responses = run_serve(&input);
        // One parse error for the oversized frame, then the valid request.
        assert_eq!(responses.len(), 2);
        assert!(responses[0]["id"].is_null());
        assert_eq!(responses[0]["error"]["code"], -32700);
        assert!(
            responses[0]["error"]["message"]
                .as_str()
                .unwrap()
                .contains("exceeds maximum size")
        );
        // The following valid request must still be served.
        assert_eq!(responses[1]["id"], 7);
        assert_eq!(responses[1]["result"]["tools"].as_array().unwrap().len(), 6);
    }

    #[test]
    fn serve_returns_ok_when_client_closes_stdout() {
        struct BrokenPipe;
        impl Write for BrokenPipe {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::from(io::ErrorKind::BrokenPipe))
            }
            fn flush(&mut self) -> io::Result<()> {
                Err(io::Error::from(io::ErrorKind::BrokenPipe))
            }
        }

        let input = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}
{"jsonrpc":"2.0","id":2,"method":"tools/list"}
"#;
        let result = serve(Cursor::new(input.as_bytes()), &mut BrokenPipe);
        assert!(
            result.is_ok(),
            "write failures must stop serving, not panic"
        );
    }

    #[cfg(unix)]
    #[test]
    fn execute_tool_call_completes_fast_children() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("echo hi");
        match execute_tool_call(&mut cmd, Duration::from_secs(5)).unwrap() {
            ToolOutcome::Completed {
                stdout,
                stderr,
                success,
            } => {
                assert_eq!(stdout.trim(), "hi");
                assert!(stderr.is_empty());
                assert!(success);
            }
            ToolOutcome::TimedOut => panic!("fast child must not time out"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn execute_tool_call_kills_hung_children() {
        let started = Instant::now();
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("sleep 30");
        let outcome = execute_tool_call(&mut cmd, Duration::from_millis(150)).unwrap();
        assert!(matches!(outcome, ToolOutcome::TimedOut));
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "timeout must not block for the child's full runtime"
        );
    }
}
