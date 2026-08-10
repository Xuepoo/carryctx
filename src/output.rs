use crate::error::{CarryCtxError, ExitCode};
use serde::Serialize;
use serde_json::Value;

/// Schema version for all JSON output
pub const SCHEMA_VERSION: u64 = 1;

/// Success envelope
#[derive(Debug, Serialize)]
pub struct SuccessEnvelope<T: Serialize> {
    pub schema_version: u64,
    pub command: String,
    pub success: bool,
    pub data: T,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    pub meta: EnvelopeMeta,
}

/// Error envelope
#[derive(Debug, Serialize)]
pub struct ErrorEnvelope {
    pub schema_version: u64,
    pub command: String,
    pub success: bool,
    pub error: ErrorPayload,
}

#[derive(Debug, Serialize)]
pub struct ErrorPayload {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Value::is_null")]
    pub details: Value,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<String>,
}

/// Envelope metadata
#[derive(Debug, Serialize)]
pub struct EnvelopeMeta {
    pub timestamp: String,
}

/// Build a success envelope
pub fn success_envelope<T: Serialize>(
    command: &str,
    data: T,
    warnings: Vec<String>,
) -> SuccessEnvelope<T> {
    SuccessEnvelope {
        schema_version: SCHEMA_VERSION,
        command: command.to_string(),
        success: true,
        data,
        warnings,
        meta: EnvelopeMeta {
            timestamp: chrono::Utc::now().to_rfc3339(),
        },
    }
}

/// Build an error envelope from a CarryCtxError
pub fn error_envelope(command: &str, err: &CarryCtxError) -> ErrorEnvelope {
    ErrorEnvelope {
        schema_version: SCHEMA_VERSION,
        command: command.to_string(),
        success: false,
        error: ErrorPayload {
            code: err.code.to_string(),
            message: err.message.clone(),
            details: err.details.clone(),
            suggestions: err.suggestions.clone(),
        },
    }
}

/// Output sink (stdout or stderr)
#[derive(Debug, Clone, Copy)]
pub enum OutputSink {
    Stdout,
    Stderr,
}

/// Render result to the appropriate stream
pub fn render_json<T: Serialize>(
    command: &str,
    result: Result<T, &CarryCtxError>,
    is_json: bool,
) -> (String, OutputSink, ExitCode) {
    render_json_with_warnings(command, result, is_json, vec![])
}

/// Render result to the appropriate stream, attaching non-fatal warnings to a
/// successful response. Warnings are ignored for an error result, since the
/// error envelope has no `warnings` field.
pub fn render_json_with_warnings<T: Serialize>(
    command: &str,
    result: Result<T, &CarryCtxError>,
    is_json: bool,
    warnings: Vec<String>,
) -> (String, OutputSink, ExitCode) {
    match result {
        Ok(data) => {
            if is_json {
                let envelope = success_envelope(command, data, warnings);
                let json = serde_json::to_string(&envelope).unwrap_or_else(|_| {
                    r#"{"schemaVersion":1,"command":"error","success":false,"error":{"code":"INTERNAL_ERROR","message":"Failed to serialize response"}}"#.into()
                });
                (json, OutputSink::Stdout, ExitCode::Success)
            } else {
                // Text output - simple implementation
                let text = serde_json::to_string_pretty(&data).unwrap_or_default();
                // Warnings must not follow the JSON document on stdout — a
                // trailing line breaks every `| jq` / `json.load` consumer.
                // Emit them on stderr instead.
                for warning in &warnings {
                    eprintln!("warning: {warning}");
                }
                (text, OutputSink::Stdout, ExitCode::Success)
            }
        }
        Err(err) => {
            let envelope = error_envelope(command, err);
            let json = serde_json::to_string(&envelope).unwrap_or_else(|_| {
                r#"{"schemaVersion":1,"command":"error","success":false,"error":{"code":"INTERNAL_ERROR","message":"Failed to serialize error"}}"#.into()
            });
            (json, OutputSink::Stderr, err.exit_code)
        }
    }
}

/// Render an entity result. JSON output is always the full success envelope
/// (public API). Text output is a compact one-line summary per command unless
/// `verbose` is set, in which case the full pretty-printed record is emitted.
///
/// `cli_fields` (from `--fields`) and `config_fields` (the per-command
/// `[output.fields]` table) optionally narrow the emitted record to an
/// allowlist of fields; the CLI flag takes precedence over configuration.
pub fn render_entity<T: Serialize>(
    command: &str,
    result: Result<T, &CarryCtxError>,
    is_json: bool,
    verbose: bool,
    cli_fields: Option<&[String]>,
    config_fields: Option<&std::collections::HashMap<String, Vec<String>>>,
    warnings: Vec<String>,
) -> (String, OutputSink, ExitCode) {
    let projection: Option<&[String]> =
        cli_fields.or_else(|| config_fields.and_then(|m| m.get(command).map(|v| v.as_slice())));
    match result {
        Ok(data) => {
            let mut value = serde_json::to_value(&data).unwrap_or_default();
            if let Some(fields) = projection {
                project(&mut value, fields);
            }
            if is_json {
                let envelope: SuccessEnvelope<serde_json::Value> = success_envelope(
                    command,
                    serde_json::from_value(value).unwrap_or_default(),
                    warnings,
                );
                let json = serde_json::to_string(&envelope).unwrap_or_else(|_| {
                    r#"{"schemaVersion":1,"command":"error","success":false,"error":{"code":"INTERNAL_ERROR","message":"Failed to serialize response"}}"#.into()
                });
                (json, OutputSink::Stdout, ExitCode::Success)
            } else if verbose {
                let text = serde_json::to_string_pretty(&value).unwrap_or_default();
                for warning in &warnings {
                    eprintln!("warning: {warning}");
                }
                (text, OutputSink::Stdout, ExitCode::Success)
            } else {
                (
                    compact_text(command, &value, projection),
                    OutputSink::Stdout,
                    ExitCode::Success,
                )
            }
        }
        Err(err) => {
            render_json_with_warnings::<serde_json::Value>(command, Err(err), is_json, warnings)
        }
    }
}

/// Keep only the listed top-level fields of an entity record (or of every
/// record in an array). Used by `--fields` and `[output.fields]`.
fn project(value: &mut Value, fields: &[String]) {
    match value {
        Value::Object(map) => map.retain(|key, _| fields.iter().any(|f| f == key)),
        Value::Array(items) => {
            for item in items {
                project(item, fields);
            }
        }
        _ => {}
    }
}

// ── Compact text rendering ────────────────────────────────────────────────
//
// Text mode emits short, single-line summaries by default so agent context
// stays small. Only the commands listed below get compact rendering; anything
// else falls back to the previous pretty-printed JSON text output.

const SUMMARY_MAX_CHARS: usize = 80;

fn field<'a>(obj: &'a Value, key: &str) -> &'a str {
    obj.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Truncate long free-text fields (titles, summaries) to keep output small.
fn clipped(value: &str) -> String {
    if value.chars().count() > SUMMARY_MAX_CHARS {
        let head: String = value.chars().take(SUMMARY_MAX_CHARS).collect();
        format!("{head}…")
    } else {
        value.to_string()
    }
}

/// Shorten a timestamp to `YYYY-MM-DD HH:MM` local-ish precision.
fn stamp(value: &str) -> String {
    let t = value.replace('T', " ");
    t.chars().take(16).collect()
}

/// Truncate a ULID to a stable short form for display.
fn ulid_short(value: &str) -> String {
    value.chars().take(8).collect()
}

fn first_of(obj: &Value, keys: &[&str]) -> String {
    for key in keys {
        let text = match obj.get(*key) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("; "),
            _ => String::new(),
        };
        if !text.is_empty() {
            return clipped(&text);
        }
    }
    String::new()
}

fn task_summary(obj: &Value, projection: Option<&[String]>) -> String {
    if projection.is_some() {
        // Explicit field projection: the template renders only the fields that
        // survived the projection, then any remaining projected-but-unrendered
        // fields are appended (e.g. `depends_on`), so `--fields` can never hide
        // a requested field or print empty brackets for a dropped one.
        let mut parts = Vec::new();
        if !field(obj, "display_id").is_empty() {
            parts.push(field(obj, "display_id").to_string());
        }
        if !field(obj, "status").is_empty() {
            parts.push(format!("[{}]", field(obj, "status")));
        }
        if !field(obj, "title").is_empty() {
            parts.push(clipped(field(obj, "title")));
        }
        let owner = field(obj, "owner_agent_id");
        if !owner.is_empty() {
            parts.push(format!("— owner {}", ulid_short(owner)));
        }
        let mut line = parts.join(" ");
        for extra in extra_fields(obj, &["display_id", "status", "title", "owner_agent_id"]) {
            line.push_str(&format!(" — {extra}"));
        }
        return line;
    }

    let title = clipped(field(obj, "title"));
    let owner = field(obj, "owner_agent_id");
    let mut line = format!(
        "{} [{}] {}",
        field(obj, "display_id"),
        field(obj, "status"),
        title
    );
    if !owner.is_empty() {
        line.push_str(&format!(" — owner {}", ulid_short(owner)));
    }
    line
}

/// Render the top-level fields of a projected record that the compact template
/// does not cover, as `label: value` fragments. Arrays of records become their
/// `display_id` list (e.g. `needs: CTX-0321, CTX-0322` for `depends_on`).
fn extra_fields(obj: &Value, covered: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let Value::Object(map) = obj else {
        return out;
    };
    for (key, value) in map {
        if covered.contains(&key.as_str()) {
            continue;
        }
        let rendered = match value {
            Value::Array(items) => {
                let ids: Vec<&str> = items
                    .iter()
                    .map(|item| field(item, "display_id"))
                    .filter(|s| !s.is_empty())
                    .collect();
                if ids.is_empty() || ids.len() != items.len() {
                    continue;
                }
                ids.join(", ")
            }
            Value::String(s) if !s.is_empty() => clipped(s),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            _ => continue,
        };
        let label = match key.as_str() {
            "depends_on" => "needs".to_string(),
            "blocks" => "blocks".to_string(),
            "owner_agent_id" => "owner".to_string(),
            other => other.to_string(),
        };
        out.push(format!("{label}: {rendered}"));
    }
    out
}

fn tasks_summary(obj: &Value, projection: Option<&[String]>) -> String {
    match obj {
        Value::Array(items) => {
            if items.is_empty() {
                return "No tasks.".to_string();
            }
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                if projection.is_some() {
                    let mut parts = Vec::new();
                    if !field(item, "display_id").is_empty() {
                        parts.push(field(item, "display_id").to_string());
                    }
                    if !field(item, "status").is_empty() {
                        parts.push(format!("[{}]", field(item, "status")));
                    }
                    if !field(item, "title").is_empty() {
                        parts.push(clipped(field(item, "title")));
                    }
                    let mut line = parts.join(" ");
                    for extra in extra_fields(item, &["display_id", "status", "title"]) {
                        line.push_str(&format!(" — {extra}"));
                    }
                    out.push(line);
                } else {
                    out.push(format!(
                        "{:<12} {:<12} {}",
                        field(item, "display_id"),
                        field(item, "status"),
                        clipped(field(item, "title"))
                    ));
                }
            }
            out.join("\n")
        }
        _ => serde_json::to_string_pretty(obj).unwrap_or_default(),
    }
}

fn agents_summary(obj: &Value) -> String {
    match obj {
        Value::Array(items) => {
            if items.is_empty() {
                return "No agents.".to_string();
            }
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(format!(
                    "{:<20} {:<16} {}",
                    field(item, "name"),
                    field(item, "provider"),
                    field(item, "status")
                ));
            }
            out.join("\n")
        }
        _ => serde_json::to_string_pretty(obj).unwrap_or_default(),
    }
}

fn checkpoints_summary(obj: &Value) -> String {
    match obj {
        Value::Array(items) => {
            if items.is_empty() {
                return "No checkpoints.".to_string();
            }
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let note = first_of(item, &["done", "remaining", "notes"]);
                out.push(format!(
                    "{:<12} {} {}",
                    ulid_short(field(item, "id")),
                    stamp(field(item, "created_at")),
                    note
                ));
            }
            out.join("\n")
        }
        _ => serde_json::to_string_pretty(obj).unwrap_or_default(),
    }
}

fn handoffs_summary(obj: &Value) -> String {
    match obj {
        Value::Array(items) => {
            if items.is_empty() {
                return "No handoffs.".to_string();
            }
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(format!(
                    "{:<10} {:<10} → {:<8} {}",
                    field(item, "display_id"),
                    field(item, "status"),
                    ulid_short(field(item, "target_agent_id")),
                    first_of(item, &["summary"])
                ));
            }
            out.join("\n")
        }
        _ => serde_json::to_string_pretty(obj).unwrap_or_default(),
    }
}

fn sessions_summary(obj: &Value) -> String {
    match obj {
        Value::Array(items) => {
            if items.is_empty() {
                return "No sessions.".to_string();
            }
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(format!(
                    "{:<12} {:<10} agent {:<8} started {}",
                    ulid_short(field(item, "id")),
                    field(item, "state"),
                    ulid_short(field(item, "agent_id")),
                    stamp(field(item, "created_at")).trim()
                ));
            }
            out.join("\n")
        }
        _ => serde_json::to_string_pretty(obj).unwrap_or_default(),
    }
}

fn events_summary(obj: &Value) -> String {
    let items: Vec<&Value> = match obj {
        Value::Object(_) => obj
            .get("events")
            .and_then(Value::as_array)
            .map(|a| a.iter().collect())
            .unwrap_or_default(),
        Value::Array(a) => a.iter().collect(),
        _ => return serde_json::to_string_pretty(obj).unwrap_or_default(),
    };
    if items.is_empty() {
        return "No events.".to_string();
    }
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        out.push(format!(
            "{} {:<22} task {:<8}",
            stamp(field(item, "occurred_at")),
            field(item, "event_type"),
            ulid_short(field(item, "task_id"))
        ));
    }
    out.join("\n")
}

fn progress_items_summary(obj: &Value) -> String {
    match obj {
        Value::Array(items) => {
            if items.is_empty() {
                return "No progress items.".to_string();
            }
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(format!(
                    "{:<10} {:<8} {}",
                    field(item, "display_id"),
                    field(item, "item_type"),
                    clipped(field(item, "content"))
                ));
            }
            out.join("\n")
        }
        _ => serde_json::to_string_pretty(obj).unwrap_or_default(),
    }
}

fn search_results_summary(obj: &Value) -> String {
    match obj {
        Value::Array(items) => {
            if items.is_empty() {
                return "No results.".to_string();
            }
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let id = match field(item, "display_id") {
                    "" => ulid_short(field(item, "id")),
                    d => d.to_string(),
                };
                out.push(format!(
                    "{:<12} {:<10} {:<12} {}",
                    id,
                    field(item, "kind"),
                    field(item, "task_status"),
                    clipped(&first_of(item, &["snippet"]))
                ));
            }
            out.join("\n")
        }
        _ => serde_json::to_string_pretty(obj).unwrap_or_default(),
    }
}

fn status_summary(obj: &Value) -> String {
    let branch = field(obj, "branch");
    let head = ulid_short(field(obj, "head"));
    let mut lines = vec![];
    if !field(obj, "projectName").is_empty() {
        lines.push(format!("Project: {}", field(obj, "projectName")));
    }
    if !branch.is_empty() || !field(obj, "head").is_empty() {
        lines.push(format!("Branch: {branch} ({head})").replace(" ()", ""));
    }
    let counts = [
        ("activeSessions", "sessions"),
        ("activeAgents", "agents"),
        ("tasks", "tasks"),
        ("worktrees", "worktrees"),
    ];
    let mut parts = Vec::new();
    for (key, label) in counts {
        let n = match obj.get(key) {
            Some(Value::Array(items)) => items.len(),
            _ => 0,
        };
        parts.push(format!("{n} {label}"));
    }
    lines.push(format!("Active: {}", parts.join(" | ")));
    if let Some(tasks) = obj.get("tasks").and_then(Value::as_array) {
        let active: Vec<&Value> = tasks
            .iter()
            .filter(|t| {
                let st = field(t, "status");
                st == "in_progress" || st == "ready" || st == "review" || st == "blocked"
            })
            .collect();
        if !active.is_empty() {
            lines.push("Active tasks:".to_string());
            for t in active.iter().take(15) {
                lines.push(format!(
                    "  {:<12} {:<12} {}",
                    field(t, "display_id"),
                    field(t, "status"),
                    clipped(field(t, "title"))
                ));
            }
            if active.len() > 15 {
                lines.push(format!("  … {} more", active.len() - 15));
            }
        }
    }
    lines.join("\n")
}

fn event_summary(obj: &Value) -> String {
    let mut line = format!(
        "{} {}",
        stamp(field(obj, "occurred_at")),
        field(obj, "event_type")
    );
    let task = ulid_short(field(obj, "task_id"));
    if !task.is_empty() {
        line.push_str(&format!(" — task {task}"));
    }
    if let Some(payload) = obj.get("payload").and_then(Value::as_object) {
        let mut bits = Vec::new();
        for key in ["branch", "summary", "message", "note", "reason"] {
            if let Some(Value::String(s)) = payload.get(key) {
                if !s.is_empty() {
                    bits.push(clipped(s));
                    break;
                }
            }
        }
        if !bits.is_empty() {
            line.push_str(&format!(" — {}", bits.join("; ")));
        }
    }
    line
}

fn decisions_summary(obj: &Value) -> String {
    match obj {
        Value::Array(items) => {
            if items.is_empty() {
                return "No decisions.".to_string();
            }
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(format!(
                    "{:<10} task {:<8} {}",
                    field(item, "display_id"),
                    ulid_short(field(item, "task_id")),
                    clipped(field(item, "title"))
                ));
            }
            out.join("\n")
        }
        _ => serde_json::to_string_pretty(obj).unwrap_or_default(),
    }
}

fn worktrees_summary(obj: &Value) -> String {
    match obj {
        Value::Array(items) => {
            if items.is_empty() {
                return "No worktrees.".to_string();
            }
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(format!(
                    "{:<12} task {:<8} {}",
                    ulid_short(field(item, "id")),
                    ulid_short(field(item, "task_id")),
                    clipped(field(item, "branch"))
                ));
            }
            out.join("\n")
        }
        _ => serde_json::to_string_pretty(obj).unwrap_or_default(),
    }
}

/// Render a compact one-line summary for a known command; fall back to
/// pretty-printed JSON for anything not covered here. When an explicit field
/// projection (`--fields` / `[output.fields]`) is in effect, the task
/// summaries render exactly the projected fields — never empty placeholders.
fn compact_text(command: &str, value: &Value, projection: Option<&[String]>) -> String {
    let summary: Option<String> = match command {
        "task.create" => Some(format!("Task created: {}", field(value, "display_id"))),
        "task.edit" => Some(format!("Task updated: {}", field(value, "display_id"))),
        "task.claim" => Some(format!("Task {} claimed", field(value, "display_id"))),
        "task.start" => Some(format!("Task {} started", field(value, "display_id"))),
        "task.release" => Some(format!("Task {} released", field(value, "display_id"))),
        "task.block" => Some(format!("Task {} blocked", field(value, "display_id"))),
        "task.unblock" => Some(format!("Task {} unblocked", field(value, "display_id"))),
        "task.complete" => Some(format!("Task {} completed", field(value, "display_id"))),
        "task.cancel" => Some(format!("Task {} cancelled", field(value, "display_id"))),
        "task.reopen" => Some(format!("Task {} reopened", field(value, "display_id"))),
        "task.depend" => Some(format!(
            "Dependency added to task {}",
            field(value, "display_id")
        )),
        "task.undepend" => Some(format!(
            "Dependency removed from task {}",
            field(value, "display_id")
        )),
        "handoff.accept" => Some(format!("Handoff {} accepted", field(value, "display_id"))),
        "handoff.reject" => Some(format!("Handoff {} rejected", field(value, "display_id"))),
        "handoff.close" => Some(format!("Handoff {} closed", field(value, "display_id"))),
        "checkpoint.correct" => Some(format!(
            "Checkpoint {} corrected",
            ulid_short(field(value, "id"))
        )),
        _ => None,
    };
    if let Some(line) = summary {
        return line;
    }
    match command {
        "task.show" => task_summary(value, projection),
        "task.list" => tasks_summary(value, projection),
        "task.review" => format!("Task {} in review", field(value, "display_id")),
        "agent.current" => field(value, "name").to_string(),
        "agent.register" => format!("Agent registered: {}", field(value, "name")),
        "agent.rename" => format!("Agent renamed: {}", field(value, "name")),
        "agent.deactivate" => format!("Agent deactivated: {}", field(value, "name")),
        "agent.show" => format!(
            "{} — {} [{}]",
            field(value, "name"),
            field(value, "provider"),
            field(value, "status")
        ),
        "agent.list" => agents_summary(value),
        "checkpoint.create" => {
            let mut line = format!(
                "Checkpoint saved: {} ({})",
                ulid_short(field(value, "id")),
                stamp(field(value, "created_at"))
            );
            let summary = first_of(value, &["done", "remaining", "notes"]);
            if !summary.is_empty() {
                line.push_str(&format!(" — {summary}"));
            }
            line
        }
        "checkpoint.show" => {
            let mut line = format!(
                "Checkpoint {} — {}",
                ulid_short(field(value, "id")),
                stamp(field(value, "created_at"))
            );
            let summary = first_of(value, &["done", "remaining", "notes"]);
            if !summary.is_empty() {
                line.push_str(&format!(" — {summary}"));
            }
            line
        }
        "checkpoint.list" => checkpoints_summary(value),
        "handoff.create" => format!(
            "Handoff created: {} → {}",
            field(value, "display_id"),
            ulid_short(field(value, "target_agent_id"))
        ),
        "handoff.show" => {
            let mut line = format!(
                "Handoff {} [{}] → {}",
                field(value, "display_id"),
                field(value, "status"),
                ulid_short(field(value, "target_agent_id"))
            );
            let summary = first_of(value, &["summary"]);
            if !summary.is_empty() {
                line.push_str(&format!(" — {summary}"));
            }
            line
        }
        "handoff.list" => handoffs_summary(value),
        "session.start" => format!("Session started: {}", ulid_short(field(value, "id"))),
        "session.pause" => format!("Session {} paused", ulid_short(field(value, "id"))),
        "session.resume" => format!("Session {} resumed", ulid_short(field(value, "id"))),
        "session.end" => format!("Session {} ended", ulid_short(field(value, "id"))),
        "session.abandon" => format!("Session {} abandoned", ulid_short(field(value, "id"))),
        "session.current" => format!(
            "Session {} [{}]",
            ulid_short(field(value, "id")),
            field(value, "state")
        ),
        "session.show" => format!(
            "Session {} [{}] — agent {} started {}",
            ulid_short(field(value, "id")),
            field(value, "state"),
            ulid_short(field(value, "agent_id")),
            stamp(field(value, "started_at"))
        ),
        "session.list" => sessions_summary(value),
        "status" => status_summary(value),
        "event.list" => events_summary(value),
        "event.show" => event_summary(value),
        "progress.create" => format!(
            "Progress {} added ({})",
            field(value, "display_id"),
            field(value, "item_type")
        ),
        "progress.show" => format!(
            "{} [{}] {}",
            field(value, "display_id"),
            field(value, "item_type"),
            clipped(field(value, "content"))
        ),
        "progress.edit" => format!("Progress {} updated", field(value, "display_id")),
        "progress.complete" => format!("Progress {} completed", field(value, "display_id")),
        "progress.reopen" => format!("Progress {} reopened", field(value, "display_id")),
        "progress.remove" => format!("Progress {} removed", field(value, "display_id")),
        "progress.reorder" => "Progress reordered".to_string(),
        "progress.list" => progress_items_summary(value),
        "search" => search_results_summary(value),
        "decision.add" => format!(
            "Decision {} recorded: {}",
            field(value, "display_id"),
            clipped(field(value, "title"))
        ),
        "decision.show" => {
            let mut line = format!(
                "{} — {}",
                field(value, "display_id"),
                clipped(field(value, "title"))
            );
            let body = first_of(value, &["decision", "context", "rationale"]);
            if !body.is_empty() {
                line.push_str(&format!("\n  {body}"));
            }
            line
        }
        "decision.list" => decisions_summary(value),
        "decision.search" => decisions_summary(value),
        "decision.supersede" => format!(
            "Decision {} superseded by {}",
            field(value, "display_id"),
            ulid_short(field(value, "superseded_by"))
        ),
        "worktree.create" => format!("Worktree created: {}", field(value, "branch")),
        "worktree.bind" => format!(
            "Worktree bound: {} → task {}",
            field(value, "branch"),
            ulid_short(field(value, "task_id"))
        ),
        "worktree.show" => {
            let mut line = format!(
                "Worktree {} — {}",
                ulid_short(field(value, "id")),
                clipped(field(value, "path"))
            );
            if !field(value, "branch").is_empty() {
                line.push_str(&format!(" ({})", field(value, "branch")));
            }
            line
        }
        "worktree.list" => worktrees_summary(value),
        "worktree.unbind" => format!("Worktree unbound: {}", clipped(field(value, "branch"))),
        _ => serde_json::to_string_pretty(value).unwrap_or_default(),
    }
}
