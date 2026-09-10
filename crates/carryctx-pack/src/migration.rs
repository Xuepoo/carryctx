//! Manifest migration to the current `format_version`.
//!
//! v2 ships with an explicit in-memory v1->v2 migrator so existing v1
//! bundles keep importing unchanged for one release cycle after v2 writers
//! ship (`design/2026-09-10-mergeable-git-managed-state.md` §1.8):
//!
//! - v1 input: `parents = []`, no watermarks, no tombstones. The v1 layout
//!   has no `tombstones.jsonl`; absence is not a delete, so the migrated
//!   bundle simply carries an empty tombstone set. A `redacted` stamp is
//!   preserved.
//! - v2 input: validated and returned as-is.
//! - Future versions refuse with `UNSUPPORTED_OPERATION`; unknown legacy
//!   versions below v1 refuse with `VALIDATION_FAILED` (no migrator).
//!
//! The migration is in-memory only: nothing rewrites the input value or
//! the bundle directory.

use crate::manifest::{PackManifest, validate_manifest_value};
use carryctx_core::error::CarryCtxError;

/// Media type / encoding hint for future pack layouts (reserved).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaType {
    /// Current `carryctx-pack-dir` directory layout.
    Dir,
}

/// A manifest value migrated to the current format version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigratedManifest {
    /// Manifest normalized to [`crate::manifest::PACK_FORMAT_VERSION`]. v1
    /// inputs were upgraded in memory ([`PackManifest::into_current`]).
    pub manifest: PackManifest,
    /// The `format_version` declared on disk before migration (1 or 2).
    pub source_format_version: u32,
}

impl MigratedManifest {
    /// True when the input declared a version older than the current one.
    pub fn migrated(&self) -> bool {
        self.source_format_version < self.manifest.format_version
    }
}

/// Validate a raw manifest JSON value and migrate it to the current format
/// version, reporting which on-disk version it came from.
///
/// Version gating lives in [`validate_manifest_value`]: newer versions
/// report `UNSUPPORTED_OPERATION` (exit 10), unknown legacy versions report
/// `VALIDATION_FAILED` (exit 8).
pub fn migrate_manifest_value(
    value: &serde_json::Value,
) -> Result<MigratedManifest, CarryCtxError> {
    let manifest = validate_manifest_value(value)?;
    let source_format_version = manifest.format_version;
    Ok(MigratedManifest {
        manifest: manifest.into_current(),
        source_format_version,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{
        PACK_FORMAT, PACK_FORMAT_VERSION, PACK_FORMAT_VERSION_V1, PackManifest, PackSource,
    };
    use std::collections::BTreeMap;

    fn v2_value() -> serde_json::Value {
        let mut m = PackManifest::new(
            "0.9.1",
            17,
            "01KY6ZK0TMQM5ANGZ97T68C71G",
            "01M22DJSZX5MHJ33F2CRDQ23YD",
            "2026-09-09T06:00:00Z",
            PackSource {
                git_branch: None,
                git_commit: None,
                hostname: None,
            },
            BTreeMap::from([("tombstones".to_string(), 0)]),
        );
        m.parents = vec!["01PARENT".to_string()];
        m.redacted = true;
        serde_json::to_value(m).unwrap()
    }

    fn v1_value() -> serde_json::Value {
        serde_json::json!({
            "format": PACK_FORMAT,
            "format_version": 1,
            "carryctx_version": "0.8.1",
            "schema_version": 17,
            "project_id": "01KY6ZK0TMQM5ANGZ97T68C71G",
            "export_id": "01M22DJSZX5MHJ33F2CRDQ23YD",
            "created_at": "2026-09-09T06:00:00Z",
            "parents": [],
            "sequences": {},
            "source": {"hostname": "dev-a"},
            "counts": {"tasks": 2}
        })
    }

    #[test]
    fn current_version_migrates_to_validated_manifest() {
        let migrated = migrate_manifest_value(&v2_value()).unwrap();
        assert_eq!(migrated.source_format_version, 2);
        assert!(!migrated.migrated());
        assert_eq!(migrated.manifest.format_version, PACK_FORMAT_VERSION);
        assert_eq!(migrated.manifest.parents, vec!["01PARENT".to_string()]);
        assert!(migrated.manifest.redacted);
    }

    #[test]
    fn v1_upgrades_in_memory_with_parents_cleared() {
        let mut value = v1_value();
        value["parents"] = serde_json::json!(["01STALE"]);
        value["redacted"] = serde_json::json!(true);
        let migrated = migrate_manifest_value(&value).unwrap();
        assert_eq!(migrated.source_format_version, PACK_FORMAT_VERSION_V1);
        assert!(migrated.migrated());
        assert_eq!(migrated.manifest.format_version, PACK_FORMAT_VERSION);
        assert!(migrated.manifest.parents.is_empty());
        assert!(migrated.manifest.watermarks.is_empty());
        assert!(migrated.manifest.redacted);
        assert_eq!(migrated.manifest.counts["tombstones"], 0);
    }

    #[test]
    fn future_version_is_unsupported() {
        let mut value = v2_value();
        value["format_version"] = serde_json::json!(999);
        let err = migrate_manifest_value(&value).unwrap_err();
        assert_eq!(err.code, "UNSUPPORTED_OPERATION");
    }

    #[test]
    fn version_zero_without_migrator_is_validation_failed() {
        let mut value = v1_value();
        value["format_version"] = serde_json::json!(0);
        let err = migrate_manifest_value(&value).unwrap_err();
        assert_eq!(err.code, "VALIDATION_FAILED");
    }

    #[test]
    fn missing_version_is_validation_failed() {
        let mut value = v1_value();
        value.as_object_mut().unwrap().remove("format_version");
        let err = migrate_manifest_value(&value).unwrap_err();
        assert_eq!(err.code, "VALIDATION_FAILED");
    }
}
