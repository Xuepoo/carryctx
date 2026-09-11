//! Trust projection helpers (CTX-0100).
//!
//! Pure assembly of the `carryctx trust list` / `carryctx trust status`
//! payloads from the domain verdict and a fail-closed registry load. IO,
//! identity resolution, and audit events stay in the command layer.

use std::path::PathBuf;

use crate::adapter::trust_store::{RegistryLoad, RegistryState, TRUST_REGISTRY_FILE};
use crate::adapter::xdg::XdgPaths;
use crate::domain::config::CarryCtxConfig;
use crate::domain::trust::{TrustEvaluation, collect_external_policy, evaluate};

/// The registry path under `$XDG_STATE_HOME/carryctx/`.
pub fn registry_path(xdg: &XdgPaths) -> PathBuf {
    xdg.state_home.join("carryctx").join(TRUST_REGISTRY_FILE)
}

/// The trust verdict plus the decision metadata for the current project.
pub struct TrustStatus {
    pub project_id: String,
    pub project_name: String,
    pub evaluation: TrustEvaluation,
    pub global_allow_project_commands: bool,
    pub decided_at: Option<String>,
    pub decided_by: Option<String>,
    pub command_count: usize,
    pub registry: RegistryLoad,
}

/// Evaluate `project_id` against its declared policy and the loaded registry.
pub fn evaluate_project(
    config: &CarryCtxConfig,
    project_id: &str,
    project_name: &str,
    registry: RegistryLoad,
) -> TrustStatus {
    let evaluation = evaluate(config, project_id, &registry.registry);
    let command_count = collect_external_policy(config).len();
    let entry = registry.registry.trusted.get(project_id);
    TrustStatus {
        project_id: project_id.to_string(),
        project_name: project_name.to_string(),
        evaluation,
        global_allow_project_commands: config.security.allow_project_commands,
        decided_at: entry.map(|entry| entry.decided_at.clone()),
        decided_by: entry.map(|entry| entry.decided_by.clone()),
        command_count,
        registry,
    }
}

impl TrustStatus {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "project_id": self.project_id,
            "project_name": self.project_name,
            "trusted": self.evaluation.trusted,
            "decided_at": self.decided_at,
            "decided_by": self.decided_by,
            "global_allow_project_commands": self.global_allow_project_commands,
            "external_policy_present": self.evaluation.external_policy_present,
            "external_policy_fingerprint": self.evaluation.external_policy_fingerprint,
            "command_count": self.command_count,
            "effective": self.evaluation.effective.as_str(),
            "reason": self.evaluation.reason.as_str(),
            "registry_state": self.registry.state.as_str(),
            "registry_path": self.registry.path.to_string_lossy(),
        })
    }
}

/// Build the `trust list` payload. Never consults the current project.
pub fn list_payload(load: &RegistryLoad) -> serde_json::Value {
    let projects: Vec<serde_json::Value> = load
        .registry
        .trusted
        .iter()
        .map(|(project_id, entry)| {
            serde_json::json!({
                "project_id": project_id,
                "project_name": entry.project_name,
                "trusted": entry.trusted,
                "decided_at": entry.decided_at,
                "decided_by": entry.decided_by,
                "policy_fingerprint": entry.policy_fingerprint,
            })
        })
        .collect();
    serde_json::json!({
        "registry_path": load.path.to_string_lossy(),
        "registry_state": load.state.as_str(),
        "schema_version": load.registry.schema_version,
        "detail": load.detail,
        "projects": projects,
    })
}

/// Non-empty warning when the registry could not be trusted, so list/status
/// surfaces explain why every project is treated as untrusted.
pub fn registry_warning(load: &RegistryLoad) -> Option<String> {
    if load.state == RegistryState::Ok || load.state == RegistryState::Absent {
        return None;
    }
    Some(format!(
        "Trust registry {} is {}; it is treated as empty and no project is trusted ({})",
        load.path.display(),
        load.state.as_str(),
        load.detail.as_deref().unwrap_or("no detail")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::trust_store::TrustStore;
    use crate::domain::config::VerificationConfig;
    use crate::domain::trust::{TrustEntry, TrustRegistry, policy_fingerprint};

    fn config_with(commands: &[&str], allow: bool) -> CarryCtxConfig {
        let mut config = CarryCtxConfig::default();
        config.verification = VerificationConfig {
            commands: commands.iter().map(|s| (*s).to_string()).collect(),
        };
        config.security.allow_project_commands = allow;
        config
    }

    fn load_with(registry: TrustRegistry) -> RegistryLoad {
        RegistryLoad {
            path: PathBuf::from("/tmp/trusted-projects.json"),
            state: RegistryState::Ok,
            registry,
            detail: None,
        }
    }

    #[test]
    fn status_reports_blocked_for_untrusted_external_policy() {
        let load = load_with(TrustRegistry::default());
        let status = evaluate_project(&config_with(&["cargo test"], true), "pid", "demo", load);
        let json = status.to_json();
        assert_eq!(json["effective"], "blocked");
        assert_eq!(json["reason"], "not_trusted");
        assert_eq!(json["external_policy_present"], true);
        assert_eq!(json["command_count"], 1);
        assert!(
            json["external_policy_fingerprint"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );
    }

    #[test]
    fn status_reports_allowed_when_trusted_and_enabled() {
        let policy = vec!["cargo test".to_string()];
        let mut registry = TrustRegistry::default();
        registry.trusted.insert(
            "pid".into(),
            TrustEntry {
                trusted: true,
                project_name: "demo".into(),
                decided_at: "2026-09-11T00:00:00Z".into(),
                decided_by: "tester".into(),
                policy_fingerprint: policy_fingerprint(&policy),
            },
        );
        let status = evaluate_project(
            &config_with(&["cargo test"], true),
            "pid",
            "demo",
            load_with(registry),
        );
        let json = status.to_json();
        assert_eq!(json["effective"], "allowed");
        assert_eq!(json["reason"], "allowed");
        assert_eq!(json["decided_by"], "tester");
    }

    #[test]
    fn status_reports_not_applicable_without_policy() {
        let status = evaluate_project(
            &config_with(&[], false),
            "pid",
            "demo",
            load_with(TrustRegistry::default()),
        );
        assert_eq!(status.to_json()["effective"], "not_applicable");
    }

    #[test]
    fn list_payload_includes_entries_sorted_by_id() {
        let mut registry = TrustRegistry::default();
        for id in ["b", "a"] {
            registry.trusted.insert(
                id.into(),
                TrustEntry {
                    trusted: true,
                    project_name: id.into(),
                    decided_at: "t".into(),
                    decided_by: "d".into(),
                    policy_fingerprint: "sha256:x".into(),
                },
            );
        }
        let payload = list_payload(&load_with(registry));
        let ids: Vec<&str> = payload["projects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["project_id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn registry_warning_is_emitted_only_for_untrusted_states() {
        let dir = tempfile::tempdir().unwrap();
        let absent = TrustStore::new(dir.path().join("nope.json")).load();
        assert!(registry_warning(&absent).is_none());
    }
}
