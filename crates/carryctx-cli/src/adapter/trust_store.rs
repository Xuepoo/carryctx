//! On-disk trust registry adapter (CTX-0100).
//!
//! The registry is user-local state at
//! `${XDG_STATE_HOME:-$HOME/.local/state}/carryctx/trusted-projects.json`.
//! Loading is fail-closed: an absent, malformed, unknown-version, unreadable,
//! or group/other-accessible file yields an empty registry and a
//! machine-readable state, never a partially trusted one. Writing is atomic
//! and forces `0600` on Unix.

use std::fs;
use std::path::{Path, PathBuf};

use crate::adapter::filesystem::write_atomic;
use crate::domain::trust::{TRUST_REGISTRY_SCHEMA_VERSION, TrustRegistry};
use crate::error::CarryCtxError;

/// The trust registry's relative path under `$XDG_STATE_HOME/carryctx/`.
pub const TRUST_REGISTRY_FILE: &str = "trusted-projects.json";

/// How the registry file was loaded. Only [`RegistryState::Ok`] and
/// [`RegistryState::Absent`] are safe to extend; the others must never be
/// silently repaired or overwritten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryState {
    Absent,
    Ok,
    Malformed,
    Unreadable,
    Insecure,
}

impl RegistryState {
    pub fn as_str(&self) -> &'static str {
        match self {
            RegistryState::Absent => "absent",
            RegistryState::Ok => "ok",
            RegistryState::Malformed => "malformed",
            RegistryState::Unreadable => "unreadable",
            RegistryState::Insecure => "insecure",
        }
    }

    /// Whether the state is safe to write to. Unknown state is never
    /// overwritten (fail closed, no data destruction).
    pub fn is_writable(&self) -> bool {
        matches!(self, RegistryState::Absent | RegistryState::Ok)
    }
}

/// Result of a fail-closed registry load.
#[derive(Debug, Clone)]
pub struct RegistryLoad {
    pub path: PathBuf,
    pub state: RegistryState,
    pub registry: TrustRegistry,
    /// Human-readable cause for non-`Ok` states. Never contains registry
    /// values beyond the file path and a parse error.
    pub detail: Option<String>,
}

/// Filesystem-backed trust registry.
pub struct TrustStore {
    path: PathBuf,
}

impl TrustStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load the registry fail-closed. Never returns `Err`: a caller must be
    /// able to evaluate trust even when the file is unreadable, and the
    /// verdict for every non-`Ok` state is "not trusted".
    pub fn load(&self) -> RegistryLoad {
        if !self.path.exists() {
            return RegistryLoad {
                path: self.path.clone(),
                state: RegistryState::Absent,
                registry: TrustRegistry::default(),
                detail: None,
            };
        }

        match registry_is_insecure(&self.path) {
            Ok(true) => {
                return RegistryLoad {
                    path: self.path.clone(),
                    state: RegistryState::Insecure,
                    registry: TrustRegistry::default(),
                    detail: Some(format!(
                        "{} has group/other permission bits set; fix with chmod 600",
                        self.path.display()
                    )),
                };
            }
            Ok(false) => {}
            Err(error) => {
                return RegistryLoad {
                    path: self.path.clone(),
                    state: RegistryState::Unreadable,
                    registry: TrustRegistry::default(),
                    detail: Some(error.to_string()),
                };
            }
        }

        let raw = match fs::read_to_string(&self.path) {
            Ok(raw) => raw,
            Err(error) => {
                return RegistryLoad {
                    path: self.path.clone(),
                    state: RegistryState::Unreadable,
                    registry: TrustRegistry::default(),
                    detail: Some(error.to_string()),
                };
            }
        };

        match serde_json::from_str::<TrustRegistry>(&raw) {
            Ok(registry) if registry.schema_version != TRUST_REGISTRY_SCHEMA_VERSION => {
                RegistryLoad {
                    path: self.path.clone(),
                    state: RegistryState::Malformed,
                    registry: TrustRegistry::default(),
                    detail: Some(format!(
                        "unsupported trust registry schema_version {} (expected {})",
                        registry.schema_version, TRUST_REGISTRY_SCHEMA_VERSION
                    )),
                }
            }
            Ok(registry) => RegistryLoad {
                path: self.path.clone(),
                state: RegistryState::Ok,
                registry,
                detail: None,
            },
            Err(error) => RegistryLoad {
                path: self.path.clone(),
                state: RegistryState::Malformed,
                registry: TrustRegistry::default(),
                detail: Some(error.to_string()),
            },
        }
    }

    /// Atomically replace the registry with `registry`, creating the parent
    /// directory `0700` and the file `0600` on Unix. Callers must only invoke
    /// this when [`RegistryState::is_writable`] is true for the current file.
    pub fn save(&self, registry: &TrustRegistry) -> Result<(), CarryCtxError> {
        ensure_parent_dir(&self.path)?;
        let mut json = serde_json::to_string_pretty(registry).map_err(|error| {
            CarryCtxError::io_error(format!("Failed to serialize trust registry: {error}"))
        })?;
        json.push('\n');
        write_atomic(&self.path, json.as_bytes()).map_err(|error| {
            CarryCtxError::io_error(format!(
                "Failed to write trust registry {}: {}",
                self.path.display(),
                error.message
            ))
        })
    }
}

fn ensure_parent_dir(path: &Path) -> Result<(), CarryCtxError> {
    if let Some(parent) = path.parent() {
        let existed = parent.exists();
        fs::create_dir_all(parent).map_err(|error| {
            CarryCtxError::io_error(format!(
                "Failed to create trust registry directory {}: {error}",
                parent.display()
            ))
        })?;
        if !existed {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn registry_is_insecure(path: &Path) -> std::io::Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(path)?.permissions().mode();
    Ok(mode & 0o077 != 0)
}

#[cfg(not(unix))]
fn registry_is_insecure(_path: &Path) -> std::io::Result<bool> {
    // Permission bits are a Unix control; Windows ACLs are a documented gap
    // (design §12 open question 4).
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::trust::TrustEntry;

    fn store_in(dir: &Path) -> TrustStore {
        TrustStore::new(dir.join("carryctx").join(TRUST_REGISTRY_FILE))
    }

    fn write_private(path: &Path, contents: &str) {
        fs::write(path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    fn sample_registry() -> TrustRegistry {
        let mut registry = TrustRegistry::default();
        registry.trusted.insert(
            "pid".into(),
            TrustEntry {
                trusted: true,
                project_name: "demo".into(),
                decided_at: "2026-09-11T00:00:00Z".into(),
                decided_by: "tester".into(),
                policy_fingerprint: "sha256:abc".into(),
            },
        );
        registry
    }

    #[test]
    fn absent_registry_is_fail_closed_and_writable() {
        let dir = tempfile::tempdir().unwrap();
        let load = store_in(dir.path()).load();
        assert_eq!(load.state, RegistryState::Absent);
        assert!(load.registry.trusted.is_empty());
        assert!(load.state.is_writable());
    }

    #[test]
    fn save_round_trips_and_creates_0600_on_unix() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        store.save(&sample_registry()).unwrap();

        let load = store.load();
        assert_eq!(load.state, RegistryState::Ok);
        assert!(load.registry.trusted.contains_key("pid"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(store.path()).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "registry file must be 0600");
            let dir_mode = fs::metadata(store.path().parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(dir_mode, 0o700, "registry directory must be 0700");
        }
    }

    #[test]
    fn malformed_registry_is_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        write_private(store.path(), "{ not json");

        let load = store.load();
        assert_eq!(load.state, RegistryState::Malformed);
        assert!(load.registry.trusted.is_empty());
        assert!(!load.state.is_writable());
    }

    #[test]
    fn unknown_schema_version_is_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        write_private(store.path(), r#"{"schema_version":999,"trusted":{}}"#);

        let load = store.load();
        assert_eq!(load.state, RegistryState::Malformed);
        assert!(!load.state.is_writable());
    }

    #[cfg(unix)]
    #[test]
    fn group_readable_registry_is_fail_closed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        store.save(&sample_registry()).unwrap();
        fs::set_permissions(store.path(), fs::Permissions::from_mode(0o644)).unwrap();

        let load = store.load();
        assert_eq!(load.state, RegistryState::Insecure);
        assert!(load.registry.trusted.is_empty());
        assert!(!load.state.is_writable());
    }
}
