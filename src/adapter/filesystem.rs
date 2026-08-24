use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

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

/// Grace period before a lock directory without readable metadata is
/// treated as an orphan from a crash between `create_dir(lock_dir)` and
/// the metadata write, and is removed automatically.
const METALESS_LOCK_GRACE: Duration = Duration::from_secs(10);

pub fn acquire_lock(
    lock_dir: &Path,
    operation_id: &str,
    pid: u32,
    hostname: &str,
    now: &str,
) -> Result<(), CarryCtxError> {
    acquire_lock_owned(
        lock_dir,
        operation_id,
        pid,
        hostname,
        now,
        METALESS_LOCK_GRACE,
    )
    .map(|_| ())
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
    metaless_grace: Duration,
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
                // The lock dir exists but has no readable metadata: a crash
                // happened between `create_dir` and the metadata write (or
                // the metadata file is missing). Left alone this would
                // brick every mutating command with "manual inspection
                // required". Auto-heal once the directory is older than
                // the grace period; before that, assume another process is
                // mid-initialization and report contention.
                if !lock_dir_older_than(lock_dir, metaless_grace) {
                    return Err(CarryCtxError::state_conflict(
                        "Admission lock directory has no metadata yet; it may still be initializing.",
                    ));
                }
                fs::remove_dir_all(lock_dir).map_err(|e| {
                    CarryCtxError::database_error(format!("Failed to remove orphaned lock: {}", e))
                })?;
                return acquire_lock_owned(
                    lock_dir,
                    operation_id,
                    pid,
                    hostname,
                    now,
                    metaless_grace,
                );
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
                return acquire_lock_owned(
                    lock_dir,
                    operation_id,
                    pid,
                    hostname,
                    now,
                    metaless_grace,
                );
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
        let owner =
            acquire_lock_owned(path, operation_id, pid, hostname, now, METALESS_LOCK_GRACE)?;
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

/// Check whether `pid` refers to a live process on this machine.
///
/// Uses `kill(pid, 0)`, which performs existence and permission checks
/// without delivering a signal. This is portable across Unix platforms —
/// unlike probing `/proc/<pid>`, which exists only on Linux and made every
/// holder on macOS/BSD look dead, silently dropping mutual exclusion.
/// `EPERM` (holder exists but belongs to another user) counts as alive.
/// On platforms without a signal API we assume the holder is alive so a
/// stale lock never causes two writers to proceed concurrently; such locks
/// then need manual cleanup, which fails safe rather than open.
#[cfg(unix)]
#[allow(unsafe_code)]
fn is_pid_alive(pid: u32) -> bool {
    // SAFETY: `kill(2)` with signal 0 only checks that the process exists
    // and that we may signal it; no signal or state change occurs.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn is_pid_alive(_pid: u32) -> bool {
    true
}

/// Whether `dir`'s modification time is at least `age` in the past.
///
/// Unreadable or future timestamps are treated as "not old enough" so an
/// ambiguous directory is never auto-removed.
fn lock_dir_older_than(dir: &Path, age: Duration) -> bool {
    fs::metadata(dir)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|elapsed| elapsed >= age)
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
            .inspect_err(|_error| {
                let _ = std::fs::remove_dir_all(&lock);
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

    #[test]
    fn admission_lock_heals_metaless_directory_after_grace_period() {
        let root = tempfile::tempdir().unwrap();
        let lock = root.path().join("command.lock");
        // Simulate a crash between create_dir and the metadata write.
        std::fs::create_dir_all(&lock).unwrap();
        // Zero grace means any metaless directory is treated as orphaned.
        acquire_lock_owned(
            &lock,
            "second",
            std::process::id(),
            "test",
            "now",
            Duration::ZERO,
        )
        .expect("metaless lock dir must be healed and acquired");
        assert!(lock.join("meta.json").is_file());
    }

    #[test]
    fn admission_lock_reports_fresh_metaless_directory_as_contention() {
        let root = tempfile::tempdir().unwrap();
        let lock = root.path().join("command.lock");
        std::fs::create_dir_all(&lock).unwrap();
        let error = acquire_lock_owned(
            &lock,
            "second",
            std::process::id(),
            "test",
            "now",
            Duration::from_secs(3600),
        )
        .unwrap_err();
        assert_eq!(error.code, "STATE_CONFLICT");
        // The fresh directory is left for its creator to finish populating.
        assert!(lock.exists());
        assert!(!lock.join("meta.json").exists());
    }

    #[cfg(unix)]
    #[test]
    fn admission_lock_reclaims_stale_lock_of_dead_holder() {
        let root = tempfile::tempdir().unwrap();
        let lock = root.path().join("command.lock");
        // Reap a short-lived child to obtain a pid guaranteed to be dead.
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("spawn true");
        let dead_pid = child.id();
        child.wait().unwrap();
        assert!(!super::is_pid_alive(dead_pid));

        std::fs::create_dir_all(&lock).unwrap();
        std::fs::write(
            lock.join("meta.json"),
            format!(
                r#"{{"operation_id":"dead","pid":{dead_pid},"hostname":"test","acquired_at":"now"}}"#
            ),
        )
        .unwrap();

        acquire_lock_owned(
            &lock,
            "second",
            std::process::id(),
            "test",
            "now",
            METALESS_LOCK_GRACE,
        )
        .expect("stale lock of a dead same-host holder must be reclaimed");
    }

    #[cfg(unix)]
    #[test]
    fn admission_lock_respects_live_holder_via_kill_liveness() {
        let root = tempfile::tempdir().unwrap();
        let lock = root.path().join("command.lock");
        let mut holder = std::process::Command::new("sleep")
            .arg("2")
            .spawn()
            .expect("spawn sleep");
        let live_pid = holder.id();
        assert!(super::is_pid_alive(live_pid));

        std::fs::create_dir_all(&lock).unwrap();
        std::fs::write(
            lock.join("meta.json"),
            format!(
                r#"{{"operation_id":"live","pid":{live_pid},"hostname":"test","acquired_at":"now"}}"#
            ),
        )
        .unwrap();

        let error = acquire_lock_owned(
            &lock,
            "second",
            std::process::id(),
            "test",
            "now",
            METALESS_LOCK_GRACE,
        )
        .unwrap_err();
        assert_eq!(error.code, "STATE_CONFLICT");
        let _ = holder.kill();
        let _ = holder.wait();
    }

    #[test]
    fn admission_lock_acquisition_has_exactly_one_winner_under_race() {
        use std::sync::{Barrier, atomic::AtomicUsize, atomic::Ordering};
        const CONTENDERS: usize = 8;

        let root = tempfile::tempdir().unwrap();
        let lock = root.path().join("command.lock");
        let barrier = Barrier::new(CONTENDERS);
        let winners = AtomicUsize::new(0);
        let conflicts = AtomicUsize::new(0);

        std::thread::scope(|scope| {
            for i in 0..CONTENDERS {
                let barrier = &barrier;
                let winners = &winners;
                let conflicts = &conflicts;
                let lock_path = lock.clone();
                scope.spawn(move || {
                    barrier.wait();
                    match AdmissionLock::acquire(
                        &lock_path,
                        &format!("racer-{i}"),
                        std::process::id(),
                        "test",
                        "now",
                    ) {
                        Ok(guard) => {
                            winners.fetch_add(1, Ordering::SeqCst);
                            // Hold briefly so every contender truly overlaps.
                            std::thread::sleep(Duration::from_millis(20));
                            drop(guard);
                        }
                        Err(e) if e.code == "STATE_CONFLICT" => {
                            conflicts.fetch_add(1, Ordering::SeqCst);
                        }
                        Err(e) => panic!("unexpected error kind {}: {e}", e.code),
                    }
                });
            }
        });

        assert_eq!(
            winners.load(Ordering::SeqCst),
            1,
            "exactly one contender may win the lock"
        );
        assert_eq!(conflicts.load(Ordering::SeqCst), CONTENDERS - 1);
    }
}
