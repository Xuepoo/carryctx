//! `import --mode merge` (design
//! `2026-09-10-mergeable-git-managed-state.md` §2).
//!
//! The CLI/application half of the merge milestone: it materializes
//! `ours` from the live database and `theirs` from the incoming bundle,
//! resolves the merge base from the export-id DAG (explicit `--base` >
//! newest common ancestor > local snapshot cache > base-less degraded),
//! runs the pure engine in `carryctx-pack::merge`, builds a validated
//! candidate database in `LOAD_ORDER`, then either
//!
//! - stages a conflict session under `<state-dir>/merges/<merge_id>/` and
//!   returns [`CarryCtxError::merge_conflicts`] (`MERGE_CONFLICTS`, exit 3)
//!   with the live database and refs untouched, or
//! - takes a verified pre-merge backup and atomically swaps the candidate in
//!   through the restore-journal pattern (`project.restore` kind), appending
//!   `project.merged` plus auto-resolution/renumber audit events in the same
//!   transaction as the state change.
//!
//! Interactive conflict resolution (`conflict list/show/resolve/apply/abort`)
//! is CTX-0143; this module only stages the session and exposes
//! [`append_merge_events`] and the `merge.json`/`conflicts.json` schema for it.
//! Git-ref import (`--from-git`) and the `carryctx-snapshots` ref are
//! CTX-0144; `--base` accepts a pack directory or a local snapshot-cache
//! export id here.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::Path;

use carryctx_pack::merge::identity::identity_key;
use carryctx_pack::merge::plan::{Conflict, MergeReport, TableSet};
use carryctx_pack::merge::{
    BaseResolution, BaseSource, ExportDag, MergeOptions, MergeRequest, SnapshotNode,
};
use serde_json::{Value, json};

use crate::adapter::filesystem;
use crate::adapter::git::GitProject;
use crate::adapter::sqlite::ProjectDatabase;
use crate::adapter::sqlite_repos::SqliteEventRepository;
use crate::adapter::xdg::XdgPaths;
use crate::application::export::{collect_snapshot, row_to_json, table_exists};
use crate::application::import::{
    bundle_project_matches_manifest, insert_row, insert_rows_nulling_pruned_worktree_refs,
    is_known_table, load_teams_cycle, new_id, now, reconcile_sequence_rows, remove_sidecars,
    sibling_path,
};
use crate::application::interchange::{PackBundle, read_bundle};
use crate::domain::pack;
use crate::error::CarryCtxError;
use crate::repository::event::{EventRepository, NewEvent};

/// CLI-side merge knobs, threaded from `ImportArgs`.
#[derive(Debug, Clone, Copy, Default)]
pub struct MergeImportOptions<'a> {
    /// `--base <dir|export-id>`: explicit base override.
    pub base: Option<&'a str>,
    /// `--require-base`: refuse a degraded base-less merge (`VALIDATION_FAILED`).
    pub require_base: bool,
    /// `--strict-edits`: promote every LWW `row_edit` to a blocking conflict.
    pub strict_edits: bool,
}

/// Run `import --mode merge` against an initialized project.
///
/// `dry_run` computes the plan and reports conflicts without writing anything.
#[allow(clippy::too_many_arguments)]
pub(crate) fn merge_import(
    bundle: &PackBundle,
    gp: &GitProject,
    xdg: &XdgPaths,
    db_path: &Path,
    options: &MergeImportOptions<'_>,
    dry_run: bool,
    actor_agent_id: Option<String>,
    session_id: Option<String>,
) -> Result<Value, CarryCtxError> {
    let repository_root = gp.repository_root.to_string_lossy().into_owned();
    let git_common_dir = gp.git_common_dir.to_string_lossy().into_owned();
    let state_dir = xdg.project_state_dir(&gp.git_common_dir);
    let merge_id = new_id();

    let _admission_lock = filesystem::AdmissionLock::acquire(
        &xdg.admission_lock_dir(&gp.git_common_dir),
        &new_id(),
        std::process::id(),
        &hostname(),
        &now(),
    )?;

    // One active merge per project (design §2.4).
    if let Some(active) = active_merge_session(&state_dir)? {
        return Err(CarryCtxError::state_conflict(format!(
            "A merge session is already active for this project ({active}); resolve or abort it before starting another merge."
        ))
        .with_details(json!({ "mergeId": active }))
        .with_suggestions([
            "Run `carryctx conflict list` to inspect the pending merge.".to_string(),
        ]));
    }

    // Materialize `ours` and the local snapshot bookkeeping from one
    // read-only connection so no write can interleave before staging.
    let (ours_snapshot, snapshot_state_rows, ours_export_id, actor_agent_id) = {
        let db = ProjectDatabase::open_readonly(db_path)?;
        let snapshot = collect_snapshot(db.connection())?;
        let local_rows = dump_optional_table(db.connection(), "snapshot_state")?;
        let last_export = read_last_export_id(db.connection())?;
        // `import` is a direct-lock command, so the pre-dispatch runtime does
        // not normalize `--agent` to a ULID; event attribution needs one.
        let actor = resolve_actor(db.connection(), actor_agent_id.as_deref())?;
        (snapshot, local_rows, last_export, actor)
    };
    if ours_snapshot.project_id != bundle.manifest.project_id {
        return Err(CarryCtxError::state_conflict(format!(
            "Bundle project '{}' does not match local project '{}'; refusing to fork identity.",
            bundle.manifest.project_id, ours_snapshot.project_id
        ))
        .with_details(json!({
            "localProjectId": ours_snapshot.project_id,
            "bundleProjectId": bundle.manifest.project_id,
        })));
    }

    let mut ours_tables: TableSet = ours_snapshot.tables.clone();
    ours_tables.insert("projects".to_string(), vec![ours_snapshot.project.clone()]);
    let mut theirs_tables: TableSet = bundle.tables.clone();
    theirs_tables.insert("projects".to_string(), vec![bundle.project.clone()]);

    let snapshot_cache = state_dir.join("snapshots");
    let mut cache_warnings: Vec<String> = Vec::new();
    let dag = build_export_dag(&snapshot_cache, bundle, &mut cache_warnings)?;
    let theirs_export_id = Some(bundle.manifest.export_id.clone());

    let explicit_base = match options.base {
        None => None,
        Some(value) => Some(load_explicit_base(value, &snapshot_cache)?),
    };

    let resolution = {
        let cache = &snapshot_cache;
        let warnings = &mut cache_warnings;
        carryctx_pack::merge::resolve_base(
            &dag,
            explicit_base.as_ref(),
            ours_export_id.as_deref(),
            theirs_export_id.as_deref(),
            options.require_base,
            |id| match cached_base(cache, id) {
                Ok(base) => base,
                Err(error) => {
                    warnings.push(format!(
                        "Ignoring unusable snapshot cache entry '{id}': {}",
                        error.message
                    ));
                    None
                }
            },
        )
    };

    if resolution.is_required_missing() {
        return Err(carryctx_pack::merge::required_base_error()
            .with_details(json!({
                "kind": "base_required_missing",
                "mergeId": merge_id,
                "oursExportId": ours_export_id,
                "bundleExportId": theirs_export_id,
            }))
            .with_suggestions([
                "Pass --base <dir|export-id> or populate the local snapshot cache.".to_string(),
            ]));
    }

    let engine_options = MergeOptions {
        strict_edits: options.strict_edits,
        require_base: options.require_base,
    };
    let mut report = match &resolution {
        BaseResolution::Resolved { base, source } => {
            MergeRequest::new(&ours_tables, &theirs_tables)
                .with_options(engine_options)
                .with_base(base, *source)
                .run()?
        }
        BaseResolution::Degraded | BaseResolution::RequiredMissing => {
            MergeRequest::new(&ours_tables, &theirs_tables)
                .with_options(engine_options)
                .run()?
        }
    };
    report.warnings.extend(cache_warnings);
    report.warnings.sort();
    report.warnings.dedup();

    let base_export_id = match report.base_source {
        BaseSource::Ancestor => match (ours_export_id.as_deref(), theirs_export_id.as_deref()) {
            (Some(ours), Some(theirs)) => dag.newest_common_ancestor(ours, theirs),
            _ => None,
        },
        BaseSource::Snapshot => ours_export_id.clone(),
        BaseSource::Explicit | BaseSource::None => None,
    };

    if dry_run {
        return Ok(json!({
            "bundleDir": bundle.dir.to_string_lossy(),
            "bundleProjectId": bundle.manifest.project_id,
            "mode": "merge",
            "mergeId": Value::Null,
            "baseSource": base_source_str(report.base_source),
            "baseExportId": base_export_id,
            "oursExportId": ours_export_id,
            "theirsExportId": theirs_export_id,
            "degraded": report.degraded,
            "wouldConflict": report.has_conflicts(),
            "conflictCount": report.conflicts.len(),
            "counts": result_counts(&report.result),
            "writes": report.writes.len(),
            "deletes": report.deletes.len(),
            "autoResolutions": report.auto_resolutions.len(),
            "renumbers": report.renumbers.len(),
            "aliases": report.aliases.len(),
            "warnings": report.warnings,
            "operation": {"applied": false},
        }));
    }

    // Candidate database is built at the swap path so the restore-journal
    // recovery path heals an interrupted merge without new recovery code.
    let candidate_path = sibling_path(db_path, &format!("restore_{merge_id}"));
    let build_result = build_candidate(
        &candidate_path,
        &report,
        &ours_tables,
        &ours_snapshot.project_id,
        &repository_root,
        &git_common_dir,
        &snapshot_state_rows,
    );
    let candidate_warnings = match build_result {
        Ok(warnings) => warnings,
        Err(error) => {
            remove_database_files(&candidate_path);
            return Err(error);
        }
    };
    let mut warnings = report.warnings.clone();
    warnings.extend(candidate_warnings);
    warnings.sort();
    warnings.dedup();

    if report.has_conflicts() {
        let session_dir = state_dir.join("merges").join(&merge_id);
        if let Err(error) = stage_conflict_session(
            &session_dir,
            &candidate_path,
            bundle,
            &report,
            &merge_id,
            base_source_str(report.base_source),
            base_export_id.as_deref(),
            ours_export_id.as_deref(),
            theirs_export_id.as_deref(),
            &warnings,
        ) {
            remove_database_files(&candidate_path);
            let _ = fs::remove_dir_all(&session_dir);
            return Err(error);
        }
        return Err(CarryCtxError::merge_conflicts(format!(
            "Merge produced {} blocking conflict(s); the live database is untouched. Resolve and apply the staged session to continue.",
            report.conflicts.len()
        ))
        .with_details(json!({
            "mergeId": merge_id,
            "conflicts": report.conflicts.len(),
            "mergeDir": session_dir.to_string_lossy(),
            "baseSource": base_source_str(report.base_source),
            "degraded": report.degraded,
        }))
        .with_suggestions([
            "Run `carryctx conflict list` to inspect the staged conflicts.".to_string(),
        ]));
    }

    // Append the merge-completion audit events into the candidate, then
    // re-validate the final image before it can become live.
    {
        let candidate = ProjectDatabase::open(&candidate_path)?;
        let tx = candidate
            .connection()
            .unchecked_transaction()
            .map_err(|e| {
                CarryCtxError::database_error(format!("Candidate transaction failed: {e}"))
            })?;
        append_merge_events(
            &tx,
            &report,
            &merge_id,
            &ours_snapshot.project_id,
            base_source_str(report.base_source),
            base_export_id.as_deref(),
            ours_export_id.as_deref(),
            theirs_export_id.as_deref(),
            actor_agent_id.clone(),
            session_id.clone(),
        )?;
        tx.commit()
            .map_err(|e| CarryCtxError::database_error(format!("Candidate commit failed: {e}")))?;
        checkpoint_database(&candidate_path)?;
    }
    if let Err(error) = validate_candidate(&candidate_path) {
        remove_database_files(&candidate_path);
        return Err(error);
    }

    if let Err(error) = swap_candidate_into_place(db_path, &candidate_path, xdg, gp, &merge_id) {
        remove_database_files(&candidate_path);
        return Err(error);
    }

    Ok(json!({
        "projectId": bundle.manifest.project_id,
        "mode": "merge",
        "mergeId": merge_id,
        "baseSource": base_source_str(report.base_source),
        "baseExportId": base_export_id,
        "oursExportId": ours_export_id,
        "theirsExportId": theirs_export_id,
        "degraded": report.degraded,
        "counts": result_counts(&report.result),
        "writes": report.writes.len(),
        "deletes": report.deletes.len(),
        "autoResolutions": report.auto_resolutions.len(),
        "renumbers": report.renumbers.len(),
        "aliases": report.aliases.len(),
        "conflicts": 0,
        "path": db_path.to_string_lossy(),
        "warnings": warnings,
        "operation": {"applied": true},
    }))
}

fn hostname() -> String {
    std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".into())
}

/// `BaseSource` as the stable public string used in envelopes and events.
fn base_source_str(source: BaseSource) -> &'static str {
    match source {
        BaseSource::Explicit => "explicit",
        BaseSource::Ancestor => "ancestor",
        BaseSource::Snapshot => "snapshot",
        BaseSource::None => "none",
    }
}

/// Row count per ctxpack table for the merged candidate.
fn result_counts(result: &TableSet) -> BTreeMap<String, u64> {
    let mut counts = BTreeMap::new();
    for table in pack::PACK_TABLE_FILES {
        let len = result.get(*table).map(Vec::len).unwrap_or(0) as u64;
        counts.insert((*table).to_string(), len);
    }
    counts
}

/// Build the export-id DAG from the local snapshot cache plus the incoming
/// bundle's own node. Cache entries are read manifest-only: the base row set
/// is materialized lazily by [`cached_base`] only for the selected ancestor.
fn build_export_dag(
    snapshot_cache: &Path,
    bundle: &PackBundle,
    warnings: &mut Vec<String>,
) -> Result<ExportDag, CarryCtxError> {
    let mut nodes: Vec<SnapshotNode> = Vec::new();
    if let Ok(entries) = fs::read_dir(snapshot_cache) {
        for entry in entries.filter_map(Result::ok) {
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            match read_cached_node(&entry.path()) {
                Some(node) => nodes.push(node),
                None => warnings.push(format!(
                    "Ignoring snapshot cache entry '{}' with no readable manifest.",
                    entry.file_name().to_string_lossy()
                )),
            }
        }
    }
    nodes.push(SnapshotNode::new(
        bundle.manifest.export_id.clone(),
        bundle.manifest.parents.clone(),
    ));
    Ok(ExportDag::from_snapshot_nodes(&nodes))
}

/// Read `manifest.json` of one cache entry into a DAG node.
fn read_cached_node(dir: &Path) -> Option<SnapshotNode> {
    let raw = fs::read_to_string(dir.join(pack::PACK_MANIFEST_FILE)).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    let export_id = value.get("export_id")?.as_str()?.to_string();
    let parents = value
        .get("parents")
        .and_then(Value::as_array)
        .map(|parents| {
            parents
                .iter()
                .filter_map(|parent| parent.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Some(SnapshotNode::new(export_id, parents))
}

/// Materialize one cached snapshot as a base row set, or `None` when the
/// entry is absent.
fn cached_base(cache_dir: &Path, export_id: &str) -> Result<Option<TableSet>, CarryCtxError> {
    let dir = cache_dir.join(export_id);
    if !dir.is_dir() {
        return Ok(None);
    }
    let bundle = read_bundle(&dir)?;
    bundle_project_matches_manifest(&bundle)?;
    let mut tables = bundle.tables.clone();
    tables.insert("projects".to_string(), vec![bundle.project.clone()]);
    Ok(Some(tables))
}

/// Resolve `--base`: an existing pack directory or a local snapshot-cache
/// export id. Git-ref bases land with CTX-0144.
fn load_explicit_base(value: &str, cache_dir: &Path) -> Result<TableSet, CarryCtxError> {
    let as_path = Path::new(value);
    if as_path.is_dir() {
        let bundle = read_bundle(as_path)?;
        bundle_project_matches_manifest(&bundle)?;
        let mut tables = bundle.tables.clone();
        tables.insert("projects".to_string(), vec![bundle.project.clone()]);
        return Ok(tables);
    }
    if looks_like_export_id(value) {
        if let Some(base) = cached_base(cache_dir, value)? {
            return Ok(base);
        }
        return Err(CarryCtxError::invalid_arguments(format!(
            "Base export id '{value}' is not present in the local snapshot cache."
        )));
    }
    Err(CarryCtxError::invalid_arguments(format!(
        "Base '{value}' is neither a pack directory nor a local export id; git-ref bases are not supported in this command yet."
    ))
    .with_suggestions([
        "Pass a ctxpack directory or an export id present under <state-dir>/snapshots/."
            .to_string(),
    ]))
}

fn looks_like_export_id(value: &str) -> bool {
    value.len() == 26
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte.is_ascii_uppercase())
}

/// The staged merge session id, if any. A directory carrying `merge.json`
/// whose status is not terminal is an active session.
fn active_merge_session(state_dir: &Path) -> Result<Option<String>, CarryCtxError> {
    let dir = state_dir.join("merges");
    let Ok(entries) = fs::read_dir(&dir) else {
        return Ok(None);
    };
    let mut candidates: Vec<String> = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let merge_json = entry.path().join("merge.json");
        if !merge_json.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let status = fs::read_to_string(&merge_json)
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .and_then(|value| {
                value
                    .get("status")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            });
        let terminal = matches!(status.as_deref(), Some("applied") | Some("aborted"));
        if !terminal {
            candidates.push(name);
        }
    }
    candidates.sort();
    Ok(candidates.into_iter().next())
}

/// Resolve an `--agent` reference (ULID or name) against the live database so
/// event attribution carries a valid foreign key. `None`/empty stays `None`.
fn resolve_actor(
    conn: &rusqlite::Connection,
    actor: Option<&str>,
) -> Result<Option<String>, CarryCtxError> {
    let Some(actor) = actor.map(str::trim).filter(|actor| !actor.is_empty()) else {
        return Ok(None);
    };
    let found: Result<String, _> = conn.query_row(
        "SELECT id FROM agents WHERE id = ?1 OR name = ?1 LIMIT 1",
        [actor],
        |row| row.get(0),
    );
    match found {
        Ok(id) => Ok(Some(id)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Err(CarryCtxError::resource_not_found(
            format!("Agent '{actor}' not found."),
        )),
        Err(error) => Err(CarryCtxError::database_error(format!(
            "Failed to resolve agent '{actor}': {error}"
        ))),
    }
}

fn read_last_export_id(conn: &rusqlite::Connection) -> Result<Option<String>, CarryCtxError> {
    if !table_exists(conn, "snapshot_state")? {
        return Ok(None);
    }
    let id: Result<String, _> = conn.query_row(
        "SELECT value FROM snapshot_state WHERE key = 'last_export_id' LIMIT 1",
        [],
        |row| row.get(0),
    );
    match id {
        Ok(id) => Ok(Some(id)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(error) => Err(CarryCtxError::database_error(format!(
            "Failed to read snapshot_state: {error}"
        ))),
    }
}

/// Dump a machine-local table that is not part of the ctxpack file set, or an
/// empty vector when the table does not exist on an older schema.
fn dump_optional_table(
    conn: &rusqlite::Connection,
    table: &str,
) -> Result<Vec<Value>, CarryCtxError> {
    if !table_exists(conn, table)? {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare(&format!("SELECT * FROM \"{table}\" ORDER BY rowid"))
        .map_err(|e| {
            CarryCtxError::database_error(format!("Failed to dump local table '{table}': {e}"))
        })?;
    let names: Vec<String> = stmt
        .column_names()
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    let rows = stmt
        .query_map([], |row| row_to_json(&names, row))
        .map_err(|e| {
            CarryCtxError::database_error(format!("Failed to dump local table '{table}': {e}"))
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| {
            CarryCtxError::database_error(format!("Failed to read local table '{table}': {e}"))
        })?;
    Ok(rows)
}

/// Build the candidate database from the merged row set.
///
/// Reconciliation happens before insert (design §1.7): worktrees are
/// machine-local, so incoming rows are kept only when their path exists at the
/// target and the active-task slot is free; `sessions`/`checkpoints` links to
/// pruned worktrees are nulled, and dangling `events.task_id` values are
/// nulled, mirroring replace import. Every other foreign key stays enforced.
#[allow(clippy::too_many_arguments)]
fn build_candidate(
    candidate_path: &Path,
    report: &MergeReport,
    ours_tables: &TableSet,
    project_id: &str,
    repository_root: &str,
    git_common_dir: &str,
    snapshot_state_rows: &[Value],
) -> Result<Vec<String>, CarryCtxError> {
    let mut candidate = ProjectDatabase::create_fresh(candidate_path)?;
    candidate
        .connection()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .map_err(|e| CarryCtxError::database_error(format!("Candidate checkpoint failed: {e}")))?;

    let mut warnings: Vec<String> = Vec::new();
    {
        let tx = candidate.connection_mut().transaction().map_err(|e| {
            CarryCtxError::database_error(format!("Candidate transaction failed: {e}"))
        })?;

        // Project row first: identity comes from `ours`, anchors re-pointed.
        let project = report
            .result
            .get("projects")
            .and_then(|rows| rows.first())
            .cloned()
            .ok_or_else(|| {
                CarryCtxError::validation_error(
                    "Merge result is missing the project row.".to_string(),
                )
            })?;
        let mut project_map = project.as_object().cloned().ok_or_else(|| {
            CarryCtxError::validation_error("Merged project row must be an object.".to_string())
        })?;
        pack::reanchor_project(&mut project_map, repository_root, git_common_dir);
        insert_row(&tx, "projects", &serde_json::Value::Object(project_map))?;

        // Worktree reconciliation (ours win; incoming rows need a live path and
        // a free active-task slot).
        let ours_worktree_keys = worktree_key_set(ours_tables)?;
        let merged_worktrees = report.result.get("worktrees").cloned().unwrap_or_default();
        let (kept_worktrees, pruned_worktrees) =
            reconcile_worktrees(merged_worktrees, &ours_worktree_keys);
        let kept_worktree_ids: HashSet<&str> = kept_worktrees
            .iter()
            .filter_map(|row| row.get("id").and_then(Value::as_str))
            .collect();

        for table in crate::application::import::LOAD_ORDER {
            match *table {
                "worktrees" => {
                    for row in &kept_worktrees {
                        insert_row(&tx, table, row)?;
                    }
                }
                "sessions" | "checkpoints" => {
                    insert_rows_nulling_pruned_worktree_refs(
                        &tx,
                        table,
                        report.result.get(*table).map(Vec::as_slice),
                        &kept_worktree_ids,
                        &mut warnings,
                    )?;
                }
                "teams" => {
                    load_teams_cycle(
                        &tx,
                        report.result.get("teams").map(Vec::as_slice),
                        report.result.get("team_members").map(Vec::as_slice),
                    )?;
                }
                "team_members" => {}
                "events" => {
                    insert_events_nulling_dangling_tasks(&tx, report, &mut warnings)?;
                }
                "sequences" => {}
                _ => {
                    if let Some(rows) = report.result.get(*table) {
                        for row in rows {
                            insert_row(&tx, table, row)?;
                        }
                    }
                }
            }
        }

        reconcile_sequence_rows(
            &tx,
            project_id,
            report.result.get("sequences").map(Vec::as_slice),
        )?;

        // Carry machine-local snapshot bookkeeping into the candidate so the
        // next merge can still resolve a base.
        copy_snapshot_state(&tx, snapshot_state_rows)?;

        // Pruned worktrees are audited exactly like replace import.
        for pruned in &pruned_worktrees {
            append_worktree_pruned(&tx, project_id, &pruned.row, pruned.reason)?;
            warnings.push(format!(
                "Pruned worktree {} ({}): {}.",
                pruned
                    .row
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("<unknown>"),
                pruned
                    .row
                    .get("normalized_path")
                    .and_then(Value::as_str)
                    .unwrap_or("<missing>"),
                match pruned.reason {
                    "active_task_slot_taken" => "active-task slot already taken at import target",
                    _ => "directory missing at import target",
                }
            ));
        }

        tx.commit()
            .map_err(|e| CarryCtxError::database_error(format!("Candidate commit failed: {e}")))?;
    }

    candidate
        .connection()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .map_err(|e| CarryCtxError::database_error(format!("Candidate checkpoint failed: {e}")))?;
    drop(candidate);
    validate_candidate(candidate_path)?;
    Ok(warnings)
}

/// Identity-key set of the local worktrees, to separate "ours" from incoming
/// rows during reconciliation.
fn worktree_key_set(ours_tables: &TableSet) -> Result<BTreeSet<String>, CarryCtxError> {
    let mut keys = BTreeSet::new();
    if let Some(rows) = ours_tables.get("worktrees") {
        for row in rows {
            let map = row.as_object().ok_or_else(|| {
                CarryCtxError::validation_error(
                    "Local worktree row must be a JSON object.".to_string(),
                )
            })?;
            keys.insert(identity_key("worktrees", map)?);
        }
    }
    Ok(keys)
}

struct PrunedWorktree {
    row: Value,
    reason: &'static str,
}

/// Split merged worktree rows into kept/pruned. Local rows are always kept;
/// incoming rows require a live `normalized_path` and a free active-task slot.
fn reconcile_worktrees(
    rows: Vec<Value>,
    ours_keys: &BTreeSet<String>,
) -> (Vec<Value>, Vec<PrunedWorktree>) {
    let mut kept = Vec::new();
    let mut pruned = Vec::new();
    let mut used_tasks: HashSet<String> = HashSet::new();
    for row in rows {
        let map = match row.as_object() {
            Some(map) => map,
            None => {
                pruned.push(PrunedWorktree {
                    row,
                    reason: "malformed_row",
                });
                continue;
            }
        };
        let key = identity_key("worktrees", map).unwrap_or_default();
        let task_id = map
            .get("task_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        if ours_keys.contains(&key) {
            if let Some(task_id) = task_id {
                used_tasks.insert(task_id);
            }
            kept.push(row);
            continue;
        }
        let live = map
            .get("normalized_path")
            .and_then(Value::as_str)
            .is_some_and(|path| Path::new(path).exists());
        let slot_free = task_id
            .as_ref()
            .is_none_or(|task_id| !used_tasks.contains(task_id));
        if live && slot_free {
            if let Some(task_id) = task_id {
                used_tasks.insert(task_id);
            }
            kept.push(row);
        } else {
            pruned.push(PrunedWorktree {
                row,
                reason: if slot_free {
                    "directory_missing"
                } else {
                    "active_task_slot_taken"
                },
            });
        }
    }
    (kept, pruned)
}

/// Insert `events`, nulling `task_id` links whose task is absent from the
/// merged result (matching the replace-import convergence).
fn insert_events_nulling_dangling_tasks(
    conn: &rusqlite::Connection,
    report: &MergeReport,
    warnings: &mut Vec<String>,
) -> Result<(), CarryCtxError> {
    let known_tasks: HashSet<&str> = report
        .result
        .get("tasks")
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.get("id").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    let mut nulled = 0u64;
    if let Some(rows) = report.result.get("events") {
        for row in rows {
            let dangling = row
                .get("task_id")
                .and_then(Value::as_str)
                .is_some_and(|id| !known_tasks.contains(id));
            if dangling {
                let mut fixed = row.clone();
                if let Some(object) = fixed.as_object_mut() {
                    object.insert("task_id".to_string(), Value::Null);
                }
                insert_row(conn, "events", &fixed)?;
                nulled += 1;
            } else {
                insert_row(conn, "events", row)?;
            }
        }
    }
    if nulled > 0 {
        warnings.push(format!(
            "Nulled {nulled} dangling event task reference(s) (referenced task absent from the merged result; audit rows kept)."
        ));
    }
    Ok(())
}

fn append_worktree_pruned(
    conn: &rusqlite::Connection,
    project_id: &str,
    row: &Value,
    reason: &str,
) -> Result<(), CarryCtxError> {
    let worktree_id = row.get("id").and_then(Value::as_str).unwrap_or_default();
    if worktree_id.is_empty() {
        return Ok(());
    }
    SqliteEventRepository::new(conn)
        .append(&NewEvent {
            id: new_id(),
            project_id: project_id.to_string(),
            event_type: "worktree.pruned".into(),
            actor_agent_id: None,
            session_id: None,
            task_id: row
                .get("task_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            payload: json!({
                "worktree_id": worktree_id,
                "path": row.get("normalized_path").cloned().unwrap_or(Value::Null),
                "task_id": row.get("task_id").cloned().unwrap_or(Value::Null),
                "reason": reason,
            }),
            occurred_at: now(),
        })
        .map_err(|e| {
            CarryCtxError::database_error(format!("Failed to append worktree.pruned event: {e}"))
        })?;
    Ok(())
}

/// Copy the machine-local `snapshot_state` rows into the candidate.
fn copy_snapshot_state(conn: &rusqlite::Connection, rows: &[Value]) -> Result<(), CarryCtxError> {
    for row in rows {
        let project_id = row.get("project_id").and_then(Value::as_str);
        let key = row.get("key").and_then(Value::as_str);
        let value = row.get("value").and_then(Value::as_str);
        let updated_at = row.get("updated_at").and_then(Value::as_str);
        let (Some(project_id), Some(key), Some(value), Some(updated_at)) =
            (project_id, key, value, updated_at)
        else {
            return Err(CarryCtxError::database_error(
                "Local snapshot_state row is malformed.".to_string(),
            ));
        };
        conn.execute(
            "INSERT INTO snapshot_state (project_id, key, value, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(project_id, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            rusqlite::params![project_id, key, value, updated_at],
        )
        .map_err(|e| {
            CarryCtxError::database_error(format!("Failed to copy snapshot_state: {e}"))
        })?;
    }
    Ok(())
}

/// Append `project.merged`, `merge.auto_resolved`, and
/// `merge.display_id_renumbered` events. Shared with CTX-0143's
/// `conflict apply`, which stages a candidate first and applies it later.
#[allow(clippy::too_many_arguments)]
pub(crate) fn append_merge_events(
    conn: &rusqlite::Connection,
    report: &MergeReport,
    merge_id: &str,
    project_id: &str,
    base_source: &str,
    base_export_id: Option<&str>,
    ours_export_id: Option<&str>,
    theirs_export_id: Option<&str>,
    actor_agent_id: Option<String>,
    session_id: Option<String>,
) -> Result<(), CarryCtxError> {
    let events = SqliteEventRepository::new(conn);
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
                "baseSource": base_source,
                "baseExportId": base_export_id,
                "oursExportId": ours_export_id,
                "theirsExportId": theirs_export_id,
                "degraded": report.degraded,
                "counts": result_counts(&report.result),
                "autoResolutions": report.auto_resolutions.len(),
                "renumbers": report.renumbers.len(),
                "aliases": report.aliases.len(),
            }),
            occurred_at: now(),
        })
        .map_err(|e| {
            CarryCtxError::database_error(format!("Failed to append project.merged event: {e}"))
        })?;

    for resolution in &report.auto_resolutions {
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
                    "table": resolution.table,
                    "key": resolution.key,
                    "kind": resolution.kind,
                    "winner": resolution.winner,
                    "reason": resolution.reason,
                }),
                occurred_at: now(),
            })
            .map_err(|e| {
                CarryCtxError::database_error(format!(
                    "Failed to append merge.auto_resolved event: {e}"
                ))
            })?;
    }

    for renumber in &report.renumbers {
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
                    "table": renumber.table,
                    "rowId": renumber.row_id,
                    "displayId": renumber.display_id,
                    "newDisplayId": renumber.new_display_id,
                    "reason": renumber.reason,
                }),
                occurred_at: now(),
            })
            .map_err(|e| {
                CarryCtxError::database_error(format!(
                    "Failed to append merge.display_id_renumbered event: {e}"
                ))
            })?;
    }
    Ok(())
}

/// Write the conflict session: candidate, `theirs/`, `merge.json`, and
/// `conflicts.json` (design §2.4). The candidate carries conflicts at `ours`.
#[allow(clippy::too_many_arguments)]
fn stage_conflict_session(
    session_dir: &Path,
    candidate_path: &Path,
    bundle: &PackBundle,
    report: &MergeReport,
    merge_id: &str,
    base_source: &str,
    base_export_id: Option<&str>,
    ours_export_id: Option<&str>,
    theirs_export_id: Option<&str>,
    warnings: &[String],
) -> Result<(), CarryCtxError> {
    filesystem::ensure_dir(session_dir)?;
    let session_candidate = session_dir.join("candidate.sqlite");
    fs::rename(candidate_path, &session_candidate).map_err(|e| {
        CarryCtxError::database_error(format!("Failed to stage merge candidate: {e}"))
    })?;
    remove_sidecars(candidate_path);

    materialize_theirs(bundle, &session_dir.join("theirs"))?;

    let merge_json = json!({
        "schemaVersion": 1,
        "mergeId": merge_id,
        "projectId": bundle.manifest.project_id,
        "status": "conflicts_open",
        "createdAt": now(),
        "sourceDir": bundle.dir.to_string_lossy(),
        "sourceExportId": theirs_export_id,
        "sourceFormatVersion": bundle.source_format_version,
        "sourceParents": bundle.manifest.parents,
        "baseSource": base_source,
        "baseExportId": base_export_id,
        "oursExportId": ours_export_id,
        "degraded": report.degraded,
        "counts": result_counts(&report.result),
        "conflictCount": report.conflicts.len(),
        "autoResolutions": report.auto_resolutions.iter().map(auto_resolution_json).collect::<Vec<_>>(),
        "renumbers": report.renumbers.iter().map(renumber_json).collect::<Vec<_>>(),
        "aliases": report.aliases.iter().map(|alias| json!({
            "agentName": alias.name,
            "existingAgentId": alias.existing_agent_id,
            "incomingAgentId": alias.incoming_agent_id,
        })).collect::<Vec<_>>(),
        "warnings": warnings,
    });
    write_json(&session_dir.join("merge.json"), &merge_json)?;
    let conflicts: Vec<Value> = report.conflicts.iter().map(conflict_json).collect();
    write_json(
        &session_dir.join("conflicts.json"),
        &Value::Array(conflicts),
    )?;
    Ok(())
}

fn auto_resolution_json(resolution: &carryctx_pack::merge::AutoResolution) -> Value {
    json!({
        "table": resolution.table,
        "key": resolution.key,
        "kind": resolution.kind,
        "winner": resolution.winner,
        "reason": resolution.reason,
    })
}

fn renumber_json(renumber: &carryctx_pack::merge::Renumber) -> Value {
    json!({
        "table": renumber.table,
        "rowId": renumber.row_id,
        "displayId": renumber.display_id,
        "newDisplayId": renumber.new_display_id,
        "reason": renumber.reason,
    })
}

fn conflict_json(conflict: &Conflict) -> Value {
    json!({
        "id": conflict.id,
        "kind": conflict.kind,
        "table": conflict.table,
        "key": conflict.key,
        "base": conflict.base,
        "ours": conflict.ours,
        "theirs": conflict.theirs,
        "reason": conflict.reason,
        // Reserved for CTX-0143; `null` means unresolved.
        "resolution": Value::Null,
    })
}

fn write_json(path: &Path, value: &Value) -> Result<(), CarryCtxError> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| CarryCtxError::io_error(format!("Failed to encode session JSON: {e}")))?;
    filesystem::write_atomic(path, text.as_bytes())
}

/// Copy the incoming bundle files into `<session>/theirs/`.
fn materialize_theirs(bundle: &PackBundle, dest: &Path) -> Result<(), CarryCtxError> {
    filesystem::ensure_dir(dest)?;
    for name in [pack::PACK_MANIFEST_FILE, pack::PACK_PROJECT_FILE] {
        let source = bundle.dir.join(name);
        fs::copy(&source, dest.join(name)).map_err(|e| {
            CarryCtxError::io_error(format!(
                "Failed to copy '{}' into the merge session: {e}",
                source.display()
            ))
        })?;
    }
    for table in pack::pack_table_files(bundle.source_format_version) {
        let source = bundle.dir.join(format!("{table}.jsonl"));
        if source.is_file() {
            fs::copy(&source, dest.join(format!("{table}.jsonl"))).map_err(|e| {
                CarryCtxError::io_error(format!(
                    "Failed to copy '{}' into the merge session: {e}",
                    source.display()
                ))
            })?;
        }
    }
    Ok(())
}

/// Verify the candidate passes the same gate as `sync pull`/replace import
/// plus the merge-specific tombstone consistency invariant.
fn validate_candidate(path: &Path) -> Result<(), CarryCtxError> {
    crate::application::project_mgmt::validate_database_for_sync(path)?;
    validate_tombstones(path)
}

/// Fail closed when a tombstone names a row the candidate still carries: the
/// merged result must never hold both a row and its deletion.
fn validate_tombstones(path: &Path) -> Result<(), CarryCtxError> {
    let db = ProjectDatabase::open_readonly(path)?;
    let conn = db.connection();
    let project_id: String = conn
        .query_row("SELECT id FROM projects LIMIT 1", [], |row| row.get(0))
        .map_err(|e| CarryCtxError::database_error(format!("Tombstone check failed: {e}")))?;
    let mut stmt = conn
        .prepare("SELECT table_name, row_id FROM tombstones WHERE project_id = ?1")
        .map_err(|e| CarryCtxError::database_error(format!("Tombstone check failed: {e}")))?;
    let rows = stmt
        .query_map([&project_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| CarryCtxError::database_error(format!("Tombstone check failed: {e}")))?;
    for row in rows {
        let (table, row_id) =
            row.map_err(|e| CarryCtxError::database_error(format!("Tombstone check failed: {e}")))?;
        if !is_known_table(&table) || table == "tombstones" {
            return Err(inconsistent_tombstone(&table, &row_id, "unknown table"));
        }
        let present = if table == "team_members" {
            let parts: Vec<String> = serde_json::from_str(&row_id).map_err(|_| {
                inconsistent_tombstone(&table, &row_id, "malformed composite row id")
            })?;
            if parts.len() != 2 {
                return Err(inconsistent_tombstone(
                    &table,
                    &row_id,
                    "malformed composite row id",
                ));
            }
            conn.query_row(
                "SELECT COUNT(*) FROM team_members WHERE team_id = ?1 AND agent_id = ?2",
                rusqlite::params![parts[0], parts[1]],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|e| CarryCtxError::database_error(format!("Tombstone check failed: {e}")))?
        } else {
            let sql = format!("SELECT COUNT(*) FROM \"{table}\" WHERE id = ?1");
            conn.query_row(&sql, [&row_id], |row| row.get::<_, i64>(0))
                .map_err(|e| {
                    CarryCtxError::database_error(format!("Tombstone check failed: {e}"))
                })?
        };
        if present > 0 {
            return Err(inconsistent_tombstone(&table, &row_id, "row still present"));
        }
    }
    Ok(())
}

fn inconsistent_tombstone(table: &str, row_id: &str, reason: &str) -> CarryCtxError {
    CarryCtxError::validation_error(format!(
        "Merged candidate is inconsistent: tombstone for '{table}':'{row_id}' is invalid ({reason})."
    ))
    .with_details(json!({
        "kind": "tombstone_inconsistent",
        "table": table,
        "rowId": row_id,
    }))
}

/// Verified pre-merge backup + restore-journal atomic swap (design §2.2 step
/// 8). The journal reuses the `project.restore` recovery path so an
/// interrupted merge heals on the next write command.
fn swap_candidate_into_place(
    db_path: &Path,
    candidate_path: &Path,
    xdg: &XdgPaths,
    gp: &GitProject,
    merge_id: &str,
) -> Result<(), CarryCtxError> {
    let operation_id = merge_id.to_string();
    let backup_dir = xdg.backup_dir(&gp.git_common_dir);
    filesystem::ensure_dir(&backup_dir)?;
    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let pre_backup_path = backup_dir.join(format!("pre_merge_{timestamp}_{operation_id}.sqlite"));
    {
        let current = ProjectDatabase::open_readonly(db_path)?;
        current.create_backup(&pre_backup_path)?;
        drop(current);
        crate::application::project_mgmt::validate_database_for_sync(&pre_backup_path)?;
    }
    checkpoint_database(db_path)?;
    remove_sidecars(db_path);

    let original_path = sibling_path(db_path, &format!("original_{operation_id}"));
    let journal_dir = xdg.journal_dir(&gp.git_common_dir);
    filesystem::write_journal(
        &journal_dir,
        &filesystem::JournalEntry {
            operation_id: operation_id.clone(),
            kind: "project.restore".into(),
            status: "prepared".into(),
            created_at: now(),
            metadata: json!({
                "databasePath": db_path.to_string_lossy(),
                "candidatePath": candidate_path.to_string_lossy(),
                "originalPath": original_path.to_string_lossy(),
                "merge": true,
                "preMergeBackupPath": pre_backup_path.to_string_lossy(),
            }),
        },
    )?;

    if let Err(error) = fs::hard_link(db_path, &original_path) {
        let _ = filesystem::remove_journal(&journal_dir, &operation_id);
        return Err(CarryCtxError::database_error(format!(
            "Failed to preserve active database: {error}"
        )));
    }
    if let Err(error) = fs::rename(candidate_path, db_path) {
        let _ = fs::remove_file(&original_path);
        let _ = filesystem::remove_journal(&journal_dir, &operation_id);
        return Err(CarryCtxError::database_error(format!(
            "Failed to atomically swap the merged database: {error}"
        )));
    }
    let _ = fs::remove_file(&original_path);
    remove_sidecars(db_path);

    filesystem::write_journal(
        &journal_dir,
        &filesystem::JournalEntry {
            operation_id: operation_id.clone(),
            kind: "project.restore".into(),
            status: "completed".into(),
            created_at: now(),
            metadata: json!({
                "databasePath": db_path.to_string_lossy(),
                "preMergeBackupPath": pre_backup_path.to_string_lossy(),
            }),
        },
    )?;
    filesystem::remove_journal(&journal_dir, &operation_id)?;
    Ok(())
}

fn checkpoint_database(path: &Path) -> Result<(), CarryCtxError> {
    let database = ProjectDatabase::open(path)?;
    database
        .connection()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .map_err(|e| CarryCtxError::database_error(format!("Database checkpoint failed: {e}")))
}

fn remove_database_files(path: &Path) {
    let _ = fs::remove_file(path);
    remove_sidecars(path);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn seeded_candidate(project_id: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("candidate.sqlite");
        let mut db = ProjectDatabase::create_fresh(&path).unwrap();
        db.connection_mut()
            .execute(
                "INSERT INTO projects (id, name, task_prefix, repository_root, git_common_dir, main_branch, schema_version, created_at, updated_at)
                 VALUES (?1, 'p', 'CTX', '/r', '/r/.git', 'main', 18, 'now', 'now')",
                [project_id],
            )
            .unwrap();
        db.connection_mut()
            .execute(
                "INSERT INTO tasks (id, project_id, display_id, title, status, priority, metadata_json, created_at, updated_at)
                 VALUES ('t1', ?1, 'CTX-0001', 't', 'planned', 'normal', '{}', 'now', 'now')",
                [project_id],
            )
            .unwrap();
        (dir, path)
    }

    #[test]
    fn tombstone_validator_accepts_absent_rows_and_rejects_present_rows() {
        let (_dir, path) = seeded_candidate("01PROJECT");
        let db = ProjectDatabase::open(&path).unwrap();
        db.connection()
            .execute(
                "INSERT INTO tombstones (project_id, table_name, row_id, deleted_at)
                 VALUES ('01PROJECT', 'tasks', 'gone', '2026-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        assert!(validate_tombstones(&path).is_ok());

        db.connection()
            .execute(
                "INSERT INTO tombstones (project_id, table_name, row_id, deleted_at)
                 VALUES ('01PROJECT', 'tasks', 't1', '2026-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        let error = validate_tombstones(&path).unwrap_err();
        assert_eq!(error.code, "VALIDATION_FAILED");
        assert_eq!(error.details["kind"], "tombstone_inconsistent");
    }

    #[test]
    fn worktree_reconciliation_prunes_missing_paths_and_taken_slots() {
        let ours = BTreeSet::new();
        let rows = vec![
            json!({"id": "a", "project_id": "p", "task_id": "t1", "normalized_path": "/tmp"}),
            json!({"id": "b", "project_id": "p", "normalized_path": "/definitely/missing"}),
            json!({"id": "c", "project_id": "p", "task_id": "t1", "normalized_path": "/tmp"}),
        ];
        let (kept, pruned) = reconcile_worktrees(rows, &ours);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0]["id"], "a");
        assert_eq!(pruned.len(), 2);
        // `c` reuses the winner's active task slot; `b` has no live path.
        assert!(pruned.iter().any(|p| p.reason == "active_task_slot_taken"));
        assert!(pruned.iter().any(|p| p.reason == "directory_missing"));
    }

    #[test]
    fn base_source_strings_are_stable() {
        assert_eq!(base_source_str(BaseSource::Explicit), "explicit");
        assert_eq!(base_source_str(BaseSource::Ancestor), "ancestor");
        assert_eq!(base_source_str(BaseSource::Snapshot), "snapshot");
        assert_eq!(base_source_str(BaseSource::None), "none");
    }

    #[test]
    fn export_id_detection_matches_ulid_shape() {
        assert!(looks_like_export_id("01M25NNVXXEGYFN59B5YKYXRHK"));
        assert!(!looks_like_export_id("refs/heads/main"));
        assert!(!looks_like_export_id("short"));
    }
}
