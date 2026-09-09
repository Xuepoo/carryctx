use carryctx::application::runtime::InvocationContext;
use carryctx::domain::pack::{PACK_FORMAT, PACK_FORMAT_VERSION};
use carryctx::error::ExitCode;
use clap::Parser;

// ── Version / contract metadata ────────────────────────────────────────

/// Show machine-readable contract versions (CLI, ctxpack, DB schema, skill surface).
///
/// Implements ADR state-transport-boundary §7: four contracts as a closed JSON shape.
/// CI compares `carryctx --json version` (or `carryctx version --json`) against the
/// normatively pinned values with strict equality. Unknown/missing/type-changed fields fail.
#[derive(Parser, Debug, Default)]
pub struct VersionArgs {
    /// Output in JSON format (also available as global --json / --format json).
    #[arg(long)]
    pub json: bool,

    /// Path to an expected contract JSON file to compare against (strict equality per ADR §7).
    /// When provided, mismatches report VALIDATION_FAILED (exit 8) instead of just emitting.
    #[arg(long, value_name = "FILE")]
    pub check: Option<String>,
}

fn contract_versions_payload() -> serde_json::Value {
    let db_schema = carryctx::adapter::sqlite::bundled_schema_version();
    serde_json::json!({
        "contract_versions": {
            "cli": env!("CARGO_PKG_VERSION"),
            "ctxpack_format": {
                "format": PACK_FORMAT,
                "format_version": PACK_FORMAT_VERSION
            },
            "db_schema": db_schema,
            "skill_surface": {
                "skill": "use-carryctx",
                "version": "1.1.0",
                "min_carryctx": env!("CARGO_PKG_VERSION")
            }
        }
    })
}

/// Strict closed-shape comparator per ADR §7. Missing, extra, or type-mismatched fields fail.
fn compare_contract_versions(
    actual: &serde_json::Value,
    expected: &serde_json::Value,
) -> Result<(), String> {
    let Some(actual_cv) = actual.get("contract_versions") else {
        return Err("actual is missing 'contract_versions'".into());
    };
    let Some(expected_cv) = expected.get("contract_versions") else {
        return Err("expected is missing 'contract_versions'".into());
    };

    // Closed top-level: exactly the four keys
    let allowed_top = ["cli", "ctxpack_format", "db_schema", "skill_surface"];
    for key in allowed_top {
        if !expected_cv.get(key).is_some() {
            return Err(format!("expected is missing contract_versions.{key}"));
        }
        if !actual_cv.get(key).is_some() {
            return Err(format!("actual is missing contract_versions.{key}"));
        }
    }
    if let Some(obj) = expected_cv.as_object() {
        for key in obj.keys() {
            if !allowed_top.contains(&key.as_str()) {
                return Err(format!(
                    "expected has unknown top-level field '{key}' (shape is closed)"
                ));
            }
        }
    }
    if let Some(obj) = actual_cv.as_object() {
        for key in obj.keys() {
            if !allowed_top.contains(&key.as_str()) {
                return Err(format!(
                    "actual has unknown top-level field '{key}' (shape is closed)"
                ));
            }
        }
    }

    // Field-by-field strict equality
    if actual_cv != expected_cv {
        // Find first differing leaf for a useful message
        for key in allowed_top {
            if actual_cv.get(key) != expected_cv.get(key) {
                return Err(format!(
                    "contract_versions.{key} mismatch: actual {} vs expected {}",
                    actual_cv.get(key).unwrap_or(&serde_json::Value::Null),
                    expected_cv.get(key).unwrap_or(&serde_json::Value::Null)
                ));
            }
        }
        return Err("contract_versions mismatch".into());
    }
    Ok(())
}

pub fn handle_version(
    args: &VersionArgs,
    ctx: &InvocationContext,
    is_json: bool,
) -> Result<ExitCode, ExitCode> {
    let emit_json = is_json || args.json;
    let payload = contract_versions_payload();

    if let Some(check_path) = &args.check {
        let content = std::fs::read_to_string(check_path).map_err(|e| {
            let err = carryctx::error::CarryCtxError::io_error(format!(
                "Failed to read --check file '{check_path}': {e}"
            ));
            let is_json = emit_json;
            crate::render_and_print::<serde_json::Value>("version", Err(err), is_json, ctx.quiet)
                .err()
                .unwrap_or(ExitCode::General)
        })?;
        let expected: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
            let err = carryctx::error::CarryCtxError::validation_error(format!(
                "Invalid JSON in --check file '{check_path}': {e}"
            ));
            crate::render_and_print::<serde_json::Value>("version", Err(err), emit_json, ctx.quiet)
                .err()
                .unwrap_or(ExitCode::Validation)
        })?;
        if let Err(reason) = compare_contract_versions(&payload, &expected) {
            let err = carryctx::error::CarryCtxError::validation_error(format!(
                "Contract version mismatch: {reason}"
            ))
            .with_details(serde_json::json!({
                "actual": payload["contract_versions"],
                "expected": expected.get("contract_versions").unwrap_or(&serde_json::Value::Null)
            }));
            return crate::render_and_print::<serde_json::Value>(
                "version",
                Err(err),
                emit_json,
                ctx.quiet,
            );
        }
    }

    let result: Result<serde_json::Value, carryctx::error::CarryCtxError> = Ok(payload);
    crate::render_and_print("version", result, emit_json, ctx.quiet)
}

#[cfg(test)]
mod tests {
    use super::*;
    use carryctx::adapter::sqlite::bundled_schema_version;
    use carryctx::domain::pack::{PACK_FORMAT, PACK_FORMAT_VERSION};

    #[test]
    fn version_payload_matches_adr_shape() {
        let payload = contract_versions_payload();
        let cv = &payload["contract_versions"];
        assert_eq!(cv["cli"], env!("CARGO_PKG_VERSION"));
        assert_eq!(cv["ctxpack_format"]["format"], PACK_FORMAT);
        assert_eq!(cv["ctxpack_format"]["format_version"], PACK_FORMAT_VERSION);
        assert_eq!(cv["db_schema"], bundled_schema_version());
        assert_eq!(cv["skill_surface"]["skill"], "use-carryctx");
        assert_eq!(cv["skill_surface"]["version"], "1.1.0");
        assert_eq!(
            cv["skill_surface"]["min_carryctx"],
            env!("CARGO_PKG_VERSION")
        );
    }

    #[test]
    fn version_payload_is_closed_shape() {
        let payload = contract_versions_payload();
        let cv = payload["contract_versions"].as_object().unwrap();
        let allowed = ["cli", "ctxpack_format", "db_schema", "skill_surface"];
        for key in cv.keys() {
            assert!(
                allowed.contains(&key.as_str()),
                "unexpected top-level field {key}"
            );
        }
        // types
        assert!(cv["cli"].is_string());
        assert!(cv["ctxpack_format"]["format"].is_string());
        assert!(cv["ctxpack_format"]["format_version"].is_number());
        assert!(cv["db_schema"].is_number());
        assert!(cv["skill_surface"]["skill"].is_string());
    }

    #[test]
    fn comparator_rejects_missing_and_extra_fields() {
        let actual = contract_versions_payload();
        let mut missing = actual.clone();
        missing["contract_versions"]
            .as_object_mut()
            .unwrap()
            .remove("db_schema");
        assert!(compare_contract_versions(&actual, &missing).is_err());
        assert!(compare_contract_versions(&missing, &actual).is_err());

        let mut extra = actual.clone();
        extra["contract_versions"]
            .as_object_mut()
            .unwrap()
            .insert("extra".into(), serde_json::Value::String("oops".into()));
        assert!(compare_contract_versions(&actual, &extra).is_err());
        assert!(compare_contract_versions(&extra, &actual).is_err());
    }

    #[test]
    fn comparator_rejects_type_mismatch() {
        let actual = contract_versions_payload();
        let mut typo = actual.clone();
        typo["contract_versions"]["db_schema"] = serde_json::Value::String("17".into());
        assert!(compare_contract_versions(&actual, &typo).is_err());
    }

    #[test]
    fn comparator_accepts_exact_match() {
        let actual = contract_versions_payload();
        assert!(compare_contract_versions(&actual, &actual).is_ok());
    }
}
