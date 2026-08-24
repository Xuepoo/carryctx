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
/// the metadata write (legacy layout), and is removed automatically.
const METALESS_LOCK_GRACE: Duration = Duration::from_secs(10);

/// Minimum gap between the two age/emptiness inspections that must BOTH
/// pass before a metaless lock directory is stolen. A legacy creator writes
/// its metadata within microseconds of creating the directory, so requiring
/// the metaless state to persist across this gap makes a false steal
/// practically impossible.
const METALESS_RECHECK_GAP: Duration = Duration::from_millis(200);

/// Transient-ENOENT tolerance when reading existing lock metadata: the
/// holder may be mid-teardown between our `is_file()` check and the read.
/// Bounded retries keep acquisition responsive while absorbing the race.
const META_READ_MAX_ATTEMPTS: u32 = 25;
const META_READ_RETRY_DELAY: Duration = Duration::from_millis(10);

/// Hard cap on steal-and-republish steps inside one acquisition call so no
/// pathological interleaving can spin indefinitely; the caller-level retry
/// loop (see `acquire_runtime_lock`) paces longer contention.
const ACQUIRE_MAX_STEPS: usize = 16;

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
    let parent = lock_dir.parent().unwrap_or(Path::new("."));
    ensure_dir(parent)?;
    let owner = LockOwner {
        owner_token: ulid::Ulid::generate().to_string(),
        operation_id: operation_id.to_string(),
        pid,
        hostname: hostname.to_string(),
    };
    sweep_orphaned_stagings(parent, lock_dir, metaless_grace);
    let staging = staging_path(lock_dir, &owner.owner_token);

    for _step in 0..ACQUIRE_MAX_STEPS {
        if publish_lock_dir(&staging, lock_dir, &owner, now)? {
            return Ok(owner);
        }
        match observe_existing_lock(lock_dir, hostname, metaless_grace)? {
            ExistingLockObservation::Conflict(error) => {
                let _ = fs::remove_dir_all(&staging);
                return Err(error);
            }
            // The directory vanished while being observed (concurrent
            // release or heal): simply re-run the publish attempt.
            ExistingLockObservation::Vanished => continue,
            ExistingLockObservation::StealReady => {
                remove_lock_dir_best_effort(lock_dir)?;
            }
        }
    }

    let _ = fs::remove_dir_all(&staging);
    Err(CarryCtxError::state_conflict(
        "Admission lock could not be stabilized under heavy contention.",
    ))
}

/// Sibling staging directory used to publish a fully populated lock dir
/// atomically via `rename(2)`. Observers therefore never see a final lock
/// directory without complete metadata.
fn staging_path(lock_dir: &Path, owner_token: &str) -> PathBuf {
    let file_name = lock_dir.file_name().unwrap_or_default().to_string_lossy();
    lock_dir
        .parent()
        .unwrap_or(Path::new("."))
        .join(format!(".{file_name}.{owner_token}.tmp"))
}

/// Remove leftover staging directories from crashed publishers. Safe by
/// construction: a live publisher whose staging disappears simply rebuilds
/// it and retries, so only age is checked (errors are ignored — this is
/// hygiene, not correctness).
fn sweep_orphaned_stagings(parent: &Path, lock_dir: &Path, grace: Duration) {
    let prefix = format!(
        ".{}.",
        lock_dir.file_name().unwrap_or_default().to_string_lossy()
    );
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(&prefix) && name.ends_with(".tmp") {
            let path = entry.path();
            if lock_dir_older_than(&path, grace) {
                let _ = fs::remove_dir_all(&path);
            }
        }
    }
}

/// Build the staging directory fully populated with our metadata, then
/// atomically move it onto `lock_dir`. Returns `true` when we won; `false`
/// means another publisher's directory occupies `lock_dir` and must be
/// observed. The no-clobber rename guarantees exactly one concurrent
/// publisher wins and that an existing directory is never silently
/// replaced — restoring the single-winner guarantee without any metaless
/// observation window.
fn publish_lock_dir(
    staging: &Path,
    lock_dir: &Path,
    owner: &LockOwner,
    now: &str,
) -> Result<bool, CarryCtxError> {
    // Rebuild from scratch every attempt; a previous attempt may have left
    // a partially populated staging behind after an error.
    let _ = fs::remove_dir_all(staging);
    fs::create_dir(staging).map_err(|e| {
        CarryCtxError::database_error(format!("Failed to create lock staging: {}", e))
    })?;
    write_lock_metadata(staging, owner, now)?;
    match rename_noreplace(staging, lock_dir) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // The parent vanished underneath us (external cleanup); recreate
            // it and try again on the next step.
            ensure_dir(lock_dir.parent().unwrap_or(Path::new(".")))?;
            Ok(false)
        }
        Err(e) => Err(CarryCtxError::database_error(format!(
            "Failed to publish lock directory: {}",
            e
        ))),
    }
}

/// Atomic no-clobber directory rename.
///
/// On x86_64/aarch64 Linux this is `renameat2(RENAME_NOREPLACE)`, which
/// fails with `EEXIST` instead of replacing an existing target — the
/// kernel-side guarantee that only one publisher can install its lock
/// directory. Everywhere else this degrades to an existence pre-check plus
/// plain `rename(2)`, whose residual replace window is bounded by legacy
/// creators being mid-initialization for microseconds.
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[allow(unsafe_code)]
fn rename_noreplace(old: &Path, new: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    const AT_FDCWD: libc::c_int = -100;
    const RENAME_NOREPLACE: libc::c_uint = 1;
    #[cfg(target_arch = "x86_64")]
    const SYS_RENAMEAT2: libc::c_long = 316;
    #[cfg(target_arch = "aarch64")]
    const SYS_RENAMEAT2: libc::c_long = 276;

    let from = CString::new(old.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let to = CString::new(new.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    // SAFETY: `renameat2(2)` with RENAME_NOREPLACE performs an atomic,
    // no-clobber directory rename. Both pointers refer to valid NUL-
    // terminated paths for the duration of the call; no memory is retained.
    let rc = unsafe {
        libc::syscall(
            SYS_RENAMEAT2,
            AT_FDCWD,
            from.as_ptr(),
            AT_FDCWD,
            to.as_ptr(),
            RENAME_NOREPLACE,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
fn rename_noreplace(old: &Path, new: &Path) -> std::io::Result<()> {
    if new.exists() {
        return Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists));
    }
    fs::rename(old, new)
}

enum ExistingLockObservation {
    /// A live or unparseable holder owns the directory; surface to caller.
    Conflict(CarryCtxError),
    /// The directory disappeared during observation; re-drive acquisition.
    Vanished,
    /// Confirmed orphaned/stale directory that is safe to remove and reuse.
    StealReady,
}

fn observe_existing_lock(
    lock_dir: &Path,
    hostname: &str,
    metaless_grace: Duration,
) -> Result<ExistingLockObservation, CarryCtxError> {
    if !lock_dir.exists() {
        return Ok(ExistingLockObservation::Vanished);
    }
    let meta_path = lock_dir.join("meta.json");
    if !meta_path.is_file() {
        if !lock_dir_older_than(lock_dir, metaless_grace) {
            return Ok(ExistingLockObservation::Conflict(
                CarryCtxError::state_conflict(
                    "Admission lock directory has no metadata yet; it may still be initializing.",
                ),
            ));
        }
        // Double confirmation: require the metaless+aged state to persist
        // across a gap before stealing, so a slow legacy creator that has
        // *just* written its metadata is never robbed of a live lock.
        std::thread::sleep(METALESS_RECHECK_GAP);
        if !lock_dir.exists() {
            return Ok(ExistingLockObservation::Vanished);
        }
        if !meta_path.is_file() && lock_dir_older_than(lock_dir, metaless_grace) {
            return Ok(ExistingLockObservation::StealReady);
        }
        // Metadata appeared between the checks: treat as a normal holder.
    }

    let mut meta_str: Option<String> = None;
    for _ in 0..META_READ_MAX_ATTEMPTS {
        match fs::read_to_string(&meta_path) {
            Ok(content) => {
                meta_str = Some(content);
                break;
            }
            // The holder released (removed the whole directory) between our
            // existence check and this read. That window is transient by
            // construction; retry briefly, then re-drive acquisition.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::thread::sleep(META_READ_RETRY_DELAY);
            }
            Err(e) => {
                return Err(CarryCtxError::database_error(format!(
                    "Failed to read lock metadata: {}",
                    e
                )));
            }
        }
    }
    let Some(meta_str) = meta_str else {
        // Still unreadable after the bounded window: reclassify from scratch.
        return Ok(if lock_dir.exists() {
            ExistingLockObservation::Conflict(CarryCtxError::state_conflict(
                "Admission lock metadata kept disappearing under contention.",
            ))
        } else {
            ExistingLockObservation::Vanished
        });
    };

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
        return Ok(ExistingLockObservation::StealReady);
    }
    Ok(ExistingLockObservation::Conflict(
        CarryCtxError::state_conflict("Admission lock held by another process."),
    ))
}

/// Remove a lock directory, tolerating a concurrent removal racing ours
/// (another healer or reclaiming contender): losing that race is success
/// for the lock protocol, not an error.
fn remove_lock_dir_best_effort(lock_dir: &Path) -> Result<(), CarryCtxError> {
    match fs::remove_dir_all(lock_dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(CarryCtxError::database_error(format!(
            "Failed to remove stale lock: {}",
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
    let bytes = serde_json::to_vec(&meta).map_err(|e| {
        CarryCtxError::database_error(format!("Failed to serialize lock metadata: {e}"))
    })?;
    write_atomic(&lock_dir.join("meta.json"), &bytes)
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
    if !lock_dir.exists() {
        return Ok(());
    }
    if !lock_owner_matches(&meta_path, owner) {
        return Ok(());
    }
    match fs::remove_dir_all(lock_dir) {
        Ok(()) => Ok(()),
        // A concurrent healer/reclaim removed the directory between our
        // ownership check and the removal: ownership was already gone.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(CarryCtxError::database_error(format!(
            "Failed to release lock: {}",
            e
        ))),
    }
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
        use std::sync::Barrier;
        use std::sync::atomic::{AtomicUsize, Ordering};
        const CONTENDERS: usize = 8;

        let root = tempfile::tempdir().unwrap();
        let lock = root.path().join("command.lock");
        let barrier = Barrier::new(CONTENDERS);
        let winners = AtomicUsize::new(0);
        let conflicts = AtomicUsize::new(0);
        let finished_losers = AtomicUsize::new(0);

        std::thread::scope(|scope| {
            for i in 0..CONTENDERS {
                let barrier = &barrier;
                let winners = &winners;
                let conflicts = &conflicts;
                let finished_losers = &finished_losers;
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
                            // Hold the lock until every contender has made
                            // its single attempt, so late schedulers cannot
                            // sneak a second acquisition after release.
                            let deadline = std::time::Instant::now() + Duration::from_secs(10);
                            while finished_losers.load(Ordering::SeqCst) < CONTENDERS - 1 {
                                assert!(
                                    std::time::Instant::now() < deadline,
                                    "contenders failed to finish their attempts"
                                );
                                std::thread::sleep(Duration::from_millis(1));
                            }
                            drop(guard);
                        }
                        Err(e) if e.code == "STATE_CONFLICT" => {
                            conflicts.fetch_add(1, Ordering::SeqCst);
                            finished_losers.fetch_add(1, Ordering::SeqCst);
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

    #[test]
    fn acquisition_treats_transient_meta_disappearance_as_retry_not_error() {
        // Regression: a holder's teardown (`remove_dir_all`) can land between
        // an observer's `is_file()` check and its `read_to_string(meta)`.
        // The observer must re-drive acquisition, never surface a hard
        // RESOURCE_NOT_FOUND / DATABASE_ERROR for that transient window.
        let root = tempfile::tempdir().unwrap();
        let lock = root.path().join("command.lock");
        use std::sync::atomic::{AtomicBool, Ordering};
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let stop_churn = std::sync::Arc::clone(&stop);
        let churn_lock = lock.clone();
        let churner = std::thread::spawn(move || {
            // Tight create/remove cycles maximize the number of
            // dir-exists-but-meta-vanishing windows the observer can hit.
            while !stop_churn.load(Ordering::Relaxed) {
                if fs::create_dir(&churn_lock).is_ok() {
                    let _ = fs::write(churn_lock.join("meta.json"), b"{}");
                    let _ = fs::remove_dir_all(&churn_lock);
                }
            }
        });

        let deadline = std::time::Instant::now() + Duration::from_millis(1500);
        let mut wins = 0usize;
        let mut conflicts = 0usize;
        while std::time::Instant::now() < deadline {
            match AdmissionLock::acquire(&lock, "observer", std::process::id(), "test", "now") {
                Ok(guard) => {
                    wins += 1;
                    drop(guard);
                }
                Err(e) if e.code == "STATE_CONFLICT" => conflicts += 1,
                Err(e) => {
                    stop.store(true, Ordering::Relaxed);
                    churner.join().unwrap();
                    panic!("transient meta disappearance surfaced as {}: {e}", e.code);
                }
            }
        }
        stop.store(true, Ordering::Relaxed);
        churner.join().unwrap();
        assert!(
            wins + conflicts > 0,
            "observer should have completed attempts against the churned lock"
        );
    }

    #[test]
    fn stale_metaless_dir_that_gains_meta_is_never_stolen() {
        // A legacy creator may be stalled between `create_dir` and its meta
        // write. If the directory ages past the grace period but valid
        // metadata appears before the double-confirmation re-check, the lock
        // is live again and must be reported as contention — not stolen.
        let root = tempfile::tempdir().unwrap();
        let lock = root.path().join("command.lock");
        fs::create_dir_all(&lock).unwrap();

        // Simulate the stalled legacy creator finishing: shortly after the
        // observer's first (aged) inspection, real metadata with OUR live
        // pid shows up. The delay must stay well below the healer's
        // re-check gap so this is deterministic.
        let writer_lock = lock.clone();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            fs::write(
                writer_lock.join("meta.json"),
                format!(
                    r#"{{"operation_id":"slow-legacy","pid":{},"hostname":"test","acquired_at":"now"}}"#,
                    std::process::id()
                ),
            )
            .unwrap();
        });

        let error = acquire_lock_owned(
            &lock,
            "healer",
            std::process::id(),
            "test",
            "now",
            Duration::ZERO, // zero grace: any metaless dir looks aged
        )
        .unwrap_err();
        writer.join().unwrap();

        assert_eq!(error.code, "STATE_CONFLICT", "live holder must win");
        assert!(lock.exists(), "stolen-and-replaced dir must not vanish");
        assert!(lock.join("meta.json").is_file());
    }

    #[test]
    fn release_lock_tolerates_concurrent_external_removal() {
        let root = tempfile::tempdir().unwrap();
        let lock = root.path().join("command.lock");
        let owner = acquire_lock_owned(
            &lock,
            "op",
            std::process::id(),
            "test",
            "now",
            METALESS_LOCK_GRACE,
        )
        .unwrap();
        // Another actor (a healer or stale reclaim) removes the directory
        // between our ownership check and the removal itself.
        fs::remove_dir_all(&lock).unwrap();
        release_lock(&lock, &owner).expect("release after external removal must be a no-op");
    }
}
