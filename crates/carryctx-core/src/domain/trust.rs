//! Project trust for executable repository policy (CTX-0100).
//!
//! Declarative `.carryctx` data (ids, names, prefixes, output preferences) is
//! always safe to read. Policy that *executes*, and whose argv/URL originates
//! from the repository's `.carryctx/` files, is **external** and is denied
//! until two independent, user-controlled keys allow it:
//!
//! 1. the global security gate `security.allow_project_commands` is `true`, and
//! 2. the project id is present in the local trust registry with `trusted:
//!    true` and a `policy_fingerprint` matching the currently declared policy.
//!
//! Nothing under `.carryctx/` or version control can grant trust. Built-in
//! actions shipped inside the binary are trusted by construction and never
//! consult the registry.
//!
//! This module is pure: it computes fingerprints, the registry data model, and
//! the verdict. Reading and writing the on-disk registry is the CLI adapter's
//! responsibility.

use std::collections::BTreeMap;

use crate::domain::config::CarryCtxConfig;
use crate::error::{CarryCtxError, ExitCode};

/// Public error code for a blocked external action (exit 9).
pub const TRUST_DENIED_CODE: &str = "TRUST_DENIED";

/// The registry file schema this binary understands. Unknown versions fail
/// closed.
pub const TRUST_REGISTRY_SCHEMA_VERSION: u64 = 1;

/// The kind of action being authorized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionSurface {
    /// Shipped with the binary; trusted by construction, never gated.
    BuiltIn,
    /// argv/URL that originated from the repository's `.carryctx/` files.
    External,
}

/// One project's decision in the trust registry.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustEntry {
    pub trusted: bool,
    #[serde(default)]
    pub project_name: String,
    pub decided_at: String,
    pub decided_by: String,
    pub policy_fingerprint: String,
}

/// The on-disk trust registry.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustRegistry {
    pub schema_version: u64,
    #[serde(default)]
    pub trusted: BTreeMap<String, TrustEntry>,
}

impl Default for TrustRegistry {
    fn default() -> Self {
        Self {
            schema_version: TRUST_REGISTRY_SCHEMA_VERSION,
            trusted: BTreeMap::new(),
        }
    }
}

/// Whether external policy may execute for the evaluated project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustEffective {
    Allowed,
    Blocked,
    NotApplicable,
}

impl TrustEffective {
    pub fn as_str(&self) -> &'static str {
        match self {
            TrustEffective::Allowed => "allowed",
            TrustEffective::Blocked => "blocked",
            TrustEffective::NotApplicable => "not_applicable",
        }
    }
}

/// Why the verdict was reached. Stable machine-readable reason strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustReason {
    Allowed,
    NoExternalPolicy,
    GlobalDisabled,
    NotTrusted,
    PolicyChanged,
}

impl TrustReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            TrustReason::Allowed => "allowed",
            TrustReason::NoExternalPolicy => "no_external_policy",
            TrustReason::GlobalDisabled => "global_disabled",
            TrustReason::NotTrusted => "not_trusted",
            TrustReason::PolicyChanged => "policy_changed",
        }
    }
}

/// The result of evaluating the trust gate for a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustEvaluation {
    pub effective: TrustEffective,
    pub reason: TrustReason,
    /// True when a registry entry for the project is trusted *and* its stored
    /// fingerprint matches the declared policy, independent of the global gate.
    pub trusted: bool,
    pub external_policy_present: bool,
    pub external_policy_fingerprint: Option<String>,
}

impl TrustEvaluation {
    pub fn is_allowed(&self) -> bool {
        self.effective == TrustEffective::Allowed
    }
}

/// The ordered external policy declared by the repository.
///
/// This is the closed list of gated surfaces. Today only `[verification]
/// commands` is executable policy; when lifecycle hooks or automation actions
/// land they must be appended here and in the design document, otherwise they
/// are not gated and must not execute.
pub fn collect_external_policy(config: &CarryCtxConfig) -> Vec<String> {
    config.verification.commands.clone()
}

/// `sha256(serde_json::to_string(policy))`, prefixed for algorithm agility.
///
/// The serialization is the canonical form: a JSON array of the declared
/// command strings in declaration order. Order is significant.
pub fn policy_fingerprint(policy: &[String]) -> String {
    let canonical = serde_json::to_string(policy).unwrap_or_else(|_| "[]".to_string());
    format!("sha256:{}", sha256_hex(canonical.as_bytes()))
}

/// Evaluate the verdict for `project_id` against its declared policy and the
/// local registry. `registry` is always the fail-closed result of loading the
/// on-disk registry: an absent, malformed, insecure, or unreadable file is an
/// empty registry.
pub fn evaluate(
    config: &CarryCtxConfig,
    project_id: &str,
    registry: &TrustRegistry,
) -> TrustEvaluation {
    let policy = collect_external_policy(config);
    let external_policy_present = !policy.is_empty();
    let external_policy_fingerprint = if external_policy_present {
        Some(policy_fingerprint(&policy))
    } else {
        None
    };

    let entry = registry.trusted.get(project_id);
    let trusted = match (entry, &external_policy_fingerprint) {
        (Some(entry), Some(fingerprint)) => {
            entry.trusted && &entry.policy_fingerprint == fingerprint
        }
        _ => false,
    };

    let (effective, reason) = if !external_policy_present {
        (TrustEffective::NotApplicable, TrustReason::NoExternalPolicy)
    } else if !config.security.allow_project_commands {
        (TrustEffective::Blocked, TrustReason::GlobalDisabled)
    } else if !trusted {
        let reason = if entry.is_some() {
            TrustReason::PolicyChanged
        } else {
            TrustReason::NotTrusted
        };
        (TrustEffective::Blocked, reason)
    } else {
        (TrustEffective::Allowed, TrustReason::Allowed)
    };

    TrustEvaluation {
        effective,
        reason,
        trusted,
        external_policy_present,
        external_policy_fingerprint,
    }
}

/// Authorize an action against an evaluation.
///
/// Built-in actions are always authorized; external actions fail closed with
/// [`TRUST_DENIED_CODE`] (exit 9) whenever the verdict is not `allowed`.
pub fn authorize(
    surface: ActionSurface,
    evaluation: &TrustEvaluation,
) -> Result<(), CarryCtxError> {
    match surface {
        ActionSurface::BuiltIn => Ok(()),
        ActionSurface::External if evaluation.is_allowed() => Ok(()),
        ActionSurface::External => Err(trust_denied(evaluation)),
    }
}

/// The typed error raised when an external action is blocked.
pub fn trust_denied(evaluation: &TrustEvaluation) -> CarryCtxError {
    let message = match evaluation.reason {
        TrustReason::GlobalDisabled => {
            "Repository-provided executable policy is disabled by the global security gate."
        }
        TrustReason::PolicyChanged => {
            "Repository-provided executable policy changed since it was trusted; re-grant required."
        }
        _ => "Repository-provided executable policy is not trusted for this project.",
    };
    CarryCtxError::new(TRUST_DENIED_CODE, message, ExitCode::PermissionScope).with_details(
        serde_json::json!({
            "effective": evaluation.effective.as_str(),
            "reason": evaluation.reason.as_str(),
            "trusted": evaluation.trusted,
            "external_policy_present": evaluation.external_policy_present,
            "external_policy_fingerprint": evaluation.external_policy_fingerprint,
        }),
    )
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;

    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::config::VerificationConfig;

    fn config_with(commands: &[&str], allow: bool) -> CarryCtxConfig {
        let mut config = CarryCtxConfig::default();
        config.verification = VerificationConfig {
            commands: commands.iter().map(|s| (*s).to_string()).collect(),
        };
        config.security.allow_project_commands = allow;
        config
    }

    fn entry(fingerprint: &str) -> TrustEntry {
        TrustEntry {
            trusted: true,
            project_name: "demo".into(),
            decided_at: "2026-09-11T00:00:00Z".into(),
            decided_by: "tester".into(),
            policy_fingerprint: fingerprint.into(),
        }
    }

    #[test]
    fn fingerprint_is_stable_and_order_sensitive() {
        let a = vec!["a".to_string(), "b".to_string()];
        let b = vec!["a".to_string(), "b".to_string()];
        let reordered = vec!["b".to_string(), "a".to_string()];
        assert_eq!(policy_fingerprint(&a), policy_fingerprint(&b));
        assert_ne!(policy_fingerprint(&a), policy_fingerprint(&reordered));
        assert!(policy_fingerprint(&a).starts_with("sha256:"));
    }

    #[test]
    fn built_in_actions_are_always_allowed() {
        let config = config_with(&["cargo test"], false);
        let evaluation = evaluate(&config, "pid", &TrustRegistry::default());
        assert_eq!(evaluation.effective, TrustEffective::Blocked);
        assert!(authorize(ActionSurface::BuiltIn, &evaluation).is_ok());
        let err = authorize(ActionSurface::External, &evaluation).unwrap_err();
        assert_eq!(err.code, TRUST_DENIED_CODE);
        assert_eq!(err.exit_code, ExitCode::PermissionScope);
    }

    #[test]
    fn no_external_policy_is_not_applicable() {
        let config = config_with(&[], false);
        let evaluation = evaluate(&config, "pid", &TrustRegistry::default());
        assert_eq!(evaluation.effective, TrustEffective::NotApplicable);
        assert_eq!(evaluation.reason, TrustReason::NoExternalPolicy);
        assert!(!evaluation.external_policy_present);
        assert!(evaluation.external_policy_fingerprint.is_none());
    }

    #[test]
    fn global_gate_disabled_blocks_even_when_trusted() {
        let config = config_with(&["cargo test"], false);
        let policy = collect_external_policy(&config);
        let mut registry = TrustRegistry::default();
        registry
            .trusted
            .insert("pid".into(), entry(&policy_fingerprint(&policy)));
        let evaluation = evaluate(&config, "pid", &registry);
        assert_eq!(evaluation.effective, TrustEffective::Blocked);
        assert_eq!(evaluation.reason, TrustReason::GlobalDisabled);
        assert!(evaluation.trusted, "registry trust survives for reporting");
    }

    #[test]
    fn untrusted_project_is_blocked_when_global_enabled() {
        let config = config_with(&["cargo test"], true);
        let evaluation = evaluate(&config, "pid", &TrustRegistry::default());
        assert_eq!(evaluation.effective, TrustEffective::Blocked);
        assert_eq!(evaluation.reason, TrustReason::NotTrusted);
    }

    #[test]
    fn policy_change_invalidates_trust() {
        let config_before = config_with(&["cargo test"], true);
        let policy = collect_external_policy(&config_before);
        let mut registry = TrustRegistry::default();
        registry
            .trusted
            .insert("pid".into(), entry(&policy_fingerprint(&policy)));

        let config_after = config_with(&["cargo test", "curl evil.example"], true);
        let evaluation = evaluate(&config_after, "pid", &registry);
        assert_eq!(evaluation.effective, TrustEffective::Blocked);
        assert_eq!(evaluation.reason, TrustReason::PolicyChanged);
        assert!(!evaluation.trusted);
    }

    #[test]
    fn all_conditions_hold_allows_external_action() {
        let config = config_with(&["cargo test"], true);
        let policy = collect_external_policy(&config);
        let mut registry = TrustRegistry::default();
        registry
            .trusted
            .insert("pid".into(), entry(&policy_fingerprint(&policy)));
        let evaluation = evaluate(&config, "pid", &registry);
        assert_eq!(evaluation.effective, TrustEffective::Allowed);
        assert_eq!(evaluation.reason, TrustReason::Allowed);
        assert!(authorize(ActionSurface::External, &evaluation).is_ok());
    }

    #[test]
    fn registry_round_trips_and_rejects_unknown_version_fields() {
        let registry: TrustRegistry = serde_json::from_str(
            r#"{"schema_version":1,"trusted":{"abc":{"trusted":true,"project_name":"p","decided_at":"t","decided_by":"a","policy_fingerprint":"sha256:x"}}}"#,
        )
        .unwrap();
        assert!(registry.trusted.contains_key("abc"));

        // Missing schema_version fails closed (deserialize error → malformed).
        assert!(serde_json::from_str::<TrustRegistry>(r#"{"trusted":{}}"#).is_err());
        // Unknown fields fail closed (closed shape).
        assert!(
            serde_json::from_str::<TrustRegistry>(r#"{"schema_version":1,"trusted":{},"extra":1}"#)
                .is_err()
        );
    }
}
