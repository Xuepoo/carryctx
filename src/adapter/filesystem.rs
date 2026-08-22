use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::CarryCtxError;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

pub fn write_atomic(path: &Path, contents: &[u8]) -> Result<(), CarryCtxError> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let tmp_name = format!(
        ".{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    );
    let tmp_path = dir.join(&tmp_name);

    let mut open_opts = fs::OpenOptions::new();
    open_opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        open_opts.mode(0o600);
    }
    let mut file = open_opts
        .open(&tmp_path)
        .map_err(|e| CarryCtxError::database_error(format!("Failed to create temp file: {}", e)))?;

    file.write_all(contents)
        .map_err(|e| CarryCtxError::database_error(format!("Failed to write temp file: {}", e)))?;
    file.sync_all()
        .map_err(|e| CarryCtxError::database_error(format!("Failed to sync temp file: {}", e)))?;

    fs::rename(&tmp_path, path)
        .map_err(|e| CarryCtxError::database_error(format!("Failed to rename temp file: {}", e)))?;

    #[cfg(unix)]
    {
        if let Ok(dir_file) = fs::File::open(dir) {
            let _ = dir_file.sync_all();
        }
    }

    Ok(())
}

pub fn read_to_string(path: &Path) -> Result<String, CarryCtxError> {
    fs::read_to_string(path)
        .map_err(|e| CarryCtxError::resource_not_found(format!("Failed to read file: {}", e)))
}

pub fn ensure_dir(path: &Path) -> Result<(), CarryCtxError> {
    fs::create_dir_all(path)
        .map_err(|e| CarryCtxError::database_error(format!("Failed to create directory: {}", e)))
}

pub fn remove_if_exists(path: &Path) -> Result<(), CarryCtxError> {
    if path.exists() {
        fs::remove_file(path)
            .map_err(|e| CarryCtxError::database_error(format!("Failed to remove file: {}", e)))?;
    }
    Ok(())
}

// --- Admission Lock ---

pub fn acquire_lock(
    lock_dir: &Path,
    operation_id: &str,
    pid: u32,
    hostname: &str,
    now: &str,
) -> Result<(), CarryCtxError> {
    acquire_lock_owned(lock_dir, operation_id, pid, hostname, now).map(|_| ())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LockOwner {
    owner_token: String,
    operation_id: String,
    pid: u32,
    hostname: String,
}

fn acquire_lock_owned(
    lock_dir: &Path,
    operation_id: &str,
    pid: u32,
    hostname: &str,
    now: &str,
) -> Result<LockOwner, CarryCtxError> {
    ensure_dir(lock_dir.parent().unwrap_or(Path::new(".")))?;
    let owner = LockOwner {
        owner_token: ulid::Ulid::generate().to_string(),
        operation_id: operation_id.to_string(),
        pid,
        hostname: hostname.to_string(),
    };

    match fs::create_dir(lock_dir) {
        Ok(()) => {
            if let Err(error) = write_lock_metadata(lock_dir, &owner, now) {
                let _ = fs::remove_dir_all(lock_dir);
                return Err(error);
            }
            Ok(owner)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let meta_path = lock_dir.join("meta.json");
            if !meta_path.is_file() {
                return Err(CarryCtxError::state_conflict(
                    "Admission lock metadata is missing or malformed; manual inspection is required.",
                ));
            }
            let meta_str = read_to_string(&meta_path)?;
            let meta = serde_json::from_str::<serde_json::Value>(&meta_str).map_err(|_| {
                CarryCtxError::state_conflict(
                    "Admission lock metadata is malformed; manual inspection is required.",
                )
            })?;
            let stored_hostname = meta["hostname"].as_str().ok_or_else(|| {
                CarryCtxError::state_conflict("Admission lock metadata has no valid hostname.")
            })?;
            let stored_pid = meta["pid"].as_u64().ok_or_else(|| {
                CarryCtxError::state_conflict("Admission lock metadata has no valid process id.")
            })? as u32;
            if stored_hostname == hostname && !is_pid_alive(stored_pid) {
                fs::remove_dir_all(lock_dir).map_err(|e| {
                    CarryCtxError::database_error(format!("Failed to remove stale lock: {}", e))
                })?;
                return acquire_lock_owned(lock_dir, operation_id, pid, hostname, now);
            }
            Err(CarryCtxError::state_conflict(
                "Admission lock held by another process.",
            ))
        }
        Err(e) => Err(CarryCtxError::database_error(format!(
            "Failed to acquire lock: {}",
            e
        ))),
    }
}

fn write_lock_metadata(lock_dir: &Path, owner: &LockOwner, now: &str) -> Result<(), CarryCtxError> {
    let meta = serde_json::json!({
        "owner_token": owner.owner_token,
        "operation_id": owner.operation_id,
        "pid": owner.pid,
        "hostname": owner.hostname,
        "acquired_at": now,
    });
    let meta_path = lock_dir.join("meta.json");
    write_atomic(&meta_path, &serde_json::to_vec(&meta).unwrap_or_default())
}

pub struct AdmissionLock {
    path: PathBuf,
    owner: LockOwner,
}

impl AdmissionLock {
    pub fn acquire(
        path: &Path,
        operation_id: &str,
        pid: u32,
        hostname: &str,
        now: &str,
    ) -> Result<Self, CarryCtxError> {
        let owner = acquire_lock_owned(path, operation_id, pid, hostname, now)?;
        Ok(Self {
            path: path.to_path_buf(),
            owner,
        })
    }
}

impl Drop for AdmissionLock {
    fn drop(&mut self) {
        let _ = release_lock(&self.path, &self.owner);
    }
}

fn release_lock(lock_dir: &Path, owner: &LockOwner) -> Result<(), CarryCtxError> {
    let meta_path = lock_dir.join("meta.json");
    if lock_dir.exists() && lock_owner_matches(&meta_path, owner) {
        fs::remove_dir_all(lock_dir)
            .map_err(|e| CarryCtxError::database_error(format!("Failed to release lock: {}", e)))?;
    }
    Ok(())
}

fn lock_owner_matches(meta_path: &Path, owner: &LockOwner) -> bool {
    let Ok(content) = fs::read_to_string(meta_path) else {
        return false;
    };
    let Ok(meta) = serde_json::from_str::<serde_json::Value>(&content) else {
        return false;
    };
    meta["owner_token"].as_str() == Some(&owner.owner_token)
        && meta["operation_id"].as_str() == Some(&owner.operation_id)
        && meta["hostname"].as_str() == Some(&owner.hostname)
        && meta["pid"].as_u64() == Some(owner.pid as u64)
}

fn is_pid_alive(pid: u32) -> bool {
    PathBuf::from(format!("/proc/{}", pid)).exists()
}

// --- Operation Journal ---

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct JournalEntry {
    pub operation_id: String,
    pub kind: String,
    pub status: String,
    pub created_at: String,
    pub metadata: serde_json::Value,
}

fn validate_operation_id(operation_id: &str) -> Result<(), CarryCtxError> {
    let parsed = ulid::Ulid::from_string(operation_id).map_err(|_| {
        CarryCtxError::database_error("Journal operation ID is malformed or unsafe.")
    })?;
    if parsed.to_string() != operation_id {
        return Err(CarryCtxError::database_error(
            "Journal operation ID is malformed or unsafe.",
        ));
    }
    Ok(())
}

pub fn write_journal(journal_dir: &Path, entry: &JournalEntry) -> Result<(), CarryCtxError> {
    validate_operation_id(&entry.operation_id)?;
    ensure_dir(journal_dir)?;
    let path = journal_dir.join(format!("{}.json", entry.operation_id));
    let json = serde_json::to_vec_pretty(entry).map_err(|e| {
        CarryCtxError::database_error(format!("Failed to serialize journal: {}", e))
    })?;
    write_atomic(&path, &json)
}

pub fn read_journal(
    journal_dir: &Path,
    operation_id: &str,
) -> Result<Option<JournalEntry>, CarryCtxError> {
    validate_operation_id(operation_id)?;
    let path = journal_dir.join(format!("{}.json", operation_id));
    if !path.exists() {
        return Ok(None);
    }
    let content = read_to_string(&path)?;
    let entry: JournalEntry = serde_json::from_str(&content)
        .map_err(|e| CarryCtxError::database_error(format!("Invalid journal entry: {}", e)))?;
    validate_operation_id(&entry.operation_id)?;
    if entry.operation_id != operation_id {
        return Err(CarryCtxError::database_error(
            "Journal operation ID does not match its filename.",
        ));
    }
    Ok(Some(entry))
}

pub fn list_journals(journal_dir: &Path) -> Result<Vec<JournalEntry>, CarryCtxError> {
    if !journal_dir.exists() {
        return Ok(vec![]);
    }
    let mut entries = Vec::new();
    let mut dir = fs::read_dir(journal_dir)
        .map_err(|e| CarryCtxError::database_error(format!("Failed to read journal dir: {}", e)))?;
    while let Some(Ok(entry)) = dir.next() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "json") {
            if let Some(entry) = read_journal(
                journal_dir,
                &path.file_stem().unwrap_or_default().to_string_lossy(),
            )? {
                entries.push(entry);
            }
        }
    }
    Ok(entries)
}

pub fn remove_journal(journal_dir: &Path, operation_id: &str) -> Result<(), CarryCtxError> {
    validate_operation_id(operation_id)?;
    let path = journal_dir.join(format!("{}.json", operation_id));
    remove_if_exists(&path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_operation_id_requires_canonical_ulid() {
        let valid = ulid::Ulid::generate().to_string();
        assert!(validate_operation_id(&valid).is_ok());
        for invalid in ["", "..", "../outside", "/tmp/outside", "not-an-id"] {
            assert!(
                validate_operation_id(invalid).is_err(),
                "accepted {invalid:?}"
            );
        }
    }

    #[test]
    fn admission_lock_rejects_contention_and_releases_on_drop() {
        let root = tempfile::tempdir().unwrap();
        let lock = root.path().join("command.lock");
        let first =
            AdmissionLock::acquire(&lock, "first", std::process::id(), "test", "now").unwrap();
        let error = acquire_lock(&lock, "second", std::process::id(), "test", "now").unwrap_err();
        assert_eq!(error.code, "STATE_CONFLICT");
        drop(first);
        assert!(!lock.exists());
    }

    #[test]
    fn admission_lock_removes_directory_when_metadata_write_fails() {
        let root = tempfile::tempdir().unwrap();
        let lock = root.path().join("command.lock");
        std::fs::create_dir_all(&lock).unwrap();
        std::fs::create_dir_all(lock.join("meta.json")).unwrap();
        let owner = LockOwner {
            owner_token: "token".into(),
            operation_id: "first".into(),
            pid: std::process::id(),
            hostname: "test".into(),
        };
        let error = write_lock_metadata(&lock, &owner, "now")
            .map_err(|error| {
                let _ = std::fs::remove_dir_all(&lock);
                error
            })
            .unwrap_err();
        assert_eq!(error.code, "DATABASE_ERROR");
        assert!(!lock.exists());
    }

    #[test]
    fn admission_lock_treats_malformed_metadata_as_conflict() {
        let root = tempfile::tempdir().unwrap();
        let lock = root.path().join("command.lock");
        std::fs::create_dir_all(&lock).unwrap();
        std::fs::write(lock.join("meta.json"), b"not-json").unwrap();

        let error = acquire_lock(&lock, "second", std::process::id(), "test", "now").unwrap_err();
        assert_eq!(error.code, "STATE_CONFLICT");
        assert!(lock.exists());
    }

    #[test]
    fn admission_lock_does_not_reclaim_lock_from_another_host() {
        let root = tempfile::tempdir().unwrap();
        let lock = root.path().join("command.lock");
        std::fs::create_dir_all(&lock).unwrap();
        std::fs::write(
            lock.join("meta.json"),
            format!(
                r#"{{"operation_id":"remote","pid":{},"hostname":"other-host","acquired_at":"now"}}"#,
                std::process::id()
            ),
        )
        .unwrap();

        let error =
            acquire_lock(&lock, "second", std::process::id(), "local-host", "now").unwrap_err();
        assert_eq!(error.code, "STATE_CONFLICT");
        assert!(lock.exists());
    }

    #[test]
    fn admission_lock_does_not_remove_a_replacement_lock_on_drop() {
        let root = tempfile::tempdir().unwrap();
        let lock = root.path().join("command.lock");
        let first = AdmissionLock::acquire(&lock, "first", 1, "host", "now").unwrap();
        std::fs::remove_dir_all(&lock).unwrap();
        let second = AdmissionLock::acquire(&lock, "second", 2, "host", "later").unwrap();

        drop(first);
        assert!(lock.exists());
        drop(second);
        assert!(!lock.exists());
    }
}
