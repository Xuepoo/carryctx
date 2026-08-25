use crate::*;
use carryctx::adapter::config::ConfigLoader;
use carryctx::adapter::xdg::XdgPaths;
use carryctx::application::runtime::InvocationContext;
use carryctx::error::{CarryCtxError, ExitCode};
use clap::Parser;

// ── Config ───────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
pub enum ConfigCommand {
    /// List all effective configuration values after merging global and project configs
    List {
        /// Only show global configuration values (ignore project-local config)
        #[arg(long)]
        global: bool,
    },
    /// Get the value of a specific configuration key
    Get { key: String },
    /// Set a configuration key to a specific value
    Set {
        key: String,
        value: String,
        /// Set the value in the global configuration file (~/.config/carryctx)
        #[arg(long)]
        global: bool,
        /// Set the value in the project-shared configuration (.carryctx/config.toml)
        #[arg(long = "cfg-project")]
        cfg_project: bool,
        /// Set the value in the user-local project configuration (.carryctx/local.toml)
        #[arg(long)]
        local: bool,
    },
    /// Remove a configuration key
    Unset {
        key: String,
        /// Remove from the global configuration
        #[arg(long)]
        global: bool,
        /// Remove from the project-shared configuration
        #[arg(long = "cfg-project")]
        cfg_project: bool,
        /// Remove from the user-local project configuration (.carryctx/local.toml)
        #[arg(long)]
        local: bool,
    },
    /// Validate the current configuration for syntax errors and schema compliance
    Validate,
    /// List the paths of all configuration files currently being merged
    Sources,
    /// Print the absolute path to a specific configuration file
    Path {
        /// Print the path to the global configuration file
        #[arg(long)]
        global: bool,
        /// Print the path to the project-local configuration file
        #[arg(long = "cfg-project")]
        cfg_project: bool,
    },
}

#[derive(Parser, Debug)]
pub struct ConfigArgs {
    /// Config subcommand to execute
    #[command(subcommand)]
    pub command: ConfigCommand,
}

// ═══════════════════════════════════════════════════════════════════════════
//  Handler: config
// ═══════════════════════════════════════════════════════════════════════════

pub fn handle_config(
    args: &ConfigArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    if let Some(result) = check_dry_run_envelope(
        ctx,
        &subcommand_label("config", &args.command),
        &format!("config {:?}", args.command),
    ) {
        return result;
    }
    let xdg = XdgPaths::new();
    let work_dir = resolve_work_dir(ctx);

    match &args.command {
        ConfigCommand::List { global } => {
            let cfg_path = if *global {
                xdg.global_config()
            } else {
                work_dir.join(".carryctx").join("config.toml")
            };
            // An unreadable file is an error, not silently-empty content.
            let content: Result<String, CarryCtxError> = if cfg_path.exists() {
                std::fs::read_to_string(&cfg_path).map_err(|e| {
                    CarryCtxError::configuration_error(format!(
                        "Failed to read {}: {e}",
                        cfg_path.display()
                    ))
                })
            } else {
                Ok(String::new())
            };
            let data = content.map(|content| {
                serde_json::json!({
                    "path": cfg_path.to_string_lossy(),
                    "content": content,
                })
            });
            render_and_print("config.list", data, is_json, ctx.quiet)
        }
        ConfigCommand::Get { key } => {
            let cfg_loader = ConfigLoader::new(xdg);
            match cfg_loader.load(Some(work_dir)) {
                Ok(config) => {
                    let value = lookup_config_value(&config, key);
                    let data = serde_json::json!({ "key": key, "value": value });
                    render_and_print("config.get", Ok(data), is_json, ctx.quiet)
                }
                Err(e) => {
                    render_and_print::<serde_json::Value>("config.get", Err(e), is_json, ctx.quiet)
                }
            }
        }
        ConfigCommand::Set {
            key,
            value,
            global,
            cfg_project,
            local,
        } => {
            let outcome = set_config_value(
                &xdg,
                work_dir,
                key,
                value,
                ScopeSelection {
                    global: *global,
                    cfg_project: *cfg_project,
                    local: *local,
                },
            );
            match outcome {
                Ok(path) => {
                    let data = serde_json::json!({
                        "path": path.to_string_lossy(),
                        "key": key,
                        "value": typed_json_preview(value),
                    });
                    render_and_print("config.set", Ok(data), is_json, ctx.quiet)
                }
                Err(e) => {
                    render_and_print::<serde_json::Value>("config.set", Err(e), is_json, ctx.quiet)
                }
            }
        }
        ConfigCommand::Unset {
            key,
            global,
            cfg_project,
            local,
        } => {
            let outcome = unset_config_value(
                &xdg,
                work_dir,
                key,
                ScopeSelection {
                    global: *global,
                    cfg_project: *cfg_project,
                    local: *local,
                },
            );
            match outcome {
                Ok((path, removed)) => {
                    let data = serde_json::json!({ "key": key, "removed": removed, "path": path.to_string_lossy() });
                    render_and_print("config.unset", Ok(data), is_json, ctx.quiet)
                }
                Err(e) => render_and_print::<serde_json::Value>(
                    "config.unset",
                    Err(e),
                    is_json,
                    ctx.quiet,
                ),
            }
        }
        ConfigCommand::Validate => {
            let cfg_loader = ConfigLoader::new(xdg);
            let result = cfg_loader.load(Some(work_dir));
            match result {
                Ok(config) => {
                    let data = serde_json::json!({
                        "valid": true,
                        "project": config.project,
                        "sources": ["global", "project", "env"]
                    });
                    render_and_print("config.validate", Ok(data), is_json, ctx.quiet)
                }
                Err(e) => render_and_print::<serde_json::Value>(
                    "config.validate",
                    Err(e),
                    is_json,
                    ctx.quiet,
                ),
            }
        }
        ConfigCommand::Sources => {
            let sources = serde_json::json!([
                { "name": "global", "path": xdg.global_config().to_string_lossy() },
                { "name": "project", "path": work_dir.join(".carryctx/config.toml").to_string_lossy() },
                { "name": "env", "prefix": "CARRYCTX_" },
            ]);
            render_and_print("config.sources", Ok(sources), is_json, ctx.quiet)
        }
        ConfigCommand::Path {
            global,
            cfg_project,
        } => {
            #[allow(clippy::if_same_then_else)]
            let path = if *global {
                xdg.global_config()
            } else if *cfg_project {
                work_dir.join(".carryctx").join("config.toml")
            } else {
                // default to project config
                work_dir.join(".carryctx").join("config.toml")
            };
            let data = serde_json::json!({ "path": path.to_string_lossy() });
            render_and_print("config.path", Ok(data), is_json, ctx.quiet)
        }
    }
}

// ── Scoped writes ────────────────────────────────────────────────────────

/// Explicit scope flags for `config set` / `config unset`.
struct ScopeSelection {
    global: bool,
    cfg_project: bool,
    local: bool,
}

/// Resolve exactly one explicit write scope into its target file path.
///
/// The v0.1 contract requires an explicit scope; there is no implicit default.
/// `--local` targets `.carryctx/local.toml`, which the loader does not read
/// yet, so it is rejected instead of silently writing a file nothing loads.
fn resolve_write_scope(
    xdg: &XdgPaths,
    work_dir: &std::path::Path,
    scope: &ScopeSelection,
) -> Result<std::path::PathBuf, CarryCtxError> {
    let selected: Vec<bool> = vec![scope.global, scope.cfg_project, scope.local];
    let count = selected.iter().filter(|b| **b).count();
    if count > 1 {
        return Err(CarryCtxError::invalid_arguments(
            "'config set'/'config unset' accepts exactly one write scope.",
        )
        .with_suggestions(["Use only one of --global, --cfg-project, or --local.".to_string()]));
    }
    if scope.local {
        return Err(CarryCtxError::unsupported_operation(
            "--local targets .carryctx/local.toml, which the configuration loader does not read yet.",
        )
        .with_suggestions([
            "Use --cfg-project to write .carryctx/config.toml (shared via Git).".to_string(),
            "Use --global to write the per-machine user configuration.".to_string(),
        ]));
    }
    if scope.global {
        Ok(xdg.global_config())
    } else if scope.cfg_project {
        Ok(work_dir.join(".carryctx").join("config.toml"))
    } else {
        Err(CarryCtxError::invalid_arguments(
            "No write scope given: 'config set'/'config unset' require --global or --cfg-project.",
        )
        .with_suggestions([
            "--global          → ~/.config/carryctx/config.toml (per machine)".to_string(),
            "--cfg-project     → <repo>/.carryctx/config.toml (shared via Git)".to_string(),
        ]))
    }
}

/// Parse the file into a structure-preserving document.
fn parse_config_document(path: &std::path::Path) -> Result<toml_edit::DocumentMut, CarryCtxError> {
    if !path.exists() {
        return Ok(toml_edit::DocumentMut::new());
    }
    let raw = std::fs::read_to_string(path).map_err(|e| {
        CarryCtxError::configuration_error(format!("Failed to read {}: {e}", path.display()))
    })?;
    raw.parse::<toml_edit::DocumentMut>().map_err(|e| {
        CarryCtxError::configuration_error(format!("{} is not valid TOML: {e}", path.display()))
    })
}

/// Serialize, round-trip validate, type-check against the loader schema,
/// then write the document.
///
/// Two gates run before a single byte reaches disk:
/// 1. a TOML syntax re-parse (never replace a working file with unparseable
///    output), and
/// 2. CTX-0082: a typed deserialization into the same configuration model
///    the loader (`adapter/config.rs`) enforces, so a type-mismatched value
///    can never brick every later command in the project with
///    CONFIGURATION_ERROR.
fn write_config_document(
    path: &std::path::Path,
    doc: &toml_edit::DocumentMut,
) -> Result<(), CarryCtxError> {
    let serialized = doc.to_string();
    serialized.parse::<toml_edit::DocumentMut>().map_err(|e| {
        CarryCtxError::configuration_error(format!(
            "Refusing to write {}: edited result is not valid TOML ({e})",
            path.display()
        ))
    })?;
    validate_typed_config(path, &serialized)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            CarryCtxError::configuration_error(format!(
                "Failed to create directory {}: {e}",
                parent.display()
            ))
        })?;
    }
    std::fs::write(path, serialized).map_err(|e| {
        CarryCtxError::configuration_error(format!("Failed to write {}: {e}", path.display()))
    })
}

/// Validate a serialized configuration document against the typed model
/// using the loader's exact serde semantics (`toml::from_str` into
/// [`carryctx::domain::config::CarryCtxConfig`], unknown keys ignored).
///
/// Failure yields VALIDATION_FAILED naming the offending key plus a
/// recovery hint; file bytes are untouched because this runs before the
/// write. A document is also accepted when it repairs an *already invalid*
/// current file into a fully valid one — but any edit that leaves the
/// result invalid is rejected and pointed at the pre-existing bad key.
fn validate_typed_config(path: &std::path::Path, serialized: &str) -> Result<(), CarryCtxError> {
    let Err(err) = toml::from_str::<carryctx::domain::config::CarryCtxConfig>(serialized) else {
        return Ok(());
    };

    // Was the CURRENT file already schema-invalid before this operation?
    // A missing (or unreadable-for-other-reasons) file counts as valid:
    // there is nothing pre-existing to blame.
    let repairing_preexisting = std::fs::read_to_string(path)
        .map(|raw| toml::from_str::<carryctx::domain::config::CarryCtxConfig>(&raw).is_err())
        .unwrap_or(false);

    // Locate the offending key from the serde error's reported line,
    // resolved against the enclosing table header (see helper).
    let key = offending_key_from_error(serialized, &err);
    let detail = err.message().trim().to_string();
    let detail = if detail.is_empty() {
        err.to_string()
    } else {
        detail
    };

    let message = match (&key, repairing_preexisting) {
        (Some(k), true) => format!(
            "Configuration {path_display} is still invalid after this change: \
             pre-existing value for '{k}' does not match the schema ({detail}). \
             Fix or remove the invalid value in {path_display}; the requested change was not applied.",
            path_display = path.display(),
        ),
        (Some(k), false) => format!(
            "Value for '{k}' does not match the configuration schema ({detail}). \
             Fix or remove the invalid value in {path_display}; the requested change was not applied.",
            path_display = path.display(),
        ),
        (None, true) => format!(
            "Configuration {path_display} is still invalid after this change ({detail}). \
             Fix or remove the invalid value in {path_display}; the requested change was not applied.",
            path_display = path.display(),
        ),
        (None, false) => format!(
            "Value does not match the configuration schema ({detail}). \
             Fix or remove the invalid value in {path_display}; the requested change was not applied.",
            path_display = path.display(),
        ),
    };
    Err(CarryCtxError::validation_error(message))
}

/// Resolve the dotted key that failed typed validation.
///
/// `toml` renders deserialization errors with a line/column pointer into
/// the document ("TOML parse error at line L, column C …"). The offending
/// value sits on that line, so the key is whatever precedes `=` there,
/// qualified by the nearest preceding `[table]` header. Root-level dotted
/// keys (how this module writes `a.b` leaves) already carry their full
/// path and appear before any table header, so they resolve directly.
/// Best-effort: any parsing surprise yields None and callers fall back to
/// a generic message.
fn offending_key_from_error(serialized: &str, err: &toml::de::Error) -> Option<String> {
    let rendered = err.to_string();
    let marker = rendered.split("line ").nth(1)?;
    let line_no: usize = marker.split(',').next()?.trim().parse().ok()?;
    let lines: Vec<&str> = serialized.lines().collect();
    let value_line = lines.get(line_no.checked_sub(1)?)?;

    let candidate = match value_line.split_once('=') {
        Some((key_part, _)) => key_part.trim().trim_matches('"').to_string(),
        None => return None,
    };
    if candidate.is_empty() {
        return None;
    }

    // Find the enclosing table section, if any.
    let mut section: Option<String> = None;
    for line in lines.iter().take(line_no.saturating_sub(1)) {
        let trimmed = line.trim();
        if trimmed.starts_with("[[") && trimmed.ends_with("]]") {
            section = Some(format!(
                "{}.0",
                trimmed.trim_matches(|c| c == '[' || c == ']')
            ));
        } else if trimmed.starts_with('[') && trimmed.ends_with(']') {
            section = Some(trimmed.trim_matches(|c| c == '[' || c == ']').to_string());
        }
    }
    Some(match section {
        Some(table) => format!("{table}.{candidate}"),
        None => candidate,
    })
}

/// Interpret a raw CLI value as a typed TOML scalar.
///
/// `true`/`false` become booleans, integer- and float-shaped strings become
/// numbers, everything else stays a literal string. This replaces the old
/// behavior of quoting every value, which turned `set k true` into the
/// string `'true'` and broke typed consumers.
fn typed_toml_value(raw: &str) -> toml_edit::Value {
    let trimmed = raw.trim();
    match trimmed {
        "true" => return toml_edit::Value::from(true),
        "false" => return toml_edit::Value::from(false),
        _ => {}
    }
    // TOML forbids leading-zero numerals (`0755`, `-08`); keep those literal
    // instead of silently reinterpreting them as decimal values.
    if !has_leading_zero_numeral(trimmed) {
        if let Ok(int) = trimmed.parse::<i64>() {
            return toml_edit::Value::from(int);
        }
        if let Some(float) = parse_toml_float(trimmed) {
            return toml_edit::Value::from(float);
        }
    }
    toml_edit::Value::from(raw)
}

/// Whether `s` is an all-digit numeral with a forbidden leading zero
/// (`0755`, `-012`). A lone `0` is fine.
fn has_leading_zero_numeral(s: &str) -> bool {
    let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
    if digits.len() <= 1 || !digits.starts_with('0') {
        return false;
    }
    digits[1..].bytes().all(|b| b.is_ascii_digit())
}

/// Float parsing restricted to TOML-valid forms (`inf`/`NaN` spellings are
/// intentionally left as plain strings).
fn parse_toml_float(trimmed: &str) -> Option<f64> {
    if !trimmed.contains(['.', 'e', 'E']) {
        return None;
    }
    trimmed.parse::<f64>().ok().filter(|f| f.is_finite())
}

/// The JSON-typed preview of a raw CLI value used in the success envelope.
///
/// Reuses the mainline `toml` serializer so the preview matches exactly what
/// a subsequent parse of the written file will produce.
fn typed_json_preview(raw: &str) -> serde_json::Value {
    let mut doc = toml_edit::DocumentMut::new();
    doc["v"] = toml_edit::Item::Value(typed_toml_value(raw));
    toml::from_str::<toml::Value>(&doc.to_string())
        .ok()
        .and_then(|parsed| parsed.get("v").cloned())
        .map(toml_value_to_json)
        .unwrap_or_else(|| serde_json::Value::String(raw.to_string()))
}

/// Convert a parsed TOML value into its JSON equivalent.
fn toml_value_to_json(value: toml::Value) -> serde_json::Value {
    match value {
        toml::Value::Boolean(b) => serde_json::Value::Bool(b),
        toml::Value::Integer(i) => serde_json::json!(i),
        toml::Value::Float(f) => serde_json::json!(f),
        toml::Value::String(s) => serde_json::Value::String(s),
        toml::Value::Datetime(dt) => serde_json::Value::String(dt.to_string()),
        toml::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(toml_value_to_json).collect())
        }
        toml::Value::Table(map) => {
            let mut obj = serde_json::Map::new();
            for (key, val) in map {
                obj.insert(key, toml_value_to_json(val));
            }
            serde_json::Value::Object(obj)
        }
    }
}

/// Walk (creating as needed) the intermediate tables for a dotted key and
/// return the table that owns the leaf segment.
fn table_for_key<'a>(
    doc: &'a mut toml_edit::DocumentMut,
    parts: &[&str],
) -> Result<&'a mut toml_edit::Table, CarryCtxError> {
    let mut current = doc.as_table_mut();
    for part in &parts[..parts.len() - 1] {
        let entry = current
            .entry(part)
            .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
        if !entry.is_table() {
            return Err(CarryCtxError::state_conflict(format!(
                "Cannot descend into '{part}': it is already a scalar value."
            )));
        }
        current = entry.as_table_mut().expect("checked as table");
    }
    Ok(current)
}

/// Insert or overwrite a dotted key at the document root.
///
/// `toml_edit` keeps root-level dotted keys out of any `[table]` section,
/// fixing the old append-at-EOF bug where `task.strict_completion = …`
/// landed inside the last `[verification]` table.
fn insert_config_key(
    doc: &mut toml_edit::DocumentMut,
    key: &str,
    value: toml_edit::Value,
) -> Result<(), CarryCtxError> {
    let parts: Vec<&str> = key.split('.').collect();
    if parts.iter().any(|p| p.trim().is_empty()) {
        return Err(CarryCtxError::invalid_arguments(format!(
            "Invalid configuration key '{key}': empty path segment."
        )));
    }
    let leaf = *parts.last().expect("non-empty split always has a last");
    let table = table_for_key(doc, &parts)?;
    if table
        .get(leaf)
        .map(|existing| existing.is_table())
        .unwrap_or(false)
    {
        return Err(CarryCtxError::state_conflict(format!(
            "Refusing to overwrite '{key}': it is a table with nested keys."
        )));
    }
    table.insert(leaf, toml_edit::Item::Value(value));
    Ok(())
}

/// Remove a dotted key from the document, returning whether it existed.
fn remove_config_key(doc: &mut toml_edit::DocumentMut, key: &str) -> Result<bool, CarryCtxError> {
    let parts: Vec<&str> = key.split('.').collect();
    if parts.iter().any(|p| p.trim().is_empty()) {
        return Err(CarryCtxError::invalid_arguments(format!(
            "Invalid configuration key '{key}': empty path segment."
        )));
    }
    let mut current = doc.as_table_mut();
    for part in &parts[..parts.len() - 1] {
        match current.get_mut(part).and_then(|item| item.as_table_mut()) {
            Some(table) => current = table,
            None => return Ok(false),
        }
    }
    let leaf = *parts.last().expect("non-empty split always has a last");
    Ok(current.remove(leaf).is_some())
}

fn set_config_value(
    xdg: &XdgPaths,
    work_dir: &std::path::Path,
    key: &str,
    value: &str,
    scope: ScopeSelection,
) -> Result<std::path::PathBuf, CarryCtxError> {
    let path = resolve_write_scope(xdg, work_dir, &scope)?;
    let mut doc = parse_config_document(&path)?;
    insert_config_key(&mut doc, key, typed_toml_value(value))?;
    write_config_document(&path, &doc)?;
    Ok(path)
}

fn unset_config_value(
    xdg: &XdgPaths,
    work_dir: &std::path::Path,
    key: &str,
    scope: ScopeSelection,
) -> Result<(std::path::PathBuf, bool), CarryCtxError> {
    let path = resolve_write_scope(xdg, work_dir, &scope)?;
    if !path.exists() {
        return Ok((path, false));
    }
    let mut doc = parse_config_document(&path)?;
    let removed = remove_config_key(&mut doc, key)?;
    if removed {
        write_config_document(&path, &doc)?;
    }
    Ok((path, removed))
}

/// Look up a dotted key in the loaded (merged) configuration by walking the
/// serialized typed struct — never by line-prefix matching over raw TOML,
/// which returned empty results for nested keys and wrong hits on prefix
/// collisions. Missing keys yield `null`.
fn lookup_config_value(
    config: &carryctx::domain::config::CarryCtxConfig,
    key: &str,
) -> serde_json::Value {
    let mut cursor = match serde_json::to_value(config) {
        Ok(value) => value,
        Err(_) => return serde_json::Value::Null,
    };
    for part in key.split('.') {
        match cursor.get(part) {
            Some(next) => cursor = next.clone(),
            None => return serde_json::Value::Null,
        }
    }
    cursor
}

#[cfg(test)]
mod config_cli_tests {
    use super::*;

    fn doc_from(raw: &str) -> toml_edit::DocumentMut {
        raw.parse::<toml_edit::DocumentMut>().expect("valid toml")
    }

    #[test]
    fn typed_values_are_not_all_strings() {
        assert!(matches!(
            typed_toml_value("true"),
            toml_edit::Value::Boolean(_)
        ));
        assert!(matches!(
            typed_toml_value("42"),
            toml_edit::Value::Integer(_)
        ));
        assert!(matches!(
            typed_toml_value("1.5"),
            toml_edit::Value::Float(_)
        ));
        match typed_toml_value("4h") {
            toml_edit::Value::String(formatted) => {
                assert_eq!(formatted.value(), "4h");
            }
            other => panic!("'4h' must stay a string, got {other:?}"),
        }
    }

    #[test]
    fn dotted_keys_land_at_root_not_in_last_table() {
        let mut doc = doc_from("[verification]\ncommands = [\"x\"]\n");
        insert_config_key(&mut doc, "task.strict_completion", typed_toml_value("true"))
            .expect("insert");
        let out = doc.to_string();
        let reparsed = out.parse::<toml_edit::DocumentMut>().expect("round trip");
        // The key must be readable as task.strict_completion…
        assert_eq!(
            reparsed["task"]["strict_completion"].as_bool(),
            Some(true),
            "typed boolean must survive the round trip:\n{out}"
        );
        // …and the [verification] section must be untouched.
        assert_eq!(
            reparsed["verification"]["commands"]
                .as_array()
                .map(|a| a.len()),
            Some(1),
            "existing table content must survive:\n{out}"
        );
        // strict_completion must not appear inside the [verification] body.
        let verification_body = out
            .split("[verification]")
            .nth(1)
            .expect("verification header present");
        let verification_body = match verification_body.split_once('[') {
            Some((before_next_table, _)) => before_next_table,
            None => verification_body,
        };
        assert!(
            !verification_body.contains("strict_completion"),
            "dotted key leaked into [verification]:\n{out}"
        );
    }

    #[test]
    fn existing_tables_and_comments_survive_set() {
        let mut doc =
            doc_from("# header comment\n[task]\n# inner\nsingle_active_task_per_agent = true\n");
        insert_config_key(
            &mut doc,
            "task.strict_completion",
            typed_toml_value("false"),
        )
        .expect("insert");
        let out = doc.to_string();
        assert!(out.contains("# header comment"));
        assert!(out.contains("# inner"));
        assert_eq!(
            doc["task"]["single_active_task_per_agent"].as_bool(),
            Some(true)
        );
    }

    #[test]
    fn unset_removes_dotted_leaf_only() {
        let mut doc =
            doc_from("[task]\nstrict_completion = false\n\n[verification]\ncommands = []\n");
        assert!(remove_config_key(&mut doc, "task.strict_completion").expect("remove"));
        let out = doc.to_string();
        assert!(!out.contains("strict_completion"));
        assert!(out.contains("[verification]"), "unrelated tables stay");
        assert!(!remove_config_key(&mut doc, "missing.key").expect("remove absent"));
    }

    #[test]
    fn refusing_to_clobber_a_table() {
        let mut doc = doc_from("[verification]\ncommands = []\n");
        let err = insert_config_key(&mut doc, "verification", typed_toml_value("oops"));
        assert!(err.is_err(), "must not replace a table with a scalar");
    }

    #[test]
    fn multibyte_values_round_trip() {
        let mut doc = doc_from("");
        insert_config_key(
            &mut doc,
            "project.name",
            typed_toml_value("项目「テスト」🚀"),
        )
        .expect("insert");
        let out = doc.to_string();
        let reparsed = out.parse::<toml_edit::DocumentMut>().expect("round trip");
        assert_eq!(
            reparsed["project"]["name"].as_str(),
            Some("项目「テスト」🚀")
        );
    }

    #[test]
    fn lookup_walks_nested_keys_and_returns_null_for_unknowns() {
        let config = carryctx::domain::config::CarryCtxConfig::default();
        assert_eq!(
            lookup_config_value(&config, "task.strict_completion"),
            serde_json::json!(config.task.strict_completion)
        );
        assert_eq!(
            lookup_config_value(&config, "git.main_branch"),
            serde_json::json!(config.git.main_branch)
        );
        assert_eq!(
            lookup_config_value(&config, "no.such.key"),
            serde_json::Value::Null
        );
        // Prefix collision: 'task' vs 'task_prefix'-like names must not match.
        assert_eq!(
            lookup_config_value(&config, "proj.name"),
            serde_json::Value::Null
        );
    }

    #[test]
    fn float_like_and_leading_zero_stay_strings_or_parse_cleanly() {
        assert!(matches!(
            typed_toml_value("0755"),
            toml_edit::Value::String(_)
        ));
        assert!(matches!(
            typed_toml_value("-12"),
            toml_edit::Value::Integer(_)
        ));
    }

    // ── CTX-0082: typed schema validation before write ───────────────────

    #[test]
    fn typed_write_rejects_wrong_type_and_leaves_file_bytes_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let original = "[task]\nsingle_active_task_per_agent = true\n";
        std::fs::write(&path, original).unwrap();

        let mut doc = parse_config_document(&path).unwrap();
        insert_config_key(
            &mut doc,
            "task.strict_completion",
            typed_toml_value("notabool"),
        )
        .unwrap();
        let err = write_config_document(&path, &doc).unwrap_err();

        assert_eq!(err.code, "VALIDATION_FAILED", "{err}");
        assert_eq!(err.exit_code, ExitCode::Validation, "{err}");
        assert!(
            err.message.contains("task.strict_completion"),
            "must name the offending key: {}",
            err.message
        );
        assert!(
            err.message.to_lowercase().contains("fix or remove"),
            "must carry a recovery hint: {}",
            err.message
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original,
            "rejected write must leave file bytes unchanged"
        );
    }

    #[test]
    fn typed_writes_pass_schema_for_bool_int_string_and_dotted_tables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[verification]\ncommands = [\"cargo test\"]\n").unwrap();

        for (key, value) in [
            ("task.strict_completion", "true"),
            ("task.list_limit", "300"),
            ("session.stale_after", "3h"),
            ("agent.default_name", "alpha"),
        ] {
            let mut doc = parse_config_document(&path).unwrap();
            insert_config_key(&mut doc, key, typed_toml_value(value)).unwrap();
            write_config_document(&path, &doc)
                .unwrap_or_else(|e| panic!("set {key}={value} must pass the schema gate: {e}"));
        }

        let final_text = std::fs::read_to_string(&path).unwrap();
        let parsed: carryctx::domain::config::CarryCtxConfig =
            toml::from_str(&final_text).expect("final file must load through the loader schema");
        assert!(parsed.task.strict_completion);
        assert_eq!(parsed.task.list_limit, 300);
        assert_eq!(parsed.session.stale_after, "3h");
        assert_eq!(parsed.agent.default_name.as_deref(), Some("alpha"));
        assert_eq!(parsed.verification.commands, vec!["cargo test"]);
    }

    #[test]
    fn set_repairs_already_invalid_file_when_result_is_fully_valid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[task]\nstrict_completion = \"oops\"\n").unwrap();

        let mut doc = parse_config_document(&path).unwrap();
        insert_config_key(&mut doc, "task.strict_completion", typed_toml_value("true")).unwrap();
        write_config_document(&path, &doc).expect("repairing write must be allowed");

        let final_text = std::fs::read_to_string(&path).unwrap();
        let parsed: carryctx::domain::config::CarryCtxConfig =
            toml::from_str(&final_text).expect("repaired file must be fully valid");
        assert!(parsed.task.strict_completion);
    }

    #[test]
    fn set_on_invalid_file_is_blocked_when_result_still_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let original = "[context]\nmax_events = \"lots\"\n";
        std::fs::write(&path, original).unwrap();

        let mut doc = parse_config_document(&path).unwrap();
        insert_config_key(&mut doc, "task.strict_completion", typed_toml_value("true")).unwrap();
        let err = write_config_document(&path, &doc).unwrap_err();

        assert_eq!(err.code, "VALIDATION_FAILED", "{err}");
        assert!(
            err.message.contains("context.max_events"),
            "must point at the pre-existing bad key: {}",
            err.message
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original,
            "blocked repair must not touch the file"
        );
    }

    #[test]
    fn unknown_keys_stay_accepted_matching_loader_policy() {
        // The loader's serde model ignores unknown keys (no
        // deny_unknown_fields); the gate must not be stricter than that.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut doc = parse_config_document(&path).unwrap();
        insert_config_key(&mut doc, "custom.nonsense", typed_toml_value("anything")).unwrap();
        write_config_document(&path, &doc).expect("unknown keys must stay accepted");
    }
}
