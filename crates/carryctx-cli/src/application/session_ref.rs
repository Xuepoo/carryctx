//! Session reference canonicalization (CTX-0148, CTX-0151).
//!
//! One resolver is shared by the pre-dispatch CLI canonicalization (normal
//! commands) and the direct-lock write paths (`import --mode merge`,
//! `conflict apply`) that bypass it. Keeping the implementation in the
//! application layer avoids the CLI dispatcher depending on handlers for a
//! resolution the application layer also needs.

use crate::adapter::sqlite_repos::SqliteSessionRepository;
use crate::error::CarryCtxError;
use crate::repository::SessionRepository;

/// Resolve a user-supplied session reference to its canonical full ULID.
///
/// Accepts an exact session ULID (returned unchanged) or a unique
/// case-insensitive ULID prefix. Unknown references fail with
/// `RESOURCE_NOT_FOUND`; a prefix matching more than one session fails with
/// `VALIDATION_FAILED` and the candidate list. Resolving here keeps short refs
/// out of foreign-key columns (`checkpoints.session_id`,
/// `handoffs.session_id`, `decisions.session_id`) and out of the raw
/// `progress_items.source_session_id` text column, where they previously
/// persisted silently or crashed with `FOREIGN KEY constraint failed`.
pub fn resolve_session_ref(
    project_id: &str,
    session_ref: &str,
    conn: &rusqlite::Connection,
) -> Result<String, CarryCtxError> {
    let reference = session_ref.trim();
    if reference.is_empty() {
        return Err(CarryCtxError::validation_error(
            "Session reference cannot be empty.",
        ));
    }
    let repo = SqliteSessionRepository::new(conn);
    if let Some(session) = repo.find_by_id(project_id, reference)? {
        return Ok(session.id);
    }
    let upper = reference.to_ascii_uppercase();
    let candidates: Vec<String> = repo
        .list(project_id)?
        .into_iter()
        .map(|session| session.id)
        .filter(|id| id.to_ascii_uppercase().starts_with(&upper))
        .collect();
    match candidates.as_slice() {
        [id] => Ok(id.clone()),
        [] => Err(
            CarryCtxError::resource_not_found(format!("Session '{session_ref}' not found."))
                .with_suggestions(vec![
                    "Run `carryctx session list` to list session ULIDs.".to_string(),
                ]),
        ),
        _ => Err(CarryCtxError::validation_error(format!(
            "Session reference '{session_ref}' is ambiguous: it matches {} sessions ({}). Pass the full 26-character ULID.",
            candidates.len(),
            candidates.join(", ")
        ))),
    }
}
