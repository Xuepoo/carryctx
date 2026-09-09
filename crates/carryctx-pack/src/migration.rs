//! Manifest migration for future `format_version` evolution.
//!
//! v1 ships with `PACK_FORMAT_VERSION = 1` and no forward migrator: older
//! `format_version` values are rejected by `validate_manifest_value`. This
//! module owns the migration entrypoint so future `format_version` bumps
//! (parents/DAG/three-way merge, checksum fields, new table layouts) can
//! land without touching `carryctx-core` or `carryctx-sqlite`.

use crate::manifest::{PACK_FORMAT_VERSION, validate_manifest_value};
use carryctx_core::error::CarryCtxError;

/// Media type / encoding hint for future pack layouts (reserved).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaType {
    /// Current `carryctx-pack-dir` directory layout.
    Dir,
}

/// Attempt to migrate a raw manifest JSON value to the current format
/// version, then validate it.
///
/// v1: older `format_version` values have no migrator and are rejected
/// with `VALIDATION_FAILED`. A future `format_version` is rejected with
/// `UNSUPPORTED_OPERATION`. When migration succeeds the returned
/// `PackManifest` is at `PACK_FORMAT_VERSION`.
pub fn migrate_manifest_value(
    value: &serde_json::Value,
) -> Result<crate::manifest::PackManifest, CarryCtxError> {
    let version = value
        .as_object()
        .and_then(|o| o.get("format_version"))
        .and_then(|v| v.as_u64());

    match version {
        Some(v) if v == u64::from(PACK_FORMAT_VERSION) => validate_manifest_value(value),
        Some(v) if v > u64::from(PACK_FORMAT_VERSION) => {
            Err(CarryCtxError::unsupported_operation(format!(
                "Pack format_version {v} is newer than supported version {PACK_FORMAT_VERSION}."
            )))
        }
        Some(v) => Err(CarryCtxError::validation_error(format!(
            "Pack format_version {v} is older than supported version {PACK_FORMAT_VERSION}; no v1 migrator exists."
        ))),
        None => Err(CarryCtxError::validation_error(
            "Pack manifest is missing an integer 'format_version'.".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{PackManifest, PackSource};
    use std::collections::BTreeMap;

    fn sample_value() -> serde_json::Value {
        let m = PackManifest::new(
            "0.8.1",
            17,
            "01KY6ZK0TMQM5ANGZ97T68C71G",
            "01M22DJSZX5MHJ33F2CRDQ23YD",
            "2026-09-09T06:00:00Z",
            PackSource {
                git_branch: None,
                git_commit: None,
                hostname: None,
            },
            BTreeMap::new(),
        );
        serde_json::to_value(m).unwrap()
    }

    #[test]
    fn current_version_migrates_to_validated_manifest() {
        let value = sample_value();
        let manifest = migrate_manifest_value(&value).unwrap();
        assert_eq!(manifest.format_version, PACK_FORMAT_VERSION);
    }

    #[test]
    fn future_version_is_unsupported() {
        let mut value = sample_value();
        value["format_version"] = serde_json::json!(999);
        let err = migrate_manifest_value(&value).unwrap_err();
        assert_eq!(err.code, "UNSUPPORTED_OPERATION");
    }

    #[test]
    fn older_version_without_migrator_is_validation_failed() {
        let mut value = sample_value();
        value["format_version"] = serde_json::json!(0);
        let err = migrate_manifest_value(&value).unwrap_err();
        assert_eq!(err.code, "VALIDATION_FAILED");
    }

    #[test]
    fn missing_version_is_validation_failed() {
        let mut value = sample_value();
        value.as_object_mut().unwrap().remove("format_version");
        let err = migrate_manifest_value(&value).unwrap_err();
        assert_eq!(err.code, "VALIDATION_FAILED");
    }
}
