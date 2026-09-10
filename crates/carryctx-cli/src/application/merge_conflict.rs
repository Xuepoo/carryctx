//! `conflict list/show/resolve/apply/abort` (design
//! `2026-09-10-mergeable-git-managed-state.md` §2.4–§2.6).
//!
//! CTX-0142 stages a conflicted merge under
//! `<state-dir>/merges/<merge_id>/` (`merge.json`, `conflicts.json`,
//! `candidate.sqlite`, `theirs/`) and leaves the live database untouched.
//! This module owns the resolution UX on top of that staging layout:
//!
//! - `list`/`show` read the session only (no database access);
//! - `resolve` appends a resolution record to `conflicts.json` atomically and
//!   never rewrites `candidate.sqlite`, so intent stays auditable and
//!   idempotent;
//! - `apply` materializes every resolution into the candidate in one
//!   transaction, appends the merge audit events in the same transaction,
//!   re-runs the CTX-0142 `validate_candidate` gate, and swaps the candidate
//!   into place through the same restore-journal path. A failure before the
//!   rename leaves the live database and staging intact; bookkeeping after the
//!   rename is deliberately infallible;
//! - `abort` deletes the staging directory with no database change.
//!
//! Envelope command names are `conflict.list|show|resolve|apply|abort`
//! (design §2.5). Missing sessions are `RESOURCE_NOT_FOUND` (7); an unknown
//! `--set` field is `VALIDATION_FAILED` (8); `apply` with open conflicts is
//! `MERGE_CONFLICTS` (3) unless `--skip-open`.

use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use carryctx_pack::merge::identity::{identity_columns, identity_key, machine_local_columns};
use serde_json::{Map, Value, json};

use crate::adapter::filesystem;
use crate::adapter::git::{GitCli, GitProject};
use crate::adapter::sqlite::ProjectDatabase;
use crate::adapter::sqlite_repos::{SqliteEventRepository, SqliteTombstoneRepository};
use crate::adapter::xdg::XdgPaths;
use crate::application::export::row_to_json;
use crate::application::import::{
    insert_row, is_known_table, json_to_sql, new_id, now, remove_sidecars, sibling_path,
};
use crate::application::merge_import::{
    active_merge_session, checkpoint_database, resolve_actor, resolve_session,
    swap_candidate_into_place, validate_candidate,
};
use crate::application::project_mgmt::delete_tasks_cascade;
use crate::error::CarryCtxError;
use crate::repository::event::{EventRepository, NewEvent};
use crate::repository::{Tombstone, TombstoneRepository};

/// One loaded merge session: its directory, `merge.json`, and the mutable
/// `conflicts.json` records (each carrying an optional `resolution`).
struct MergeSession {
    merge: Value,
    conflicts: Vec<Value>,
}

fn hostname() -> String {
    std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".into())
}

fn write_json(path: &Path, value: &Value) -> Result<(), CarryCtxError> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| CarryCtxError::io_error(format!("Failed to encode session JSON: {e}")))?;
    filesystem::write_atomic(path, text.as_bytes())
}

fn merge_id_of(merge: &Value) -> Result<String, CarryCtxError> {
    merge
        .get("mergeId")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            CarryCtxError::database_error(
                "Merge session merge.json is missing 'mergeId'.".to_string(),
            )
        })
}

/// Whether a conflict record still has no resolution (`resolution == null`).
fn is_open(conflict: &Value) -> bool {
    conflict
        .get("resolution")
        .map(Value::is_null)
        .unwrap_or(true)
}

/// Load a session directory, requiring both marker files (design §2.4: a
/// session is active only when `merge.json` and `conflicts.json` both exist).
fn load_session(dir: &Path) -> Result<MergeSession, CarryCtxError> {
    let merge_path = dir.join("merge.json");
    let conflicts_path = dir.join("conflicts.json");
    let merge: Value =
        serde_json::from_str(&fs::read_to_string(&merge_path).map_err(|_| session_not_found(dir))?)
            .map_err(|e| CarryCtxError::database_error(format!("Malformed merge.json: {e}")))?;
    let conflicts: Value = serde_json::from_str(
        &fs::read_to_string(&conflicts_path).map_err(|_| session_not_found(dir))?,
    )
    .map_err(|e| CarryCtxError::database_error(format!("Malformed conflicts.json: {e}")))?;
    let conflicts = conflicts.as_array().cloned().ok_or_else(|| {
        CarryCtxError::database_error("conflicts.json must be a JSON array.".to_string())
    })?;
    Ok(MergeSession { merge, conflicts })
}

fn session_not_found(dir: &Path) -> CarryCtxError {
    CarryCtxError::resource_not_found(format!(
        "Merge session '{}' was not found.",
        dir.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    ))
}

/// Session statuses that are no longer actionable (design §2.4: a session is
/// active only while it is neither applied nor aborted).
fn is_terminal_status(status: &str) -> bool {
    matches!(status, "applied" | "aborted")
}

fn merge_status(merge: &Value) -> Option<&str> {
    merge.get("status").and_then(Value::as_str)
}

/// Resolve the session directory: an explicit `--merge <id>`, else the single
/// active session; none (or a terminal session) yields `RESOURCE_NOT_FOUND`
/// (exit 7). `--merge` respects status, so a finished session cannot be
/// re-applied or aborted.
fn resolve_session_dir(
    gp: &GitProject,
    xdg: &XdgPaths,
    merge: Option<&str>,
) -> Result<PathBuf, CarryCtxError> {
    let merges_dir = xdg.merges_dir(&gp.git_common_dir);
    match merge.map(str::trim).filter(|id| !id.is_empty()) {
        Some(id) => {
            let dir = merges_dir.join(id);
            if !dir.join("merge.json").is_file() || !dir.join("conflicts.json").is_file() {
                return Err(CarryCtxError::resource_not_found(format!(
                    "Merge session '{id}' was not found."
                )));
            }
            let status = fs::read_to_string(dir.join("merge.json"))
                .ok()
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                .and_then(|value| merge_status(&value).map(str::to_string));
            if status.as_deref().is_some_and(is_terminal_status) {
                return Err(CarryCtxError::resource_not_found(format!(
                    "Merge session '{id}' is already {}; no active merge session.",
                    status.unwrap_or_default()
                )));
            }
            Ok(dir)
        }
        None => match active_merge_session(&merges_dir)? {
            Some(id) => Ok(merges_dir.join(id)),
            None => Err(CarryCtxError::resource_not_found(
                "No active merge session; stage one with `carryctx import --mode merge`.",
            )),
        },
    }
}

/// Fail closed on a `conflict.table` outside the interchange table set before
/// any dynamic SQL identifier is built (a hand-edited `conflicts.json` must
/// never drive SQL).
fn ensure_known_table(table: &str) -> Result<(), CarryCtxError> {
    if is_known_table(table) && table != "tombstones" {
        Ok(())
    } else {
        Err(CarryCtxError::validation_error(format!(
            "Conflict table '{table}' is not a known interchange table."
        )))
    }
}

// ── list / show ──────────────────────────────────────────────────────────

/// `conflict list [--all] [--merge <id>]`: open conflicts by default; `--all`
/// also includes resolved conflicts and the staged auto-resolutions.
pub fn list_conflicts(
    gp: &GitProject,
    xdg: &XdgPaths,
    merge: Option<&str>,
    all: bool,
) -> Result<Value, CarryCtxError> {
    let dir = resolve_session_dir(gp, xdg, merge)?;
    let session = load_session(&dir)?;
    let mut open = 0u64;
    let mut resolved = 0u64;
    let mut conflicts = Vec::new();
    for conflict in &session.conflicts {
        if is_open(conflict) {
            open += 1;
        } else {
            resolved += 1;
        }
        if all || is_open(conflict) {
            conflicts.push(json!({
                "id": conflict.get("id").cloned().unwrap_or(Value::Null),
                "kind": conflict.get("kind").cloned().unwrap_or(Value::Null),
                "table": conflict.get("table").cloned().unwrap_or(Value::Null),
                "key": conflict.get("key").cloned().unwrap_or(Value::Null),
                "reason": conflict.get("reason").cloned().unwrap_or(Value::Null),
                "resolution": conflict.get("resolution").cloned().unwrap_or(Value::Null),
            }));
        }
    }
    let auto_resolutions = if all {
        session
            .merge
            .get("autoResolutions")
            .cloned()
            .unwrap_or_else(|| json!([]))
    } else {
        json!([])
    };
    Ok(json!({
        "mergeId": session.merge.get("mergeId").cloned().unwrap_or(Value::Null),
        "status": session.merge.get("status").cloned().unwrap_or(Value::Null),
        "degraded": session.merge.get("degraded").cloned().unwrap_or(Value::Null),
        "baseSource": session.merge.get("baseSource").cloned().unwrap_or(Value::Null),
        "counts": session.merge.get("counts").cloned().unwrap_or_else(|| json!({})),
        "open": open,
        "resolved": resolved,
        "conflicts": conflicts,
        "autoResolutions": auto_resolutions,
    }))
}

/// `conflict show <conflict-id>`: the full base/ours/theirs records plus the
/// policy `reason` and current `resolution`. Unknown ids are
/// `RESOURCE_NOT_FOUND` (exit 7).
pub fn show_conflict(
    gp: &GitProject,
    xdg: &XdgPaths,
    conflict_id: &str,
    merge: Option<&str>,
) -> Result<Value, CarryCtxError> {
    let dir = resolve_session_dir(gp, xdg, merge)?;
    let session = load_session(&dir)?;
    let conflict = session
        .conflicts
        .iter()
        .find(|conflict| conflict.get("id").and_then(Value::as_str) == Some(conflict_id))
        .ok_or_else(|| {
            CarryCtxError::resource_not_found(format!(
                "Conflict '{conflict_id}' was not found in merge session {}.",
                session
                    .merge
                    .get("mergeId")
                    .and_then(Value::as_str)
                    .unwrap_or("<unknown>")
            ))
        })?;
    Ok(json!({
        "mergeId": session.merge.get("mergeId").cloned().unwrap_or(Value::Null),
        "status": session.merge.get("status").cloned().unwrap_or(Value::Null),
        "conflict": conflict.clone(),
    }))
}

// ── resolve ──────────────────────────────────────────────────────────────

/// `conflict resolve <conflict-id> --ours|--theirs [--set field=value]...`.
///
/// Writes the resolution record into `conflicts.json` atomically; the
/// candidate database is never touched here (design §2.5).
pub fn resolve_conflict(
    gp: &GitProject,
    xdg: &XdgPaths,
    merge: Option<&str>,
    conflict_id: &str,
    choice: &str,
    sets: &[String],
    actor: Option<&str>,
    dry_run: bool,
) -> Result<Value, CarryCtxError> {
    // A dry run validates the same way a real resolve would, but takes no lock
    // and never writes (design §2.6: a dry run writes nothing).
    if dry_run {
        let dir = resolve_session_dir(gp, xdg, merge)?;
        let session = load_session(&dir)?;
        let (index, chosen) = find_conflict(&session, conflict_id, choice)?;
        parse_overrides(&chosen, conflict_id, sets)?;
        let _ = index;
        let open = session.conflicts.iter().filter(|c| is_open(c)).count() as u64;
        let resolved = session.conflicts.len() as u64 - open;
        return Ok(json!({
            "mergeId": session.merge.get("mergeId").cloned().unwrap_or(Value::Null),
            "conflictId": conflict_id,
            "choice": choice,
            "resolvedCount": resolved,
            "openCount": open,
            "warnings": ["[dry-run] No resolution was recorded."],
            "operation": {"applied": false},
        }));
    }

    let _lock = filesystem::AdmissionLock::acquire(
        &xdg.admission_lock_dir(&gp.git_common_dir),
        &new_id(),
        std::process::id(),
        &hostname(),
        &now(),
    )?;
    let dir = resolve_session_dir(gp, xdg, merge)?;
    let mut session = load_session(&dir)?;
    let (index, chosen) = find_conflict(&session, conflict_id, choice)?;
    let fields = parse_overrides(&chosen, conflict_id, sets)?;
    let resolved_by = actor
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("unknown");
    if let Some(conflict) = session.conflicts.get_mut(index) {
        conflict["resolution"] = json!({
            "choice": choice,
            "fields": fields,
            "resolvedAt": now(),
            "resolvedBy": resolved_by,
        });
    }
    write_json(
        &dir.join("conflicts.json"),
        &Value::Array(session.conflicts.clone()),
    )?;
    let open = session.conflicts.iter().filter(|c| is_open(c)).count() as u64;
    let resolved = session.conflicts.len() as u64 - open;
    Ok(json!({
        "mergeId": session.merge.get("mergeId").cloned().unwrap_or(Value::Null),
        "conflictId": conflict_id,
        "choice": choice,
        "resolvedCount": resolved,
        "openCount": open,
        "operation": {"applied": true},
    }))
}

/// Locate a conflict by id, returning its index and the row the `choice` names.
fn find_conflict(
    session: &MergeSession,
    conflict_id: &str,
    choice: &str,
) -> Result<(usize, Value), CarryCtxError> {
    let index = session
        .conflicts
        .iter()
        .position(|conflict| conflict.get("id").and_then(Value::as_str) == Some(conflict_id))
        .ok_or_else(|| {
            CarryCtxError::resource_not_found(format!("Conflict '{conflict_id}' was not found."))
        })?;
    let chosen = session.conflicts[index]
        .get(choice)
        .cloned()
        .unwrap_or(Value::Null);
    Ok((index, chosen))
}

/// Parse repeatable `--set field=value` overrides against the chosen row
/// shape. v1 only accepts JSON-object rows; values are parsed as JSON when
/// possible, else kept as strings. Unknown fields are `VALIDATION_FAILED` (8).
fn parse_overrides(
    chosen: &Value,
    conflict_id: &str,
    sets: &[String],
) -> Result<Map<String, Value>, CarryCtxError> {
    let mut fields = Map::new();
    if sets.is_empty() {
        return Ok(fields);
    }
    let object = chosen.as_object().ok_or_else(|| {
        CarryCtxError::validation_error(format!(
            "Conflict '{conflict_id}' cannot accept --set overrides because the chosen side is a deletion."
        ))
    })?;
    for raw in sets {
        let (field, value) = raw.split_once('=').ok_or_else(|| {
            CarryCtxError::validation_error(format!("--set '{raw}' must be FIELD=VALUE."))
        })?;
        let field = field.trim();
        if field.is_empty() {
            return Err(CarryCtxError::validation_error(format!(
                "--set '{raw}' has an empty field name."
            )));
        }
        if !object.contains_key(field) {
            return Err(CarryCtxError::validation_error(format!(
                "Unknown field '{field}' for conflict '{conflict_id}'; the chosen row has: {}.",
                object.keys().cloned().collect::<Vec<_>>().join(", ")
            )));
        }
        let parsed =
            serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.to_string()));
        fields.insert(field.to_string(), parsed);
    }
    Ok(fields)
}

// ── apply ────────────────────────────────────────────────────────────────

/// `conflict apply [--merge <id>] [--skip-open] [--dry-run]`.
///
/// Refuses with `MERGE_CONFLICTS` (exit 3) while any conflict is open unless
/// `--skip-open`; otherwise materializes every resolution into a fresh copy of
/// the staged candidate, appends the audit events, validates, and atomically
/// swaps it into place.
///
/// Retry safety: the staged `candidate.sqlite` is never mutated in place.
/// Every attempt rebuilds the apply image from it, so a crash after the
/// candidate commit but before the rename leaves nothing to double-append; a
/// live-database idempotency check refuses an already-applied merge.
#[allow(clippy::too_many_arguments)]
pub fn apply_conflicts(
    gp: &GitProject,
    xdg: &XdgPaths,
    db_path: &Path,
    merge: Option<&str>,
    skip_open: bool,
    snapshot_ref: Option<&str>,
    dry_run: bool,
    actor: Option<&str>,
    session_id: Option<&str>,
) -> Result<Value, CarryCtxError> {
    // Fail closed on a bad snapshot ref before any write, dry run included.
    if let Some(git_ref) = snapshot_ref {
        crate::application::export::validate_snapshot_ref(&gp.repository_root, git_ref)?;
    }
    // Read-only preflight so `--dry-run` never creates even the transient
    // admission lock (design §2.6: a dry run writes nothing).
    let preflight_dir = resolve_session_dir(gp, xdg, merge)?;
    let preflight = load_session(&preflight_dir)?;
    let preflight_id = merge_id_of(&preflight.merge)?;
    let mut preflight_warnings = staged_warnings(&preflight.merge);
    let open = preflight.conflicts.iter().filter(|c| is_open(c)).count() as u64;
    let would_apply = open == 0 || skip_open;
    if skip_open && open > 0 {
        preflight_warnings.push(skip_open_warning(open));
    }
    let preflight_apply = conflicts_to_apply(&preflight.conflicts, skip_open)?;
    let resolved_count = preflight_apply.len() as u64;

    // `--dry-run` reports what apply would do without writing anything; a
    // real apply refuses while open conflicts remain (design §2.5–§2.6).
    if dry_run {
        if !would_apply {
            preflight_warnings.push(blocking_conflicts_warning(open));
        }
        let mut data = json!({
            "mergeId": preflight_id,
            "applied": false,
            "wouldApply": would_apply,
            "resolvedCount": resolved_count,
            "openConflicts": open,
            "skipOpen": skip_open,
            "counts": preflight.merge.get("counts").cloned().unwrap_or_else(|| json!({})),
            "warnings": preflight_warnings,
            "operation": {"applied": false},
        });
        if let Some(git_ref) = snapshot_ref {
            data["snapshot"] = json!({"ref": git_ref, "wouldCommit": true});
        }
        return Ok(data);
    }
    if !would_apply {
        return Err(merge_conflicts_error(&preflight_id, open));
    }

    // Everything below mutates state: take the admission lock, then re-read
    // the session so a concurrent resolve/abort between the preflight and the
    // lock cannot be missed.
    let _lock = filesystem::AdmissionLock::acquire(
        &xdg.admission_lock_dir(&gp.git_common_dir),
        &new_id(),
        std::process::id(),
        &hostname(),
        &now(),
    )?;
    let dir = resolve_session_dir(gp, xdg, merge)?;
    let session = load_session(&dir)?;
    let merge_id = merge_id_of(&session.merge)?;
    let open = session.conflicts.iter().filter(|c| is_open(c)).count() as u64;
    if open > 0 && !skip_open {
        return Err(merge_conflicts_error(&merge_id, open));
    }
    let mut warnings = staged_warnings(&session.merge);
    if skip_open && open > 0 {
        warnings.push(skip_open_warning(open));
    }
    let to_apply = conflicts_to_apply(&session.conflicts, skip_open)?;
    let resolved_count = to_apply.len() as u64;
    let project_id = session
        .merge
        .get("projectId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let counts = session
        .merge
        .get("counts")
        .cloned()
        .unwrap_or_else(|| json!({}));
    // Capture the incoming snapshot commit now: the session directory is
    // removed after a successful swap, but `--snapshot-ref` is written after
    // that (CTX-0145).
    let incoming_commit = session
        .merge
        .get("sourceCommit")
        .and_then(Value::as_str)
        .map(str::to_string);
    let incoming_export_id = session
        .merge
        .get("sourceExportId")
        .and_then(Value::as_str)
        .map(str::to_string);
    let staged_candidate = dir.join("candidate.sqlite");

    // Resolve the actor to a ULID against the live database: audit events
    // carry an FK, so a raw name would fail the candidate closed. The session
    // ref goes through the same shared resolution (CTX-0151): `conflict apply`
    // is a direct-lock command, so `--session` never saw the pre-dispatch
    // canonicalization and a short prefix would otherwise land raw in
    // `events.session_id`. The same connection answers the idempotency check
    // that survives a crash after the rename (when the session status write
    // did not happen).
    let (actor_id, session_id) = {
        let live = ProjectDatabase::open_readonly(db_path)?;
        if live_merge_already_applied(live.connection(), &merge_id)? {
            return Err(CarryCtxError::state_conflict(format!(
                "Merge session {merge_id} has already been applied to the live database."
            ))
            .with_details(json!({ "mergeId": merge_id })));
        }
        let actor = resolve_actor(live.connection(), actor)?;
        let session = resolve_session(live.connection(), &project_id, session_id)?;
        (actor, session)
    };

    // Build the apply image on a fresh throwaway copy at the trusted sibling
    // name the restore journal accepts; the staged candidate stays untouched
    // so an interrupted apply can be retried without double-appending events.
    let swap_candidate = sibling_path(db_path, &format!("restore_{merge_id}"));
    remove_candidate_artifacts(&swap_candidate);
    if let Err(error) = fs::copy(&staged_candidate, &swap_candidate) {
        remove_candidate_artifacts(&swap_candidate);
        return Err(CarryCtxError::database_error(format!(
            "Failed to stage merge candidate for swap: {error}"
        )));
    }
    remove_sidecars(&swap_candidate);

    let build = (|| -> Result<(), CarryCtxError> {
        let candidate = ProjectDatabase::open(&swap_candidate)?;
        let tx = candidate
            .connection()
            .unchecked_transaction()
            .map_err(|e| {
                CarryCtxError::database_error(format!("Candidate transaction failed: {e}"))
            })?;
        for conflict in &to_apply {
            apply_resolution(&tx, &project_id, conflict)?;
        }
        append_conflict_events(
            &tx,
            &session.merge,
            &merge_id,
            &project_id,
            &to_apply,
            skip_open,
            actor_id,
            session_id,
        )?;
        tx.commit()
            .map_err(|e| CarryCtxError::database_error(format!("Candidate commit failed: {e}")))?;
        Ok(())
    })();
    if let Err(error) = build {
        remove_candidate_artifacts(&swap_candidate);
        return Err(error);
    }
    if let Err(error) = checkpoint_database(&swap_candidate) {
        remove_candidate_artifacts(&swap_candidate);
        return Err(error);
    }
    if let Err(error) = validate_candidate(&swap_candidate) {
        remove_candidate_artifacts(&swap_candidate);
        return Err(error);
    }

    // The merge is durable once the rename lands. A crash before it leaves
    // the live database and the staged session intact; a crash after it is
    // caught by the live-database idempotency check above.
    let pre_merge_backup_path =
        match swap_candidate_into_place(db_path, &swap_candidate, xdg, gp, &merge_id) {
            Ok(path) => path,
            Err(error) => {
                remove_candidate_artifacts(&swap_candidate);
                return Err(error);
            }
        };

    // Post-commit bookkeeping is deliberately best-effort: the candidate is
    // already the live database, so no failure here may surface as an error
    // (design AC9).
    mark_session_applied(&dir);
    remove_candidate_artifacts(&swap_candidate);
    let _ = fs::remove_dir_all(&dir);

    // CTX-0145: the swap above is the commit point. Write the two-parent merge
    // snapshot only now; a snapshot failure is its own error and never rolls
    // the durable merge back.
    let mut snapshot_data = None;
    if let Some(git_ref) = snapshot_ref {
        match (incoming_commit.as_deref(), incoming_export_id.as_deref()) {
            (Some(incoming_commit), Some(incoming_export_id)) => {
                let git = GitCli::new();
                match crate::application::merge_snapshot::local_snapshot_tip(&git, gp, git_ref)? {
                    Some((local_commit, local_export_id)) => {
                        snapshot_data =
                            Some(crate::application::merge_snapshot::write_merge_snapshot_commit(
                                db_path,
                                gp,
                                git_ref,
                                (&local_commit, &local_export_id),
                                (incoming_commit, incoming_export_id),
                            )?);
                    }
                    None => warnings.push(format!(
                        "Local snapshot ref '{git_ref}' has no tip to use as the first parent; skipping the merge snapshot. Run `carryctx export --snapshot` first."
                    )),
                }
            }
            _ => warnings.push(format!(
                "The staged merge session records no incoming snapshot commit; skipping the merge snapshot on '{git_ref}'. Run `carryctx export --snapshot` to commit the merged state."
            )),
        }
    }

    let mut data = json!({
        "mergeId": merge_id,
        "applied": true,
        "resolvedCount": resolved_count,
        "conflicts": 0,
        "counts": counts,
        "path": db_path.to_string_lossy(),
        "preMergeBackupPath": pre_merge_backup_path.to_string_lossy(),
        "warnings": warnings,
        "operation": {"applied": true},
    });
    if let Some(snapshot) = snapshot_data {
        data["snapshot"] = snapshot;
    }
    Ok(data)
}

/// The conflicts apply must materialize: every resolved conflict, plus — when
/// `--skip-open` settles them — a synthetic `ours` resolution for each still
/// open conflict, so `--skip-open` genuinely leaves the candidate at "ours"
/// (dropping the incoming side) instead of only warning about it.
fn conflicts_to_apply(conflicts: &[Value], skip_open: bool) -> Result<Vec<Value>, CarryCtxError> {
    let mut to_apply = Vec::new();
    for conflict in conflicts {
        if is_open(conflict) {
            if !skip_open {
                continue;
            }
            let mut settled = conflict.clone();
            settled["resolution"] = json!({
                "choice": "ours",
                "fields": {},
                "resolvedAt": now(),
                "resolvedBy": "skip-open",
            });
            to_apply.push(settled);
        } else {
            to_apply.push(conflict.clone());
        }
    }
    Ok(to_apply)
}

/// Whether the live database already records a `project.merged` for `merge_id`
/// (durable idempotency marker for a crash after the atomic rename).
fn live_merge_already_applied(
    conn: &rusqlite::Connection,
    merge_id: &str,
) -> Result<bool, CarryCtxError> {
    let pattern = format!("%\"mergeId\":\"{merge_id}\"%");
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM events WHERE type = 'project.merged' AND payload_json LIKE ?1",
            [pattern],
            |row| row.get(0),
        )
        .map_err(|e| {
            CarryCtxError::database_error(format!("Failed to check merge idempotency: {e}"))
        })?;
    Ok(count > 0)
}

/// Remove a partially staged swap candidate and its WAL sidecars; a failure
/// is ignored (the caller is already on an error path).
fn remove_candidate_artifacts(path: &Path) {
    let _ = fs::remove_file(path);
    remove_sidecars(path);
}

/// Best-effort `status: applied` marker; the staging directory is normally
/// removed right after, but the marker makes an interrupted cleanup refuse a
/// re-apply.
fn mark_session_applied(dir: &Path) {
    let path = dir.join("merge.json");
    let Ok(raw) = fs::read_to_string(&path) else {
        return;
    };
    let Ok(mut merge) = serde_json::from_str::<Value>(&raw) else {
        return;
    };
    merge["status"] = json!("applied");
    let _ = write_json(&path, &merge);
}

fn merge_conflicts_error(merge_id: &str, conflicts: u64) -> CarryCtxError {
    CarryCtxError::merge_conflicts(
        format!(
            "Merge session {merge_id} still has {conflicts} open conflict(s); resolve them or pass --skip-open."
        ),
        merge_id,
        conflicts,
    )
    .with_suggestions([
        "Run `carryctx conflict list` to inspect open conflicts.".to_string(),
        "Pass `--skip-open` to settle every open conflict at the local (ours) value.".to_string(),
    ])
}

fn staged_warnings(merge: &Value) -> Vec<String> {
    merge
        .get("warnings")
        .and_then(Value::as_array)
        .map(|warnings| {
            warnings
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn skip_open_warning(count: u64) -> String {
    format!("Skipped {count} open conflict(s); each was left at the local (ours) value.")
}

fn blocking_conflicts_warning(count: u64) -> String {
    format!("{count} open conflict(s) would block apply; resolve them or pass --skip-open.")
}

/// Materialize one resolution into the candidate database.
///
/// A chosen object writes that row (preserving the candidate's machine-local
/// columns); a null chosen row means the chosen side deleted the row, so the
/// candidate row is removed and a tombstone ensured. Resurrecting a row drops
/// any tombstone the candidate carried for it.
///
/// `unique_key` conflicts are keyed by the semantic unique key rather than a
/// row identity, so the chosen row is written and every row sharing that
/// semantic key is deleted and tombstoned in the same transaction — otherwise
/// the candidate would keep the losing row and fail `UNIQUE`.
fn apply_resolution(
    conn: &rusqlite::Connection,
    project_id: &str,
    conflict: &Value,
) -> Result<(), CarryCtxError> {
    let table = conflict
        .get("table")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            CarryCtxError::database_error("Conflict record has no table.".to_string())
        })?;
    ensure_known_table(table)?;
    let kind = conflict
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let key = conflict
        .get("key")
        .and_then(Value::as_str)
        .ok_or_else(|| CarryCtxError::database_error("Conflict record has no key.".to_string()))?;
    let resolution = conflict
        .get("resolution")
        .filter(|value| !value.is_null())
        .ok_or_else(|| {
            CarryCtxError::database_error(format!(
                "Conflict '{key}' has no resolution record to apply."
            ))
        })?;
    let choice = resolution
        .get("choice")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            CarryCtxError::database_error(format!("Conflict '{key}' resolution has no choice."))
        })?;
    if kind == "unique_key" {
        return apply_unique_key_resolution(
            conn, project_id, table, key, conflict, choice, resolution,
        );
    }
    let chosen = conflict.get(choice).cloned().unwrap_or(Value::Null);
    if chosen.is_null() {
        if table == "tasks" {
            delete_tasks_cascade(
                conn,
                project_id,
                &[key.to_string()],
                &now(),
                "merge.conflict_resolved",
            )?;
        } else {
            delete_identity_row(conn, table, key)?;
            record_tombstone(conn, project_id, table, key)?;
        }
        return Ok(());
    }
    let mut row = chosen.as_object().cloned().ok_or_else(|| {
        CarryCtxError::validation_error(format!(
            "Conflict '{key}' chosen row for table '{table}' must be a JSON object."
        ))
    })?;
    // Machine-local columns never come from the incoming side (design §1.7):
    // keep whatever the candidate already carries for this row.
    if let Some(existing) = load_identity_row(conn, table, key)? {
        for column in machine_local_columns(table) {
            if let Some(value) = existing.get(*column) {
                row.insert((*column).to_string(), value.clone());
            }
        }
    }
    if let Some(fields) = resolution.get("fields").and_then(Value::as_object) {
        for (field, value) in fields {
            row.insert(field.clone(), value.clone());
        }
    }
    upsert_row(conn, table, &row)?;
    delete_tombstone(conn, project_id, table, key)?;
    Ok(())
}

/// Apply a `unique_key` collision: write the chosen row and drop every other
/// row that shares the semantic unique key (the losing `ours`/`theirs` rows),
/// tombstoning each so a later merge cannot resurrect it.
fn apply_unique_key_resolution(
    conn: &rusqlite::Connection,
    project_id: &str,
    table: &str,
    semantic_key: &str,
    conflict: &Value,
    choice: &str,
    resolution: &Value,
) -> Result<(), CarryCtxError> {
    let chosen = conflict
        .get(choice)
        .filter(|value| !value.is_null())
        .ok_or_else(|| {
            CarryCtxError::validation_error(format!(
                "Unique-key conflict '{semantic_key}' has no '{choice}' row to materialize."
            ))
        })?;
    let chosen_object = chosen.as_object().ok_or_else(|| {
        CarryCtxError::validation_error(format!(
            "Unique-key conflict '{semantic_key}' chosen row must be a JSON object."
        ))
    })?;
    let chosen_identity = identity_key(table, chosen_object)?;
    let semantic_columns = semantic_columns_from_key(conn, table, semantic_key)?;

    let mut row = chosen_object.clone();
    if let Some(existing) = load_identity_row(conn, table, &chosen_identity)? {
        if let Some(existing_object) = existing.as_object() {
            for column in machine_local_columns(table) {
                if let Some(value) = existing_object.get(*column) {
                    row.insert((*column).to_string(), value.clone());
                }
            }
        }
    }
    if let Some(fields) = resolution.get("fields").and_then(Value::as_object) {
        for (field, value) in fields {
            row.insert(field.clone(), value.clone());
        }
    }

    // Deletions must land BEFORE the chosen row is written: the survivor the
    // merge result kept can share the semantic unique key with the chosen row,
    // so an insert-first order would trip the UNIQUE constraint.
    //
    // Drop every candidate row sharing the semantic key that is not the chosen
    // identity, plus the non-chosen conflict side even if the merge result
    // already excluded it (so its ULID is tombstoned and cannot resurrect).
    let mut losers: BTreeSet<String> = BTreeSet::new();
    for candidate in load_all_rows(conn, table)? {
        let Some(candidate_object) = candidate.as_object() else {
            continue;
        };
        let candidate_identity = identity_key(table, candidate_object)?;
        if candidate_identity == chosen_identity {
            continue;
        }
        if semantic_values_match(candidate_object, chosen_object, &semantic_columns) {
            losers.insert(candidate_identity);
        }
    }
    let other_side = if choice == "ours" { "theirs" } else { "ours" };
    if let Some(other) = conflict.get(other_side).and_then(Value::as_object) {
        let other_identity = identity_key(table, other)?;
        if other_identity != chosen_identity {
            losers.insert(other_identity);
        }
    }
    for loser in losers {
        delete_identity_row(conn, table, &loser)?;
        record_tombstone(conn, project_id, table, &loser)?;
    }

    upsert_row(conn, table, &row)?;
    delete_tombstone(conn, project_id, table, &chosen_identity)?;
    Ok(())
}

/// Parse the semantic column list from a `unique_key` conflict key
/// (`"col_a,col_b|value_a\u{1}value_b"`) and validate each column against the
/// table schema before it can drive SQL.
fn semantic_columns_from_key(
    conn: &rusqlite::Connection,
    table: &str,
    semantic_key: &str,
) -> Result<Vec<String>, CarryCtxError> {
    let columns_part = semantic_key.split('|').next().unwrap_or_default();
    let columns: Vec<String> = columns_part
        .split(',')
        .map(str::trim)
        .filter(|column| !column.is_empty())
        .map(str::to_string)
        .collect();
    if columns.is_empty() {
        return Err(CarryCtxError::validation_error(format!(
            "Unique-key conflict for table '{table}' has no semantic columns."
        )));
    }
    let known: HashSet<String> = table_columns(conn, table)?.into_iter().collect();
    for column in &columns {
        if !known.contains(column) {
            return Err(CarryCtxError::validation_error(format!(
                "Unique-key conflict for table '{table}' names unknown column '{column}'."
            )));
        }
    }
    Ok(columns)
}

/// Whether two rows carry equal values for every semantic column.
fn semantic_values_match(
    candidate: &Map<String, Value>,
    chosen: &Map<String, Value>,
    columns: &[String],
) -> bool {
    columns
        .iter()
        .all(|column| candidate.get(column) == chosen.get(column))
}

/// Read every row of a table (used for semantic unique-key collision sweeps).
fn load_all_rows(conn: &rusqlite::Connection, table: &str) -> Result<Vec<Value>, CarryCtxError> {
    ensure_known_table(table)?;
    let sql = format!("SELECT * FROM \"{table}\"");
    let mut stmt = conn.prepare(&sql).map_err(|e| {
        CarryCtxError::database_error(format!("Failed to read table '{table}': {e}"))
    })?;
    let names: Vec<String> = stmt
        .column_names()
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    let rows = stmt
        .query_map([], |row| row_to_json(&names, row))
        .map_err(|e| {
            CarryCtxError::database_error(format!("Failed to read table '{table}': {e}"))
        })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| CarryCtxError::database_error(format!("Failed to read table '{table}': {e}")))
}

fn table_columns(conn: &rusqlite::Connection, table: &str) -> Result<Vec<String>, CarryCtxError> {
    ensure_known_table(table)?;
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info(\"{table}\")"))
        .map_err(|e| {
            CarryCtxError::database_error(format!("Failed to inspect table '{table}': {e}"))
        })?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| {
            CarryCtxError::database_error(format!("Failed to inspect table '{table}': {e}"))
        })?;
    let mut columns = Vec::new();
    for column in rows {
        columns.push(column.map_err(|e| {
            CarryCtxError::database_error(format!("Failed to inspect table '{table}': {e}"))
        })?);
    }
    if columns.is_empty() {
        return Err(CarryCtxError::database_error(format!(
            "Table '{table}' has no columns."
        )));
    }
    Ok(columns)
}

/// Build the identity `WHERE` clause and bound values for `table`/`key`.
fn identity_clause(table: &str, key: &str) -> Result<(String, Vec<String>), CarryCtxError> {
    ensure_known_table(table)?;
    let columns = identity_columns(table);
    if columns.len() == 1 {
        return Ok((format!("\"{}\" = ?1", columns[0]), vec![key.to_string()]));
    }
    let parts: Vec<String> = serde_json::from_str(key).map_err(|_| {
        CarryCtxError::validation_error(format!(
            "Conflict key '{key}' for table '{table}' is not a valid composite key."
        ))
    })?;
    if parts.len() != columns.len() {
        return Err(CarryCtxError::validation_error(format!(
            "Conflict key '{key}' for table '{table}' does not match its identity columns."
        )));
    }
    let clause = columns
        .iter()
        .enumerate()
        .map(|(index, column)| format!("\"{column}\" = ?{}", index + 1))
        .collect::<Vec<_>>()
        .join(" AND ");
    Ok((clause, parts))
}

fn load_identity_row(
    conn: &rusqlite::Connection,
    table: &str,
    key: &str,
) -> Result<Option<Value>, CarryCtxError> {
    let (where_clause, params) = identity_clause(table, key)?;
    let sql = format!("SELECT * FROM \"{table}\" WHERE {where_clause} LIMIT 1");
    let mut stmt = conn.prepare(&sql).map_err(|e| {
        CarryCtxError::database_error(format!("Failed to read table '{table}': {e}"))
    })?;
    let names: Vec<String> = stmt
        .column_names()
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    let mut rows = stmt
        .query(rusqlite::params_from_iter(params.iter()))
        .map_err(|e| {
            CarryCtxError::database_error(format!("Failed to read table '{table}': {e}"))
        })?;
    match rows.next().map_err(|e| {
        CarryCtxError::database_error(format!("Failed to read table '{table}': {e}"))
    })? {
        Some(row) => row_to_json(&names, row).map(Some).map_err(|e| {
            CarryCtxError::database_error(format!("Failed to read table '{table}': {e}"))
        }),
        None => Ok(None),
    }
}

/// Update the candidate row if it exists, otherwise insert it. An `UPDATE`
/// that matches no row falls through to `insert_row` (still parameter-bound).
fn upsert_row(
    conn: &rusqlite::Connection,
    table: &str,
    row: &Map<String, Value>,
) -> Result<(), CarryCtxError> {
    let columns = table_columns(conn, table)?;
    let known: HashSet<&str> = columns.iter().map(String::as_str).collect();
    for key in row.keys() {
        if !known.contains(key.as_str()) {
            return Err(CarryCtxError::validation_error(format!(
                "Conflict row for table '{table}' has unknown column '{key}'."
            )));
        }
    }
    let mut set_columns: Vec<String> = Vec::new();
    let mut params: Vec<rusqlite::types::Value> = Vec::new();
    for column in &columns {
        if let Some(value) = row.get(column) {
            params.push(json_to_sql(value)?);
            set_columns.push(format!("\"{column}\" = ?{}", params.len()));
        }
    }
    let mut where_parts: Vec<String> = Vec::new();
    for column in identity_columns(table) {
        let value = row.get(*column).ok_or_else(|| {
            CarryCtxError::validation_error(format!(
                "Conflict row for table '{table}' is missing identity column '{column}'."
            ))
        })?;
        params.push(json_to_sql(value)?);
        where_parts.push(format!("\"{column}\" = ?{}", params.len()));
    }
    let sql = format!(
        "UPDATE \"{table}\" SET {} WHERE {}",
        set_columns.join(", "),
        where_parts.join(" AND ")
    );
    let changed = conn
        .execute(&sql, rusqlite::params_from_iter(params.iter()))
        .map_err(|e| {
            CarryCtxError::database_error(format!(
                "Failed to apply conflict resolution to '{table}': {e}"
            ))
        })?;
    if changed == 0 {
        insert_row(conn, table, &Value::Object(row.clone()))?;
    }
    Ok(())
}

fn delete_identity_row(
    conn: &rusqlite::Connection,
    table: &str,
    key: &str,
) -> Result<(), CarryCtxError> {
    let (where_clause, params) = identity_clause(table, key)?;
    let sql = format!("DELETE FROM \"{table}\" WHERE {where_clause}");
    conn.execute(&sql, rusqlite::params_from_iter(params.iter()))
        .map_err(|e| {
            CarryCtxError::database_error(format!(
                "Failed to delete resolved row from '{table}': {e}"
            ))
        })?;
    Ok(())
}

fn record_tombstone(
    conn: &rusqlite::Connection,
    project_id: &str,
    table: &str,
    key: &str,
) -> Result<(), CarryCtxError> {
    ensure_known_table(table)?;
    SqliteTombstoneRepository::new(conn)
        .record(&Tombstone {
            project_id: project_id.to_string(),
            table_name: table.to_string(),
            row_id: key.to_string(),
            deleted_at: now(),
            deleted_by: None,
            reason: Some("conflict resolution: deleted side selected".to_string()),
        })
        .map_err(|e| {
            CarryCtxError::database_error(format!("Failed to record conflict tombstone: {e}"))
        })
}

fn delete_tombstone(
    conn: &rusqlite::Connection,
    project_id: &str,
    table: &str,
    key: &str,
) -> Result<(), CarryCtxError> {
    conn.execute(
        "DELETE FROM tombstones WHERE project_id = ?1 AND table_name = ?2 AND row_id = ?3",
        rusqlite::params![project_id, table, key],
    )
    .map_err(|e| {
        CarryCtxError::database_error(format!("Failed to clear conflict tombstone: {e}"))
    })?;
    Ok(())
}

/// Append exactly one `project.merged` plus one `merge.conflict_resolved` per
/// resolved conflict, keeping the CTX-0142 `merge.auto_resolved` /
/// `merge.display_id_renumbered` events byte-shape compatible.
#[allow(clippy::too_many_arguments)]
fn append_conflict_events(
    conn: &rusqlite::Connection,
    merge: &Value,
    merge_id: &str,
    project_id: &str,
    resolved: &[Value],
    skip_open: bool,
    actor_agent_id: Option<String>,
    session_id: Option<String>,
) -> Result<(), CarryCtxError> {
    let events = SqliteEventRepository::new(conn);
    let auto_resolutions = merge
        .get("autoResolutions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    events
        .append(&NewEvent {
            id: new_id(),
            project_id: project_id.to_string(),
            event_type: "project.merged".into(),
            actor_agent_id: actor_agent_id.clone(),
            session_id: session_id.clone(),
            task_id: None,
            payload: json!({
                "mergeId": merge_id,
                "base": merge.get("baseExportId").cloned().unwrap_or(Value::Null),
                "baseSource": merge.get("baseSource").cloned().unwrap_or(Value::Null),
                "degraded": merge.get("degraded").cloned().unwrap_or(Value::Null),
                "counts": merge.get("counts").cloned().unwrap_or_else(|| json!({})),
                "autoResolutionCount": auto_resolutions.len(),
                "conflictCount": merge.get("conflictCount").cloned().unwrap_or_else(|| json!(0)),
                "resolvedCount": resolved.len(),
                "skipOpen": skip_open,
            }),
            occurred_at: now(),
        })
        .map_err(|e| {
            CarryCtxError::database_error(format!("Failed to append project.merged event: {e}"))
        })?;

    for resolution in &auto_resolutions {
        events
            .append(&NewEvent {
                id: new_id(),
                project_id: project_id.to_string(),
                event_type: "merge.auto_resolved".into(),
                actor_agent_id: actor_agent_id.clone(),
                session_id: session_id.clone(),
                task_id: None,
                payload: json!({
                    "mergeId": merge_id,
                    "table": resolution.get("table").cloned().unwrap_or(Value::Null),
                    "key": resolution.get("key").cloned().unwrap_or(Value::Null),
                    "kind": resolution.get("kind").cloned().unwrap_or(Value::Null),
                    "winner": resolution.get("winner").cloned().unwrap_or(Value::Null),
                    "reason": resolution.get("reason").cloned().unwrap_or(Value::Null),
                }),
                occurred_at: now(),
            })
            .map_err(|e| {
                CarryCtxError::database_error(format!(
                    "Failed to append merge.auto_resolved event: {e}"
                ))
            })?;
    }

    if let Some(renumbers) = merge.get("renumbers").and_then(Value::as_array) {
        for renumber in renumbers {
            events
                .append(&NewEvent {
                    id: new_id(),
                    project_id: project_id.to_string(),
                    event_type: "merge.display_id_renumbered".into(),
                    actor_agent_id: actor_agent_id.clone(),
                    session_id: session_id.clone(),
                    task_id: None,
                    payload: json!({
                        "mergeId": merge_id,
                        "table": renumber.get("table").cloned().unwrap_or(Value::Null),
                        "rowId": renumber.get("rowId").cloned().unwrap_or(Value::Null),
                        "displayId": renumber.get("displayId").cloned().unwrap_or(Value::Null),
                        "newDisplayId": renumber.get("newDisplayId").cloned().unwrap_or(Value::Null),
                        "reason": renumber.get("reason").cloned().unwrap_or(Value::Null),
                    }),
                    occurred_at: now(),
                })
                .map_err(|e| {
                    CarryCtxError::database_error(format!(
                        "Failed to append merge.display_id_renumbered event: {e}"
                    ))
                })?;
        }
    }

    for conflict in resolved {
        let resolution = conflict.get("resolution");
        events
            .append(&NewEvent {
                id: new_id(),
                project_id: project_id.to_string(),
                event_type: "merge.conflict_resolved".into(),
                actor_agent_id: actor_agent_id.clone(),
                session_id: session_id.clone(),
                task_id: None,
                payload: json!({
                    "mergeId": merge_id,
                    "conflictId": conflict.get("id").cloned().unwrap_or(Value::Null),
                    "kind": conflict.get("kind").cloned().unwrap_or(Value::Null),
                    "table": conflict.get("table").cloned().unwrap_or(Value::Null),
                    "key": conflict.get("key").cloned().unwrap_or(Value::Null),
                    "choice": resolution.and_then(|r| r.get("choice")).cloned().unwrap_or(Value::Null),
                    "fields": resolution.and_then(|r| r.get("fields")).cloned().unwrap_or_else(|| json!({})),
                }),
                occurred_at: now(),
            })
            .map_err(|e| {
                CarryCtxError::database_error(format!(
                    "Failed to append merge.conflict_resolved event: {e}"
                ))
            })?;
    }
    Ok(())
}

// ── abort ────────────────────────────────────────────────────────────────

/// `conflict abort [--merge <id>] [--dry-run]`: delete the staging directory
/// with no database change and no journal. A dry run reports without deleting.
pub fn abort_conflicts(
    gp: &GitProject,
    xdg: &XdgPaths,
    merge: Option<&str>,
    dry_run: bool,
) -> Result<Value, CarryCtxError> {
    if dry_run {
        let dir = resolve_session_dir(gp, xdg, merge)?;
        let session = load_session(&dir)?;
        let merge_id = merge_id_of(&session.merge)?;
        return Ok(json!({
            "mergeId": merge_id,
            "aborted": false,
            "warnings": ["[dry-run] The merge session was not removed."],
            "operation": {"applied": false},
        }));
    }
    let _lock = filesystem::AdmissionLock::acquire(
        &xdg.admission_lock_dir(&gp.git_common_dir),
        &new_id(),
        std::process::id(),
        &hostname(),
        &now(),
    )?;
    let dir = resolve_session_dir(gp, xdg, merge)?;
    let session = load_session(&dir)?;
    let merge_id = merge_id_of(&session.merge)?;
    fs::remove_dir_all(&dir).map_err(|e| {
        CarryCtxError::io_error(format!("Failed to remove merge session '{merge_id}': {e}"))
    })?;
    Ok(json!({
        "mergeId": merge_id,
        "aborted": true,
        "operation": {"applied": true},
    }))
}
