//! Structure-preserving edits to TOML configuration documents.
//!
//! `config set`/`config unset` and project initialization (`init`, `import`)
//! all rewrite an existing configuration document through the same two
//! pre-write gates, so none of them can replace a working file with
//! unparseable or schema-invalid output.

use crate::error::CarryCtxError;

/// Parse the file into a structure-preserving document.
pub(crate) fn parse_config_document(
    path: &std::path::Path,
) -> Result<toml_edit::DocumentMut, CarryCtxError> {
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
pub(crate) fn write_config_document(
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
/// [`crate::domain::config::CarryCtxConfig`], unknown keys ignored).
///
/// Failure yields VALIDATION_FAILED naming the offending key plus a
/// recovery hint; file bytes are untouched because this runs before the
/// write. A document is also accepted when it repairs an *already invalid*
/// current file into a fully valid one — but any edit that leaves the
/// result invalid is rejected and pointed at the pre-existing bad key.
fn validate_typed_config(path: &std::path::Path, serialized: &str) -> Result<(), CarryCtxError> {
    let Err(err) = toml::from_str::<crate::domain::config::CarryCtxConfig>(serialized) else {
        return Ok(());
    };

    // Was the CURRENT file already schema-invalid before this operation?
    // A missing (or unreadable-for-other-reasons) file counts as valid:
    // there is nothing pre-existing to blame.
    let repairing_preexisting = std::fs::read_to_string(path)
        .map(|raw| toml::from_str::<crate::domain::config::CarryCtxConfig>(&raw).is_err())
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
             pre-existing value for '{0}' does not match the schema ({detail}). \
             Fix or remove the invalid value in {path_display}; the requested change was not applied.",
            k,
            path_display = path.display(),
        ),
        (Some(k), false) => format!(
            "Value for '{0}' does not match the configuration schema ({detail}). \
             Fix or remove the invalid value in {path_display}; the requested change was not applied.",
            k,
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
