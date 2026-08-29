//! Safety evaluator for worktree cleanup — M2.
//!
//! Pure assessment (`assess_worktree_cleanup`) and idempotent execution
//! (`execute_worktree_cleanup`) are deliberately separated: `assess` never
//! mutates git state or the database, `execute` never re-implements blocker
//! policy. Blockers are sourced from the domain (`CleanupBlocker` /
//! `CleanupAssessment`) so persistence stays a single TEXT column and the
//! application layer remains the only place that touches `GitCli` or
//! repository traits — the domain stays free of `rusqlite` and `git`.

use std::path::{Path, PathBuf};

use crate::adapter::filesystem::AdmissionLock;
use crate::adapter::git::GitCli;
use crate::adapter::sqlite_repos::{
    SqliteAgentRepository, SqliteCleanupRepository, SqliteEventRepository, SqliteSessionRepository,
    SqliteWorktreeRepository,
};
use crate::adapter::unit_of_work::UnitOfWork;
use crate::domain::cleanup::CleanupState;
use crate::domain::cleanup::{CleanupAssessment, CleanupBlocker};
use crate::domain::session::SessionState;
use crate::error::CarryCtxError;
use crate::repository::{CleanupRepository, EventRepository, NewEvent};
use crate::repository::{SessionRepository, WorktreeRepository};

/// Outcome of [`execute_worktree_cleanup`].
///
/// `Blocked` is **not** an error — it is a distinct, retryable outcome that
/// carries the assessment that caused the block. `Failed` surfaces only as
/// `Err(CarryCtxError)` so callers can distinguish `STATE_CONFLICT` /
/// `GIT_ERROR` cleanly.
#[derive(Debug)]
pub enum ExecuteOutcome {
    /// A live git worktree was removed.
    Removed,
    /// The worktree was already gone (row missing or directory absent, or git
    /// reports `is not a working tree`). Idempotent completion.
    AlreadyRemoved,
    /// Removal was refused by a safety guard. Re-run `assess` or inspect the
    /// embedded assessment to render a user-facing message.
    Blocked(CleanupAssessment),
}

/// Attempt one durable cleanup request after its creating transaction has
/// committed. Git/filesystem work is deliberately outside that transaction.
pub fn try_cleanup(
    conn: &mut rusqlite::Connection,
    project_id: &str,
    task_id: &str,
    repo_root: &Path,
    actor_agent_id: Option<&str>,
    admission_lock: &AdmissionLock,
) -> Result<Vec<String>, CarryCtxError> {
    let _admission_lock = admission_lock;
    let request = SqliteCleanupRepository::new(conn)
        .find_by_task(project_id, task_id)?
        .into_iter()
        .find(|r| r.state.is_active() || r.state == CleanupState::Failed);
    let Some(request) = request else {
        return Ok(Vec::new());
    };
    let now = chrono::Utc::now().to_rfc3339();

    // Claim and audit the attempt atomically. This also recovers requests left
    // running by a crashed process and permits retryable failed attempts.
    let running = {
        let uow = UnitOfWork::begin(conn)?;
        let repo = SqliteCleanupRepository::new(uow.connection());
        let Some(record) = repo.claim_for_attempt(
            &request.id,
            project_id,
            request.state,
            request.last_attempt_at.as_deref(),
            &now,
        )?
        else {
            uow.rollback()?;
            return Ok(Vec::new());
        };
        let events = SqliteEventRepository::new(uow.connection());
        events.append(&NewEvent {
            id: ulid::Ulid::generate().to_string(),
            project_id: project_id.to_string(),
            event_type: "worktree.cleanup_started".into(),
            actor_agent_id: actor_agent_id.map(str::to_owned),
            session_id: None,
            task_id: record.task_id.clone(),
            payload: serde_json::json!({
                "cleanup_id": record.id,
                "attempt_count": record.attempt_count,
            }),
            occurred_at: now.clone(),
        })?;
        uow.commit()?;
        record
    };
    let outcome: Result<ExecuteOutcome, CarryCtxError> = {
        let sessions = SqliteSessionRepository::new(conn);
        let worktrees = SqliteWorktreeRepository::new(conn);
        assess_worktree_cleanup(
            &sessions,
            &worktrees,
            &GitCli::new(),
            project_id,
            running.worktree_id.as_deref(),
            Path::new(&running.worktree_path),
            repo_root,
            None,
        )
        .and_then(|assessment| {
            if !assessment.removable {
                Ok(ExecuteOutcome::Blocked(assessment))
            } else {
                execute_worktree_cleanup(
                    &worktrees,
                    &GitCli::new(),
                    project_id,
                    running.worktree_id.as_deref(),
                    Path::new(&running.worktree_path),
                    repo_root,
                    false,
                )
            }
        })
    };
    let (state, blocker, failure_reason, warning) = match outcome {
        Ok(ExecuteOutcome::Removed | ExecuteOutcome::AlreadyRemoved) => {
            (CleanupState::Completed, None, None, None)
        }
        Ok(ExecuteOutcome::Blocked(a)) => {
            let blocker = a.blockers.first().cloned();
            (
                CleanupState::Blocked,
                blocker,
                None,
                Some(format!(
                    "Worktree cleanup deferred: {}",
                    a.blockers
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            )
        }
        Err(err) => (
            CleanupState::Failed,
            None,
            Some(err.message.clone()),
            Some(format!("Worktree cleanup failed: {}", err.message)),
        ),
    };
    let uow = UnitOfWork::begin(conn)?;
    let repo = SqliteCleanupRepository::new(uow.connection());
    let record = repo.update_state(
        &request.id,
        project_id,
        state,
        blocker.clone(),
        &chrono::Utc::now().to_rfc3339(),
    )?;
    if state == CleanupState::Completed
        && let Some(worktree_id) = request.worktree_id.as_deref()
    {
        SqliteWorktreeRepository::new(uow.connection()).delete(worktree_id, project_id)?;
    }
    let actor_agent_id = crate::application::task::canonical_actor_id(
        project_id,
        actor_agent_id,
        &SqliteAgentRepository::new(uow.connection()),
    )?;
    let events = SqliteEventRepository::new(uow.connection());
    events.append(&NewEvent {
        id: ulid::Ulid::generate().to_string(),
        project_id: project_id.to_string(),
        event_type: match state {
            CleanupState::Completed => "worktree.removed",
            CleanupState::Blocked => "worktree.cleanup_blocked",
            CleanupState::Failed => "worktree.cleanup_failed",
            _ => unreachable!("try_cleanup only persists completed, blocked, or failed"),
        }.into(),
        actor_agent_id,
        session_id: None,
        task_id: Some(task_id.to_string()),
        payload: serde_json::json!({"cleanup_id": record.id, "state": state, "blocked_reason": blocker, "error": failure_reason}),
        occurred_at: chrono::Utc::now().to_rfc3339(),
    })?;
    uow.commit()?;
    Ok(warning.into_iter().collect())
}

/// Reconcile every retryable cleanup request for a project. This is the M4/
/// scheduler entry point; unlike task completion it does not require a task
/// reference and safely skips requests claimed by another process.
pub fn reconcile_pending_cleanup(
    conn: &mut rusqlite::Connection,
    project_id: &str,
    repo_root: &Path,
    actor_agent_id: Option<&str>,
    admission_lock: &AdmissionLock,
) -> Result<Vec<String>, CarryCtxError> {
    let requests = SqliteCleanupRepository::new(conn).find_pending_by_project(project_id)?;
    let mut warnings = Vec::new();
    for request in requests {
        warnings.extend(reconcile_cleanup_request(
            conn,
            project_id,
            request,
            repo_root,
            actor_agent_id,
            admission_lock,
        )?);
    }
    Ok(warnings)
}

fn reconcile_cleanup_request(
    conn: &mut rusqlite::Connection,
    project_id: &str,
    request: crate::repository::CleanupRecord,
    repo_root: &Path,
    actor_agent_id: Option<&str>,
    admission_lock: &AdmissionLock,
) -> Result<Vec<String>, CarryCtxError> {
    let request_id = request.id.clone();
    let task_id = request.task_id.clone().unwrap_or_default();
    if request.task_id.is_some() {
        return try_cleanup(
            conn,
            project_id,
            &task_id,
            repo_root,
            actor_agent_id,
            admission_lock,
        );
    }
    try_cleanup_request(
        conn,
        project_id,
        request_id,
        repo_root,
        actor_agent_id,
        admission_lock,
    )
}

fn try_cleanup_request(
    conn: &mut rusqlite::Connection,
    project_id: &str,
    request_id: String,
    repo_root: &Path,
    actor_agent_id: Option<&str>,
    admission_lock: &AdmissionLock,
) -> Result<Vec<String>, CarryCtxError> {
    let request = SqliteCleanupRepository::new(conn)
        .find_by_id(project_id, &request_id)?
        .ok_or_else(|| CarryCtxError::resource_not_found("Cleanup request not found."))?;
    reconcile_request_record(
        conn,
        project_id,
        request,
        repo_root,
        actor_agent_id,
        admission_lock,
    )
}

fn reconcile_request_record(
    conn: &mut rusqlite::Connection,
    project_id: &str,
    request: crate::repository::CleanupRecord,
    repo_root: &Path,
    actor_agent_id: Option<&str>,
    admission_lock: &AdmissionLock,
) -> Result<Vec<String>, CarryCtxError> {
    let _ = admission_lock;
    let now = chrono::Utc::now().to_rfc3339();
    let uow = UnitOfWork::begin(conn)?;
    let repo = SqliteCleanupRepository::new(uow.connection());
    let Some(running) = repo.claim_for_attempt(
        &request.id,
        project_id,
        request.state,
        request.last_attempt_at.as_deref(),
        &now,
    )?
    else {
        uow.rollback()?;
        return Ok(Vec::new());
    };
    SqliteEventRepository::new(uow.connection()).append(&NewEvent {
        id: ulid::Ulid::generate().to_string(), project_id: project_id.to_string(),
        event_type: "worktree.cleanup_started".into(), actor_agent_id: actor_agent_id.map(str::to_owned),
        session_id: None, task_id: running.task_id.clone(),
        payload: serde_json::json!({"cleanup_id": running.id, "attempt_count": running.attempt_count}), occurred_at: now,
    })?;
    uow.commit()?;
    finalize_cleanup_request(conn, project_id, running, repo_root, actor_agent_id)
}

fn finalize_cleanup_request(
    conn: &mut rusqlite::Connection,
    project_id: &str,
    running: crate::repository::CleanupRecord,
    repo_root: &Path,
    actor_agent_id: Option<&str>,
) -> Result<Vec<String>, CarryCtxError> {
    let sessions = SqliteSessionRepository::new(conn);
    let worktrees = SqliteWorktreeRepository::new(conn);
    let outcome = assess_worktree_cleanup(
        &sessions,
        &worktrees,
        &GitCli::new(),
        project_id,
        running.worktree_id.as_deref(),
        Path::new(&running.worktree_path),
        repo_root,
        None,
    )
    .and_then(|assessment| {
        if !assessment.removable {
            Ok(ExecuteOutcome::Blocked(assessment))
        } else {
            execute_worktree_cleanup(
                &worktrees,
                &GitCli::new(),
                project_id,
                running.worktree_id.as_deref(),
                Path::new(&running.worktree_path),
                repo_root,
                false,
            )
        }
    });
    let (state, blocker, failure_reason, warning) = match outcome {
        Ok(ExecuteOutcome::Removed | ExecuteOutcome::AlreadyRemoved) => {
            (CleanupState::Completed, None, None, None)
        }
        Ok(ExecuteOutcome::Blocked(assessment)) => {
            let blocker = assessment.blockers.first().cloned();
            (
                CleanupState::Blocked,
                blocker,
                None,
                Some(format!(
                    "Worktree cleanup deferred: {}",
                    assessment
                        .blockers
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            )
        }
        Err(err) => (
            CleanupState::Failed,
            None,
            Some(err.message.clone()),
            Some(format!("Worktree cleanup failed: {}", err.message)),
        ),
    };
    let uow = UnitOfWork::begin(conn)?;
    let repo = SqliteCleanupRepository::new(uow.connection());
    let record = repo.update_state(
        &running.id,
        project_id,
        state,
        blocker.clone(),
        &chrono::Utc::now().to_rfc3339(),
    )?;
    if state == CleanupState::Completed
        && let Some(worktree_id) = running.worktree_id.as_deref()
    {
        SqliteWorktreeRepository::new(uow.connection()).delete(worktree_id, project_id)?;
    }
    let actor_agent_id = crate::application::task::canonical_actor_id(
        project_id,
        actor_agent_id,
        &SqliteAgentRepository::new(uow.connection()),
    )?;
    SqliteEventRepository::new(uow.connection()).append(&NewEvent {
        id: ulid::Ulid::generate().to_string(),
        project_id: project_id.to_string(),
        event_type: match state {
            CleanupState::Completed => "worktree.removed",
            CleanupState::Blocked => "worktree.cleanup_blocked",
            CleanupState::Failed => "worktree.cleanup_failed",
            _ => unreachable!(),
        }
        .into(),
        actor_agent_id,
        session_id: None,
        task_id: running.task_id,
        payload: serde_json::json!({
            "cleanup_id": record.id,
            "state": state,
            "blocked_reason": blocker,
            "error": failure_reason,
        }),
        occurred_at: chrono::Utc::now().to_rfc3339(),
    })?;
    uow.commit()?;
    Ok(warning.into_iter().collect())
}

// ---------------------------------------------------------------------------
// Assessment
// ---------------------------------------------------------------------------

/// Assess whether `worktree_path` can be removed.
///
/// Checks, in order:
/// 1. **Active sessions** — any `SessionState::Active` row whose
///    `worktree_id` matches `worktree_id` *or* whose `cwd` equals / is
///    inside `worktree_path`.
/// 2. **Current working directory** — `env::current_dir()` (or the injected
///    override) is inside `worktree_path`.
/// 3. **Missing git metadata** — `worktree_id` was supplied but no
///    `worktrees` row exists, *or* the directory exists but is not recognised
///    as a git worktree (discover fails and it is absent from
///    `git worktree list`).
/// 4. **Dirty worktree** — `git status --porcelain` is non-empty.
/// 5. **Locked worktree** — `git worktree list --porcelain` emits a `locked`
///    line for this path.
///
/// `assess` never calls `git worktree remove` and never mutates the DB.
/// Pass `current_dir_override` in tests to avoid depending on the process
/// cwd.
pub fn assess_worktree_cleanup(
    session_repo: &dyn SessionRepository,
    worktree_repo: &dyn WorktreeRepository,
    git_cli: &GitCli,
    project_id: &str,
    worktree_id: Option<&str>,
    worktree_path: &Path,
    repo_root: &Path,
    current_dir_override: Option<&Path>,
) -> Result<CleanupAssessment, CarryCtxError> {
    let mut blockers = Vec::new();

    // 1. Active sessions
    if let Some(ids) =
        collect_active_session_blockers(session_repo, project_id, worktree_id, worktree_path)?
    {
        blockers.extend(ids);
    }

    // 2. Current working directory
    if is_current_dir_inside(worktree_path, current_dir_override) {
        blockers.push(CleanupBlocker::CurrentWorkingDirectory);
    }

    // 3. Missing git metadata
    if is_missing_git_metadata(
        worktree_repo,
        git_cli,
        project_id,
        worktree_id,
        worktree_path,
        repo_root,
    ) {
        blockers.push(CleanupBlocker::MissingGitMetadata);
    }

    // 4. Dirty worktree (only when directory exists; a missing dir is handled
    //    by idempotent execute, not by blocking assess)
    if worktree_path.exists() && is_dirty_worktree(git_cli, worktree_path) {
        blockers.push(CleanupBlocker::DirtyWorktree);
    }

    // 5. Locked worktree
    if is_worktree_locked(git_cli, repo_root, worktree_path) {
        blockers.push(CleanupBlocker::WorktreeLocked);
    }

    if blockers.is_empty() {
        Ok(CleanupAssessment::removable())
    } else {
        Ok(CleanupAssessment::blocked(blockers))
    }
}

/// Convenience wrapper that reads `env::current_dir()` directly.
///
/// Prefer the 8-arg form in tests where an injected cwd is required.
pub fn assess_worktree_cleanup_simple(
    session_repo: &dyn SessionRepository,
    worktree_repo: &dyn WorktreeRepository,
    git_cli: &GitCli,
    project_id: &str,
    worktree_id: Option<&str>,
    worktree_path: &Path,
    repo_root: &Path,
) -> Result<CleanupAssessment, CarryCtxError> {
    assess_worktree_cleanup(
        session_repo,
        worktree_repo,
        git_cli,
        project_id,
        worktree_id,
        worktree_path,
        repo_root,
        None,
    )
}

// ---------------------------------------------------------------------------
// Execution — idempotent, separate from assess
// ---------------------------------------------------------------------------

/// Idempotently remove a worktree directory via `git worktree remove`.
///
/// * If the `worktrees` row is missing **or** `worktree_path` does not exist
///   on the filesystem, returns `AlreadyRemoved` without invoking git.
/// * Otherwise calls `GitCli::remove_worktree`. `is not a working tree` is
///   mapped to `AlreadyRemoved` (git already pruned it). `STATE_CONFLICT`
///   (dirty / locked) is mapped to `Blocked`. Any other `GIT_ERROR` is
///   returned as `Err` (`Failed`).
///
/// `force` is forwarded verbatim to `git worktree remove --force`.
///
/// This function does **not** delete the `worktrees` registration row and
/// does **not** mutate `worktree_cleanup_requests` — those transitions belong
/// to M3/M4. It is purely the git side-effect, wrapped with idempotency.
pub fn execute_worktree_cleanup(
    worktree_repo: &dyn WorktreeRepository,
    git_cli: &GitCli,
    project_id: &str,
    worktree_id: Option<&str>,
    worktree_path: &Path,
    repo_root: &Path,
    force: bool,
) -> Result<ExecuteOutcome, CarryCtxError> {
    // Cheap idempotency: row missing or path absent — already completed.
    let row_missing =
        is_worktree_row_missing(worktree_repo, project_id, worktree_id, worktree_path);
    let path_missing = !worktree_path.exists();

    if row_missing || path_missing {
        // When both are missing it's definitely idempotent. When only the
        // directory is missing but the row still exists, treat as already
        // removed as well — git would report `is not a working tree` anyway,
        // and the caller (M3/M4) owns row deletion.
        return Ok(ExecuteOutcome::AlreadyRemoved);
    }

    match git_cli.remove_worktree(repo_root, worktree_path, force) {
        Ok(()) => Ok(ExecuteOutcome::Removed),
        Err(err) => {
            let msg = err.message.to_lowercase();
            // Git's own idempotency: the directory is no longer registered as
            // a worktree (already pruned or foreign directory).
            if msg.contains("is not a working tree") || msg.contains("not a working tree") {
                return Ok(ExecuteOutcome::AlreadyRemoved);
            }
            // Locked worktree — git says `cannot remove a locked working tree`
            // and requires `-f -f` or unlock.
            if msg.contains("locked working tree") || msg.contains("lock reason") {
                let assessment = CleanupAssessment::blocked(vec![CleanupBlocker::WorktreeLocked]);
                return Ok(ExecuteOutcome::Blocked(assessment));
            }
            // Dirty guard — surfaced as STATE_CONFLICT by GitCli.
            if err.code == "STATE_CONFLICT" {
                // Distinguish dirty vs generic conflict: DirtyWorktree is the
                // common case; callers that need finer granularity can re-run
                // assess.
                let assessment = CleanupAssessment::blocked(vec![CleanupBlocker::DirtyWorktree]);
                return Ok(ExecuteOutcome::Blocked(assessment));
            }
            // Current directory guard (rare, but keep it distinct).
            if msg.contains("current working directory") || msg.contains("is the current") {
                let assessment =
                    CleanupAssessment::blocked(vec![CleanupBlocker::CurrentWorkingDirectory]);
                return Ok(ExecuteOutcome::Blocked(assessment));
            }
            Err(err)
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers — each blocker is a small testable predicate
// ---------------------------------------------------------------------------

fn collect_active_session_blockers(
    session_repo: &dyn SessionRepository,
    project_id: &str,
    worktree_id: Option<&str>,
    worktree_path: &Path,
) -> Result<Option<Vec<CleanupBlocker>>, CarryCtxError> {
    let sessions = session_repo.list(project_id)?;
    let wp_str = worktree_path.to_string_lossy().to_string();
    let mut out = Vec::new();

    for s in sessions {
        if s.state != SessionState::Active {
            continue;
        }
        // Match by worktree_id when available.
        let matches_id = match (worktree_id, s.worktree_id.as_deref()) {
            (Some(wid), Some(sid)) => wid == sid,
            _ => false,
        };
        // Match by cwd: exact equality or prefix (session is inside worktree).
        let matches_cwd = if let Some(cwd) = s.cwd.as_deref() {
            cwd == wp_str
                || Path::new(cwd).starts_with(worktree_path)
                || wp_str.starts_with(cwd) && cwd_matches_worktree(cwd, worktree_path)
        } else {
            false
        };

        // When worktree_id is None we still match on cwd; when it is Some,
        // either id or cwd suffices (a session may be bound to the worktree
        // without recording worktree_id, e.g. legacy rows).
        if matches_id || matches_cwd {
            out.push(CleanupBlocker::ActiveSession {
                session_id: s.id.clone(),
            });
        }
    }

    if out.is_empty() {
        Ok(None)
    } else {
        Ok(Some(out))
    }
}

/// Strict prefix check that tolerates both string forms of cwd.
fn cwd_matches_worktree(cwd: &str, worktree_path: &Path) -> bool {
    // Normalise both sides via Path comparison; fall back to string prefix
    // when one side is not valid UTF-8 on disk.
    let cwd_path = Path::new(cwd);
    cwd_path.starts_with(worktree_path) || worktree_path.starts_with(cwd_path)
}

fn is_current_dir_inside(worktree_path: &Path, override_dir: Option<&Path>) -> bool {
    let current = if let Some(p) = override_dir {
        p.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(p) => p,
            Err(_) => return false,
        }
    };

    // Canonicalize both sides when possible, but degrade gracefully when the
    // worktree directory does not exist (missing dir is not a cwd block).
    let wt_canon = worktree_path
        .canonicalize()
        .unwrap_or_else(|_| worktree_path.to_path_buf());
    let cur_canon = current.canonicalize().unwrap_or(current);

    cur_canon == wt_canon || cur_canon.starts_with(&wt_canon)
}

fn is_missing_git_metadata(
    worktree_repo: &dyn WorktreeRepository,
    git_cli: &GitCli,
    project_id: &str,
    worktree_id: Option<&str>,
    worktree_path: &Path,
    repo_root: &Path,
) -> bool {
    // A supplied worktree_id that has no row is missing metadata.
    if let Some(wid) = worktree_id {
        if let Ok(None) = worktree_repo.find_by_id(project_id, wid) {
            return true;
        }
    }

    // Path does not exist at all — missing dir is *not* treated as MissingGitMetadata
    // here; execute handles it as AlreadyRemoved. Only flag MissingGitMetadata
    // when the directory exists but git does not recognise it.
    if !worktree_path.exists() {
        return false;
    }

    // Check whether git recognises this path as a worktree.
    // First try to discover it as a git repo; failure suggests missing metadata.
    // Then cross-check `git worktree list --porcelain`.
    let is_git_worktree = git_cli
        .list_worktrees(repo_root)
        .map(|entries| {
            entries.iter().any(|e| {
                Path::new(&e.path) == worktree_path
                    || same_path_canonical(Path::new(&e.path), worktree_path)
            })
        })
        .unwrap_or(false);

    // Also consider a worktree that has a missing `.git` file (common for
    // secondary worktrees) as not missing — the `git worktree list` check is
    // authoritative.
    if is_git_worktree {
        return false;
    }

    // If it is not in `git worktree list` but the directory exists, check
    // whether the path is at least inside the repository. A path that is
    // completely foreign (e.g. /tmp/other) with no worktree registration is
    // not "missing metadata" — it's just not a worktree.
    // Only flag MissingGitMetadata when the caller expected a worktree (has an id
    // or the path is under repo_root) but git disagrees.
    if worktree_id.is_some() {
        return true;
    }

    // No id, but path exists under repo_root and is not a registered worktree:
    // treat as missing metadata only if the path looks like a worktree
    // (contains a `.git` file or directory). Otherwise it's just a regular
    // directory, not a worktree at all.
    let git_file = worktree_path.join(".git");
    git_file.exists()
}

fn same_path_canonical(a: &Path, b: &Path) -> bool {
    let a_c = a.canonicalize().unwrap_or_else(|_| a.to_path_buf());
    let b_c = b.canonicalize().unwrap_or_else(|_| b.to_path_buf());
    a_c == b_c
}

fn is_dirty_worktree(git_cli: &GitCli, worktree_path: &Path) -> bool {
    // Use the snapshot helper when available — it shells out to
    // `git status --porcelain`. If that fails (e.g. not a git repo) treat as
    // not dirty; MissingGitMetadata already covers that case.
    match git_cli.get_snapshot(worktree_path) {
        Ok(snap) => snap.dirty,
        Err(_) => false,
    }
}

fn is_worktree_locked(git_cli: &GitCli, repo_root: &Path, worktree_path: &Path) -> bool {
    if let Ok(entries) = git_cli.list_worktrees(repo_root) {
        for e in entries {
            let matches = Path::new(&e.path) == worktree_path
                || same_path_canonical(Path::new(&e.path), worktree_path);
            if matches {
                return e.locked.is_some();
            }
        }
    }
    // Fallback: check the git-common-dir filesystem marker
    // `<common>/worktrees/<name>/locked` when `git worktree list` is
    // unavailable (e.g. repo_root not a git repo). Best-effort only.
    filesystem_locked_fallback(repo_root, worktree_path)
}

fn filesystem_locked_fallback(_repo_root: &Path, _worktree_path: &Path) -> bool {
    // Without a reliable mapping from worktree path to worktree name we
    // cannot safely inspect the `locked` file directly. Return false and let
    // the git path be authoritative. This hook exists so future code can add
    // a direct read once the name mapping is available.
    false
}

fn is_worktree_row_missing(
    worktree_repo: &dyn WorktreeRepository,
    project_id: &str,
    worktree_id: Option<&str>,
    worktree_path: &Path,
) -> bool {
    // Prefer id lookup when available.
    if let Some(wid) = worktree_id {
        if let Ok(Some(_)) = worktree_repo.find_by_id(project_id, wid) {
            return false;
        }
        // Id was supplied but row missing — treat as missing.
        // Fall through to also check path-based lookup before declaring missing
        // (there could be a row under a different id).
    }
    // Path-based lookup.
    match worktree_repo.find_by_path(project_id, &worktree_path.to_string_lossy()) {
        Ok(Some(_)) => false,
        Ok(None) => true,
        Err(_) => true,
    }
}

// Keep `PathBuf` import used for the cwd canonicalization helper.
#[allow(dead_code)]
fn _use_pathbuf(p: PathBuf) -> PathBuf {
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::git::GitCli;
    use crate::adapter::sqlite::ProjectDatabase;
    use crate::adapter::sqlite_repos::{SqliteSessionRepository, SqliteWorktreeRepository};
    use crate::repository::NewSession;
    use std::process::Command as StdCommand;

    const GIT_STATE_VARS: &[&str] = &[
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_COMMON_DIR",
        "GIT_NAMESPACE",
        "GIT_CEILING_DIRECTORIES",
        "GIT_AUTHOR_NAME",
        "GIT_AUTHOR_EMAIL",
        "GIT_AUTHOR_DATE",
        "GIT_COMMITTER_NAME",
        "GIT_COMMITTER_EMAIL",
        "GIT_COMMITTER_DATE",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_SYSTEM",
    ];

    fn git_fixture(repo_root: &Path, args: &[&str]) {
        let mut cmd = StdCommand::new("git");
        cmd.args(args).current_dir(repo_root);
        for v in GIT_STATE_VARS {
            cmd.env_remove(v);
        }
        let out = cmd.output().unwrap_or_else(|e| panic!("git {args:?}: {e}"));
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn init_repo() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        git_fixture(&root, &["init", "-b", "main"]);
        git_fixture(&root, &["config", "user.email", "test@example.com"]);
        git_fixture(&root, &["config", "user.name", "Test"]);
        std::fs::write(root.join("README.md"), "# test\n").unwrap();
        git_fixture(&root, &["add", "."]);
        git_fixture(&root, &["commit", "-m", "init"]);
        (dir, root)
    }

    fn seeded_db(dir: &Path) -> ProjectDatabase {
        let mut db = ProjectDatabase::open(dir.join("test.sqlite")).unwrap();
        db.migrate().unwrap();
        db.connection_mut()
            .execute(
                "INSERT INTO projects (id, name, task_prefix, repository_root, git_common_dir, main_branch, schema_version, created_at, updated_at)
                 VALUES ('p1', 'proj', 'CTX', '/tmp/r1', '/tmp/g1', 'main', 1, 'now', 'now')",
                [],
            )
            .unwrap();
        db
    }

    // -----------------------------------------------------------------------
    // assess: blocker parsing / DB round-trip helpers (domain already covers
    // the string mapping, but we verify the application wiring too)
    // -----------------------------------------------------------------------

    #[test]
    fn blocker_db_string_round_trips_via_domain() {
        let cases = vec![
            CleanupBlocker::ActiveSession {
                session_id: "sess-1".into(),
            },
            CleanupBlocker::DirtyWorktree,
            CleanupBlocker::CurrentWorkingDirectory,
            CleanupBlocker::MissingGitMetadata,
            CleanupBlocker::WorktreeLocked,
        ];
        for b in cases {
            let s = b.to_db_string();
            let back = CleanupBlocker::from_db_string(&s).expect("round-trip");
            assert_eq!(back, b);
            assert_eq!(b.to_string(), s);
        }
    }

    #[test]
    fn assess_removable_when_no_blockers() {
        let (_tmp, repo_root) = init_repo();
        let wt = repo_root.join("wt-clean");
        GitCli::new()
            .create_worktree(&repo_root, &wt, "feat/clean", None)
            .unwrap();

        let db_dir = tempfile::tempdir().unwrap();
        let mut db = seeded_db(db_dir.path());
        let conn = db.connection_mut();
        conn.execute(
            "INSERT INTO worktrees (id, project_id, task_id, normalized_path, git_common_dir, branch, head, bound_at, updated_at)
             VALUES ('wt1', 'p1', NULL, ?1, '', 'feat/clean', 'abc', 'now', 'now')",
            rusqlite::params![wt.to_string_lossy().to_string()],
        )
        .unwrap();

        let session_repo = SqliteSessionRepository::new(conn);
        let worktree_repo = SqliteWorktreeRepository::new(conn);
        let git_cli = GitCli::new();

        let assessment = assess_worktree_cleanup(
            &session_repo,
            &worktree_repo,
            &git_cli,
            "p1",
            Some("wt1"),
            &wt,
            &repo_root,
            Some(&PathBuf::from("/tmp")),
        )
        .unwrap();

        assert!(assessment.removable);
        assert!(assessment.blockers.is_empty());
    }

    #[test]
    fn assess_detects_active_session_by_worktree_id() {
        let (_tmp, repo_root) = init_repo();
        let wt = repo_root.join("wt-active");
        GitCli::new()
            .create_worktree(&repo_root, &wt, "feat/active", None)
            .unwrap();

        let db_dir = tempfile::tempdir().unwrap();
        let mut db = seeded_db(db_dir.path());
        {
            let conn = db.connection_mut();
            conn.execute(
                "INSERT INTO worktrees (id, project_id, task_id, normalized_path, git_common_dir, branch, head, bound_at, updated_at)
                 VALUES ('wt1', 'p1', NULL, ?1, '', 'feat/active', 'abc', 'now', 'now')",
                rusqlite::params![wt.to_string_lossy().to_string()],
            )
            .unwrap();
            // Active session bound to this worktree.
            conn.execute(
                "INSERT INTO agents (id, project_id, name, provider, role, kind, created_at, updated_at)
                 VALUES ('agent1', 'p1', 'a1', 'test', NULL, NULL, 'now', 'now')",
                [],
            )
            .unwrap();
            let sess = SqliteSessionRepository::new(conn);
            sess.create(
                &NewSession {
                    id: "sess1".into(),
                    project_id: "p1".into(),
                    agent_id: "agent1".into(),
                    task_id: None,
                    worktree_id: Some("wt1".into()),
                    branch: None,
                    head: None,
                    cwd: Some(wt.to_string_lossy().to_string()),
                    provider: None,
                },
                "now",
            )
            .unwrap();
        }

        let conn = db.connection_mut();
        let session_repo = SqliteSessionRepository::new(conn);
        let worktree_repo = SqliteWorktreeRepository::new(conn);
        let git_cli = GitCli::new();

        let assessment = assess_worktree_cleanup(
            &session_repo,
            &worktree_repo,
            &git_cli,
            "p1",
            Some("wt1"),
            &wt,
            &repo_root,
            Some(&PathBuf::from("/tmp")),
        )
        .unwrap();

        assert!(!assessment.removable);
        assert!(
            assessment
                .blockers
                .iter()
                .any(|b| matches!(b, CleanupBlocker::ActiveSession { .. }))
        );
    }

    #[test]
    fn assess_detects_active_session_by_cwd() {
        let (_tmp, repo_root) = init_repo();
        let wt = repo_root.join("wt-cwd");
        GitCli::new()
            .create_worktree(&repo_root, &wt, "feat/cwd", None)
            .unwrap();

        let db_dir = tempfile::tempdir().unwrap();
        let mut db = seeded_db(db_dir.path());
        {
            let conn = db.connection_mut();
            conn.execute(
                "INSERT INTO worktrees (id, project_id, task_id, normalized_path, git_common_dir, branch, head, bound_at, updated_at)
                 VALUES ('wt1', 'p1', NULL, ?1, '', 'feat/cwd', 'abc', 'now', 'now')",
                rusqlite::params![wt.to_string_lossy().to_string()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO agents (id, project_id, name, provider, role, kind, created_at, updated_at)
                 VALUES ('agent1', 'p1', 'a1', 'test', NULL, NULL, 'now', 'now')",
                [],
            )
            .unwrap();
            let sess = SqliteSessionRepository::new(conn);
            sess.create(
                &NewSession {
                    id: "sess2".into(),
                    project_id: "p1".into(),
                    agent_id: "agent1".into(),
                    task_id: None,
                    worktree_id: None,
                    branch: None,
                    head: None,
                    cwd: Some(wt.to_string_lossy().to_string()),
                    provider: None,
                },
                "now",
            )
            .unwrap();
        }

        let conn = db.connection_mut();
        let session_repo = SqliteSessionRepository::new(conn);
        let worktree_repo = SqliteWorktreeRepository::new(conn);
        let git_cli = GitCli::new();

        let assessment = assess_worktree_cleanup(
            &session_repo,
            &worktree_repo,
            &git_cli,
            "p1",
            Some("wt1"),
            &wt,
            &repo_root,
            Some(&PathBuf::from("/tmp")),
        )
        .unwrap();

        assert!(
            assessment
                .blockers
                .iter()
                .any(|b| matches!(b, CleanupBlocker::ActiveSession { .. }))
        );
    }

    #[test]
    fn assess_detects_current_working_directory() {
        let (_tmp, repo_root) = init_repo();
        let wt = repo_root.join("wt-cwd2");
        GitCli::new()
            .create_worktree(&repo_root, &wt, "feat/cwd2", None)
            .unwrap();

        let db_dir = tempfile::tempdir().unwrap();
        let mut db = seeded_db(db_dir.path());
        db.connection_mut()
            .execute(
                "INSERT INTO worktrees (id, project_id, task_id, normalized_path, git_common_dir, branch, head, bound_at, updated_at)
                 VALUES ('wt1', 'p1', NULL, ?1, '', 'feat/cwd2', 'abc', 'now', 'now')",
                rusqlite::params![wt.to_string_lossy().to_string()],
            )
            .unwrap();

        let conn = db.connection_mut();
        let session_repo = SqliteSessionRepository::new(conn);
        let worktree_repo = SqliteWorktreeRepository::new(conn);
        let git_cli = GitCli::new();

        let assessment = assess_worktree_cleanup(
            &session_repo,
            &worktree_repo,
            &git_cli,
            "p1",
            Some("wt1"),
            &wt,
            &repo_root,
            Some(&wt),
        )
        .unwrap();

        assert!(
            assessment
                .blockers
                .contains(&CleanupBlocker::CurrentWorkingDirectory)
        );
    }

    #[test]
    fn assess_detects_dirty_worktree() {
        let (_tmp, repo_root) = init_repo();
        let wt = repo_root.join("wt-dirty");
        GitCli::new()
            .create_worktree(&repo_root, &wt, "feat/dirty", None)
            .unwrap();
        // Make it dirty: untracked file.
        std::fs::write(wt.join("untracked.txt"), "hello\n").unwrap();

        let db_dir = tempfile::tempdir().unwrap();
        let mut db = seeded_db(db_dir.path());
        db.connection_mut()
            .execute(
                "INSERT INTO worktrees (id, project_id, task_id, normalized_path, git_common_dir, branch, head, bound_at, updated_at)
                 VALUES ('wt1', 'p1', NULL, ?1, '', 'feat/dirty', 'abc', 'now', 'now')",
                rusqlite::params![wt.to_string_lossy().to_string()],
            )
            .unwrap();

        let conn = db.connection_mut();
        let session_repo = SqliteSessionRepository::new(conn);
        let worktree_repo = SqliteWorktreeRepository::new(conn);
        let git_cli = GitCli::new();

        let assessment = assess_worktree_cleanup(
            &session_repo,
            &worktree_repo,
            &git_cli,
            "p1",
            Some("wt1"),
            &wt,
            &repo_root,
            Some(&PathBuf::from("/tmp")),
        )
        .unwrap();

        assert!(assessment.blockers.contains(&CleanupBlocker::DirtyWorktree));
    }

    #[test]
    fn assess_detects_locked_worktree() {
        let (_tmp, repo_root) = init_repo();
        let wt = repo_root.join("wt-locked");
        GitCli::new()
            .create_worktree(&repo_root, &wt, "feat/locked", None)
            .unwrap();
        // Lock it via git.
        let mut cmd = StdCommand::new("git");
        cmd.args(["worktree", "lock", "--reason", "test lock"])
            .arg(&wt)
            .current_dir(&repo_root);
        for v in GIT_STATE_VARS {
            cmd.env_remove(v);
        }
        let out = cmd.output().expect("lock");
        assert!(out.status.success(), "lock failed: {:?}", out);

        let db_dir = tempfile::tempdir().unwrap();
        let mut db = seeded_db(db_dir.path());
        db.connection_mut()
            .execute(
                "INSERT INTO worktrees (id, project_id, task_id, normalized_path, git_common_dir, branch, head, bound_at, updated_at)
                 VALUES ('wt1', 'p1', NULL, ?1, '', 'feat/locked', 'abc', 'now', 'now')",
                rusqlite::params![wt.to_string_lossy().to_string()],
            )
            .unwrap();

        let conn = db.connection_mut();
        let session_repo = SqliteSessionRepository::new(conn);
        let worktree_repo = SqliteWorktreeRepository::new(conn);
        let git_cli = GitCli::new();

        let assessment = assess_worktree_cleanup(
            &session_repo,
            &worktree_repo,
            &git_cli,
            "p1",
            Some("wt1"),
            &wt,
            &repo_root,
            Some(&PathBuf::from("/tmp")),
        )
        .unwrap();

        assert!(
            assessment
                .blockers
                .contains(&CleanupBlocker::WorktreeLocked)
        );
    }

    #[test]
    fn assess_missing_git_metadata_when_id_has_no_row() {
        let (_tmp, repo_root) = init_repo();
        let wt = repo_root.join("wt-exists");
        std::fs::create_dir_all(&wt).unwrap();

        let db_dir = tempfile::tempdir().unwrap();
        let mut db = seeded_db(db_dir.path());
        // No worktree row inserted — id `ghost` is dangling.

        let conn = db.connection_mut();
        let session_repo = SqliteSessionRepository::new(conn);
        let worktree_repo = SqliteWorktreeRepository::new(conn);
        let git_cli = GitCli::new();

        let assessment = assess_worktree_cleanup(
            &session_repo,
            &worktree_repo,
            &git_cli,
            "p1",
            Some("ghost"),
            &wt,
            &repo_root,
            Some(&PathBuf::from("/tmp")),
        )
        .unwrap();

        assert!(
            assessment
                .blockers
                .contains(&CleanupBlocker::MissingGitMetadata)
        );
    }

    #[test]
    fn assess_is_idempotent_across_calls() {
        let (_tmp, repo_root) = init_repo();
        let wt = repo_root.join("wt-idem");
        GitCli::new()
            .create_worktree(&repo_root, &wt, "feat/idem", None)
            .unwrap();

        let db_dir = tempfile::tempdir().unwrap();
        let mut db = seeded_db(db_dir.path());
        db.connection_mut()
            .execute(
                "INSERT INTO worktrees (id, project_id, task_id, normalized_path, git_common_dir, branch, head, bound_at, updated_at)
                 VALUES ('wt1', 'p1', NULL, ?1, '', 'feat/idem', 'abc', 'now', 'now')",
                rusqlite::params![wt.to_string_lossy().to_string()],
            )
            .unwrap();
        std::fs::write(wt.join("dirty.txt"), "x\n").unwrap();

        let conn = db.connection_mut();
        let session_repo = SqliteSessionRepository::new(conn);
        let worktree_repo = SqliteWorktreeRepository::new(conn);
        let git_cli = GitCli::new();

        let a1 = assess_worktree_cleanup(
            &session_repo,
            &worktree_repo,
            &git_cli,
            "p1",
            Some("wt1"),
            &wt,
            &repo_root,
            Some(&PathBuf::from("/tmp")),
        )
        .unwrap();
        let a2 = assess_worktree_cleanup(
            &session_repo,
            &worktree_repo,
            &git_cli,
            "p1",
            Some("wt1"),
            &wt,
            &repo_root,
            Some(&PathBuf::from("/tmp")),
        )
        .unwrap();

        assert_eq!(a1, a2);
        // assess must not mutate git state — second call still sees dirty.
        assert!(a1.blockers.contains(&CleanupBlocker::DirtyWorktree));
    }

    // -----------------------------------------------------------------------
    // execute: idempotency
    // -----------------------------------------------------------------------

    #[test]
    fn execute_is_idempotent_when_path_missing() {
        let (_tmp, repo_root) = init_repo();
        let missing = repo_root.join("does-not-exist");

        let db_dir = tempfile::tempdir().unwrap();
        let mut db = seeded_db(db_dir.path());
        let conn = db.connection_mut();
        let worktree_repo = SqliteWorktreeRepository::new(conn);
        let git_cli = GitCli::new();

        let r1 = execute_worktree_cleanup(
            &worktree_repo,
            &git_cli,
            "p1",
            Some("ghost"),
            &missing,
            &repo_root,
            false,
        )
        .unwrap();
        assert!(matches!(r1, ExecuteOutcome::AlreadyRemoved));

        let r2 = execute_worktree_cleanup(
            &worktree_repo,
            &git_cli,
            "p1",
            Some("ghost"),
            &missing,
            &repo_root,
            false,
        )
        .unwrap();
        assert!(matches!(r2, ExecuteOutcome::AlreadyRemoved));
    }

    #[test]
    fn execute_is_idempotent_when_row_missing() {
        let (_tmp, repo_root) = init_repo();
        let wt = repo_root.join("wt-ghost");
        std::fs::create_dir_all(&wt).unwrap();

        let db_dir = tempfile::tempdir().unwrap();
        let mut db = seeded_db(db_dir.path());
        // No row for `ghost`.

        let conn = db.connection_mut();
        let worktree_repo = SqliteWorktreeRepository::new(conn);
        let git_cli = GitCli::new();

        let outcome = execute_worktree_cleanup(
            &worktree_repo,
            &git_cli,
            "p1",
            Some("ghost"),
            &wt,
            &repo_root,
            false,
        )
        .unwrap();
        // Row missing => AlreadyRemoved even though directory exists (it's not
        // a registered worktree).
        assert!(matches!(outcome, ExecuteOutcome::AlreadyRemoved));
    }

    #[test]
    fn execute_removes_live_worktree_and_is_idempotent_after() {
        let (_tmp, repo_root) = init_repo();
        let wt = repo_root.join("wt-live");
        GitCli::new()
            .create_worktree(&repo_root, &wt, "feat/live", None)
            .unwrap();

        let db_dir = tempfile::tempdir().unwrap();
        let mut db = seeded_db(db_dir.path());
        db.connection_mut()
            .execute(
                "INSERT INTO worktrees (id, project_id, task_id, normalized_path, git_common_dir, branch, head, bound_at, updated_at)
                 VALUES ('wt1', 'p1', NULL, ?1, '', 'feat/live', 'abc', 'now', 'now')",
                rusqlite::params![wt.to_string_lossy().to_string()],
            )
            .unwrap();

        let conn = db.connection_mut();
        let worktree_repo = SqliteWorktreeRepository::new(conn);
        let git_cli = GitCli::new();

        let r1 = execute_worktree_cleanup(
            &worktree_repo,
            &git_cli,
            "p1",
            Some("wt1"),
            &wt,
            &repo_root,
            false,
        )
        .unwrap();
        assert!(matches!(r1, ExecuteOutcome::Removed));
        assert!(!wt.exists());

        let r2 = execute_worktree_cleanup(
            &worktree_repo,
            &git_cli,
            "p1",
            Some("wt1"),
            &wt,
            &repo_root,
            false,
        )
        .unwrap();
        assert!(matches!(r2, ExecuteOutcome::AlreadyRemoved));
    }

    #[test]
    fn execute_maps_locked_to_blocked_not_failed() {
        let (_tmp, repo_root) = init_repo();
        let wt = repo_root.join("wt-locked-exec");
        GitCli::new()
            .create_worktree(&repo_root, &wt, "feat/locked-exec", None)
            .unwrap();
        let mut cmd = StdCommand::new("git");
        cmd.args(["worktree", "lock", "--reason", "hold"])
            .arg(&wt)
            .current_dir(&repo_root);
        for v in GIT_STATE_VARS {
            cmd.env_remove(v);
        }
        cmd.output().expect("lock");

        let db_dir = tempfile::tempdir().unwrap();
        let mut db = seeded_db(db_dir.path());
        db.connection_mut()
            .execute(
                "INSERT INTO worktrees (id, project_id, task_id, normalized_path, git_common_dir, branch, head, bound_at, updated_at)
                 VALUES ('wt1', 'p1', NULL, ?1, '', 'feat/locked-exec', 'abc', 'now', 'now')",
                rusqlite::params![wt.to_string_lossy().to_string()],
            )
            .unwrap();

        let conn = db.connection_mut();
        let worktree_repo = SqliteWorktreeRepository::new(conn);
        let git_cli = GitCli::new();

        let outcome = execute_worktree_cleanup(
            &worktree_repo,
            &git_cli,
            "p1",
            Some("wt1"),
            &wt,
            &repo_root,
            false,
        )
        .unwrap();

        match outcome {
            ExecuteOutcome::Blocked(a) => {
                assert!(a.blockers.contains(&CleanupBlocker::WorktreeLocked));
            }
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn execute_maps_dirty_to_blocked_not_failed() {
        let (_tmp, repo_root) = init_repo();
        let wt = repo_root.join("wt-dirty-exec");
        GitCli::new()
            .create_worktree(&repo_root, &wt, "feat/dirty-exec", None)
            .unwrap();
        std::fs::write(wt.join("untracked.txt"), "x\n").unwrap();

        let db_dir = tempfile::tempdir().unwrap();
        let mut db = seeded_db(db_dir.path());
        db.connection_mut()
            .execute(
                "INSERT INTO worktrees (id, project_id, task_id, normalized_path, git_common_dir, branch, head, bound_at, updated_at)
                 VALUES ('wt1', 'p1', NULL, ?1, '', 'feat/dirty-exec', 'abc', 'now', 'now')",
                rusqlite::params![wt.to_string_lossy().to_string()],
            )
            .unwrap();

        let conn = db.connection_mut();
        let worktree_repo = SqliteWorktreeRepository::new(conn);
        let git_cli = GitCli::new();

        let outcome = execute_worktree_cleanup(
            &worktree_repo,
            &git_cli,
            "p1",
            Some("wt1"),
            &wt,
            &repo_root,
            false,
        )
        .unwrap();

        match outcome {
            ExecuteOutcome::Blocked(a) => {
                assert!(a.blockers.contains(&CleanupBlocker::DirtyWorktree));
            }
            other => panic!("expected Blocked for dirty, got {other:?}"),
        }
    }

    #[test]
    fn execute_treats_not_a_worktree_as_already_removed() {
        let (_tmp, repo_root) = init_repo();
        let foreign = repo_root.join("foreign");
        std::fs::create_dir_all(&foreign).unwrap();

        let db_dir = tempfile::tempdir().unwrap();
        let mut db = seeded_db(db_dir.path());
        db.connection_mut()
            .execute(
                "INSERT INTO worktrees (id, project_id, task_id, normalized_path, git_common_dir, branch, head, bound_at, updated_at)
                 VALUES ('wt1', 'p1', NULL, ?1, '', 'foreign', 'abc', 'now', 'now')",
                rusqlite::params![foreign.to_string_lossy().to_string()],
            )
            .unwrap();

        let conn = db.connection_mut();
        let worktree_repo = SqliteWorktreeRepository::new(conn);
        let git_cli = GitCli::new();

        // The directory exists but is not a git worktree — git reports
        // `is not a working tree`, which must map to AlreadyRemoved.
        let outcome = execute_worktree_cleanup(
            &worktree_repo,
            &git_cli,
            "p1",
            Some("wt1"),
            &foreign,
            &repo_root,
            false,
        )
        .unwrap();
        assert!(matches!(outcome, ExecuteOutcome::AlreadyRemoved));
    }

    #[test]
    fn assess_does_not_call_remove() {
        // Guard: assess must be pure. We verify by calling assess on a dirty
        // worktree and asserting the directory still exists afterwards.
        let (_tmp, repo_root) = init_repo();
        let wt = repo_root.join("wt-pure");
        GitCli::new()
            .create_worktree(&repo_root, &wt, "feat/pure", None)
            .unwrap();
        std::fs::write(wt.join("dirty.txt"), "x\n").unwrap();

        let db_dir = tempfile::tempdir().unwrap();
        let mut db = seeded_db(db_dir.path());
        db.connection_mut()
            .execute(
                "INSERT INTO worktrees (id, project_id, task_id, normalized_path, git_common_dir, branch, head, bound_at, updated_at)
                 VALUES ('wt1', 'p1', NULL, ?1, '', 'feat/pure', 'abc', 'now', 'now')",
                rusqlite::params![wt.to_string_lossy().to_string()],
            )
            .unwrap();

        let conn = db.connection_mut();
        let session_repo = SqliteSessionRepository::new(conn);
        let worktree_repo = SqliteWorktreeRepository::new(conn);
        let git_cli = GitCli::new();

        let assessment = assess_worktree_cleanup(
            &session_repo,
            &worktree_repo,
            &git_cli,
            "p1",
            Some("wt1"),
            &wt,
            &repo_root,
            Some(&PathBuf::from("/tmp")),
        )
        .unwrap();

        assert!(wt.exists(), "assess must not remove the worktree");
        assert!(assessment.blockers.contains(&CleanupBlocker::DirtyWorktree));
    }
}
