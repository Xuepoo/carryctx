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
    ensure_dir(lock_dir.parent().unwrap_or(Path::new(".")))?;

    match fs::create_dir(lock_dir) {
        Ok(()) => {
            let meta = serde_json::json!({
                "operation_id": operation_id,
                "pid": pid,
                "hostname": hostname,
                "acquired_at": now,
            });
            let meta_path = lock_dir.join("meta.json");
            write_atomic(&meta_path, &serde_json::to_vec(&meta).unwrap_or_default())?;
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let meta_path = lock_dir.join("meta.json");
            if meta_path.exists() {
                let meta_str = read_to_string(&meta_path)?;
                if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&meta_str) {
                    if let Some(stored_pid) = meta["pid"].as_u64() {
                        if !is_pid_alive(stored_pid as u32) {
                            fs::remove_dir_all(lock_dir).map_err(|e| {
                                CarryCtxError::database_error(format!(
                                    "Failed to remove stale lock: {}",
                                    e
                                ))
                            })?;
                            return acquire_lock(lock_dir, operation_id, pid, hostname, now);
                        }
                    }
                }
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

pub fn release_lock(lock_dir: &Path) -> Result<(), CarryCtxError> {
    if lock_dir.exists() {
        fs::remove_dir_all(lock_dir)
            .map_err(|e| CarryCtxError::database_error(format!("Failed to release lock: {}", e)))?;
    }
    Ok(())
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

pub fn write_journal(journal_dir: &Path, entry: &JournalEntry) -> Result<(), CarryCtxError> {
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
    let path = journal_dir.join(format!("{}.json", operation_id));
    if !path.exists() {
        return Ok(None);
    }
    let content = read_to_string(&path)?;
    let entry: JournalEntry = serde_json::from_str(&content)
        .map_err(|e| CarryCtxError::database_error(format!("Invalid journal entry: {}", e)))?;
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
    let path = journal_dir.join(format!("{}.json", operation_id));
    remove_if_exists(&path)
}
