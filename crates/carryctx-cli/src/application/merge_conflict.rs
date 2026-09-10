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

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use carryctx_pack::merge::identity::{identity_columns, machine_local_columns};
use serde_json::{Map, Value, json};

use crate::adapter::filesystem;
use crate::adapter::git::GitProject;
use crate::adapter::sqlite::ProjectDatabase;
use crate::adapter::sqlite_repos::{SqliteEventRepository, SqliteTombstoneRepository};
use crate::adapter::xdg::XdgPaths;
use crate::application::export::row_to_json;
use crate::application::import::{
    insert_row, json_to_sql, new_id, now, remove_sidecars, sibling_path,
};
use crate::application::merge_import::{
    active_merge_session, checkpoint_database, resolve_actor, swap_candidate_into_place,
    validate_candidate,
};
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

/// Resolve the session directory: an explicit `--merge <id>`, else the single
/// active session; none yields `RESOURCE_NOT_FOUND` (exit 7).
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
) -> Result<Value, CarryCtxError> {
    let _lock = filesystem::AdmissionLock::acquire(
        &xdg.admission_lock_dir(&gp.git_common_dir),
        &new_id(),
        std::process::id(),
        &hostname(),
        &now(),
    )?;
    let dir = resolve_session_dir(gp, xdg, merge)?;
    let mut session = load_session(&dir)?;
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
    }))
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
/// `--skip-open`; otherwise materializes every resolution into the candidate,
/// appends the audit events, validates, and atomically swaps it into place.
#[allow(clippy::too_many_arguments)]
pub fn apply_conflicts(
    gp: &GitProject,
    xdg: &XdgPaths,
    db_path: &Path,
    merge: Option<&str>,
    skip_open: bool,
    dry_run: bool,
    actor: Option<&str>,
    session_id: Option<&str>,
) -> Result<Value, CarryCtxError> {
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
    let resolved_count = preflight.conflicts.iter().filter(|c| !is_open(c)).count() as u64;

    // `--dry-run` reports what apply would do without writing anything; a
    // real apply refuses while open conflicts remain (design §2.5–§2.6).
    if dry_run {
        if !would_apply {
            preflight_warnings.push(blocking_conflicts_warning(open));
        }
        return Ok(json!({
            "mergeId": preflight_id,
            "applied": false,
            "wouldApply": would_apply,
            "resolvedCount": resolved_count,
            "openConflicts": open,
            "skipOpen": skip_open,
            "counts": preflight.merge.get("counts").cloned().unwrap_or_else(|| json!({})),
            "warnings": preflight_warnings,
            "operation": {"applied": false},
        }));
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
    let resolved: Vec<Value> = session
        .conflicts
        .iter()
        .filter(|c| !is_open(c))
        .cloned()
        .collect();
    let resolved_count = resolved.len() as u64;
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
    let candidate_path = dir.join("candidate.sqlite");

    // Resolve the actor to a ULID against the live database: audit events
    // carry an FK, so a raw name would fail the candidate closed.
    let actor_id = {
        let live = ProjectDatabase::open_readonly(db_path)?;
        resolve_actor(live.connection(), actor)?
    };

    // Materialize resolutions and append audit events in the same
    // transaction; the transaction is scoped so the candidate is closed
    // before the atomic swap.
    {
        let candidate = ProjectDatabase::open(&candidate_path)?;
        let tx = candidate
            .connection()
            .unchecked_transaction()
            .map_err(|e| {
                CarryCtxError::database_error(format!("Candidate transaction failed: {e}"))
            })?;
        for conflict in &resolved {
            apply_resolution(&tx, &project_id, conflict)?;
        }
        append_conflict_events(
            &tx,
            &session.merge,
            &merge_id,
            &project_id,
            &resolved,
            skip_open,
            actor_id,
            session_id.map(str::to_string),
        )?;
        tx.commit()
            .map_err(|e| CarryCtxError::database_error(format!("Candidate commit failed: {e}")))?;
    }
    checkpoint_database(&candidate_path)?;
    validate_candidate(&candidate_path)?;

    // The restore journal only trusts paths directly under the state
    // directory, so stage a copy at the same sibling name the clean merge
    // path uses before swapping. The session copy stays intact, keeping a
    // failed swap retryable.
    let swap_candidate = sibling_path(db_path, &format!("restore_{merge_id}"));
    fs::copy(&candidate_path, &swap_candidate).map_err(|e| {
        CarryCtxError::database_error(format!("Failed to stage merge candidate for swap: {e}"))
    })?;
    remove_sidecars(&swap_candidate);
    let pre_merge_backup_path =
        match swap_candidate_into_place(db_path, &swap_candidate, xdg, gp, &merge_id) {
            Ok(path) => path,
            Err(error) => {
                let _ = fs::remove_file(&swap_candidate);
                remove_sidecars(&swap_candidate);
                return Err(error);
            }
        };

    // Post-commit bookkeeping is deliberately best-effort: the candidate is
    // already the live database, so no failure here may surface as an error
    // (design AC9).
    let _ = fs::remove_dir_all(&dir);

    Ok(json!({
        "mergeId": merge_id,
        "applied": true,
        "resolvedCount": resolved_count,
        "conflicts": 0,
        "counts": counts,
        "path": db_path.to_string_lossy(),
        "preMergeBackupPath": pre_merge_backup_path.to_string_lossy(),
        "warnings": warnings,
        "operation": {"applied": true},
    }))
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
    let chosen = conflict.get(choice).cloned().unwrap_or(Value::Null);
    if chosen.is_null() {
        if table == "tasks" {
            detach_task_references(conn, key)?;
        }
        delete_identity_row(conn, table, key)?;
        record_tombstone(conn, project_id, table, key)?;
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

fn table_columns(conn: &rusqlite::Connection, table: &str) -> Result<Vec<String>, CarryCtxError> {
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

/// Unlink nullable references to a task before deleting it, mirroring
/// `project prune` (design §1.7: history rows are kept; only the dangling
/// pointer is cleared). `events` is append-only behind a trigger, so the
/// trigger is lifted and restored inside the caller's transaction; any
/// failure rolls the whole transaction back, restoring it.
fn detach_task_references(conn: &rusqlite::Connection, task_id: &str) -> Result<(), CarryCtxError> {
    conn.execute("DROP TRIGGER IF EXISTS events_reject_update", [])
        .map_err(|e| {
            CarryCtxError::database_error(format!(
                "Failed to lift the events append-only guard before resolution: {e}"
            ))
        })?;
    for table in ["sessions", "worktrees", "events"] {
        let sql = format!("UPDATE {table} SET task_id = NULL WHERE task_id = ?1");
        conn.execute(&sql, [task_id]).map_err(|e| {
            CarryCtxError::database_error(format!(
                "Failed to unlink {table}.task_id before resolution: {e}"
            ))
        })?;
    }
    conn.execute(
        "UPDATE tasks SET parent_task_id = NULL WHERE parent_task_id = ?1",
        [task_id],
    )
    .map_err(|e| {
        CarryCtxError::database_error(format!(
            "Failed to unlink parent_task_id before resolution: {e}"
        ))
    })?;
    conn.execute_batch(
        "CREATE TRIGGER events_reject_update\n\
         BEFORE UPDATE ON events\n\
         BEGIN\n\
           SELECT RAISE(ABORT, 'events are append-only');\n\
         END;",
    )
    .map_err(|e| {
        CarryCtxError::database_error(format!(
            "Failed to restore the events append-only guard after resolution: {e}"
        ))
    })?;
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

/// `conflict abort [--merge <id>]`: delete the staging directory with no
/// database change and no journal.
pub fn abort_conflicts(
    gp: &GitProject,
    xdg: &XdgPaths,
    merge: Option<&str>,
) -> Result<Value, CarryCtxError> {
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
