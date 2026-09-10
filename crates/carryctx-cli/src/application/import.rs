//! ctxpack import (format `carryctx-pack-dir` v1, Section 4).
//!
//! State machine: `validate bundle -> require git repo -> state exists? ->
//! fresh INIT path | initialized REFUSE-or-REPLACE path`.
//!
//! - Fresh repo (git repo, no `state.sqlite`): equivalent to `init` reusing
//!   the bundle's `project_id`/`task_prefix`/`name`, then load, then
//!   re-anchor `projects.repository_root/git_common_dir`, then
//!   `project.imported`. A `.carryctx/config.toml` with a _different_
//!   `project.id` refuses with `STATE_CONFLICT`.
//! - Initialized repo: bare import refuses (`STATE_CONFLICT`, exit 3) with a
//!   `--mode replace` hint. `--mode replace --yes` takes a verified
//!   `pre_import_*` backup, builds a candidate from the bundle, validates it
//!   with the same gate as `sync pull`, then atomically swaps via the
//!   restore journal pattern (`project.restore` kind, so the existing
//!   crash-recovery path heals interrupted imports). `--mode merge` returns
//!   `UNSUPPORTED_OPERATION` (exit 10).
//! - Re-anchor: `projects.repository_root/git_common_dir` rewritten to the
//!   target; `worktrees` rows whose `normalized_path` is missing at the
//!   target leave the live table with a warning and a `worktree.pruned`
//!   audit event; `sessions.working_directory` kept as history.
//! - `--dry-run` validates + returns a `would_replace` diff, writes nothing.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::adapter::filesystem;
use crate::adapter::git::GitCli;
use crate::adapter::sqlite::ProjectDatabase;
use crate::adapter::sqlite_repos::SqliteEventRepository;
use crate::adapter::xdg::XdgPaths;
use crate::application::interchange::{PackBundle, read_bundle};
use crate::domain::pack;
use crate::error::CarryCtxError;
use crate::repository::event::{EventRepository, NewEvent};

pub(crate) fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

pub(crate) fn new_id() -> String {
    ulid::Ulid::generate().to_string()
}

fn hostname() -> String {
    std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".into())
}

/// Insertion order satisfying every declared foreign key without ever
/// disabling enforcement: parents strictly before children, `events` last
/// (it references projects/agents/tasks), `sequences` after the tables its
/// `max+1` reconciliation scans. `tombstones` only references `projects`,
/// so it loads first (v1 bundles carry zero tombstone rows).
///
/// `teams`/`team_members` come before `tasks`: the
/// `tasks_reject_cross_project_team` trigger (and the composite
/// `tasks(project_id, team_id)` FK) requires the referenced team row to
/// exist in the same project when a task with `team_id` is inserted.
/// `teams.commander_agent_id` itself is deferred two-phase inside
/// `load_bundle_into_db` because it references `team_members` while
/// `team_members` references `teams`.
///
/// `worktrees` sorts before `sessions`/`checkpoints`: `sessions.worktree_id`
/// and `checkpoints.worktree_id` reference it, so inserting those tables
/// first aborted real snapshots with `FOREIGN KEY constraint failed`
/// (CTX-0137). Rows referencing a worktree dropped by the re-anchor prune
/// policy are handled by `insert_rows_nulling_pruned_worktree_refs`.
pub(crate) const LOAD_ORDER: &[&str] = &[
    "tombstones",
    "agents",
    "teams",
    "team_members",
    "tasks",
    "task_dependencies",
    "progress_items",
    "worktrees",
    "sessions",
    "checkpoints",
    "checkpoint_corrections",
    "scopes",
    "decisions",
    "handoffs",
    "graph_nodes",
    "graph_edges",
    "events",
    "sequences",
];

/// Resolve the `--mode` flag to its Section 4 meaning.
///
/// `None` is the bare call (refuses on initialized targets, succeeds on
/// fresh ones). `Some("replace")` replaces whole state (requires `--yes`).
/// `Some("merge")` runs the CTX-0142 three-way merge path. Any other value is
/// `INVALID_ARGUMENTS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImportMode {
    Bare,
    Replace,
    Merge,
}

fn resolve_mode(mode: Option<&str>) -> Result<ImportMode, CarryCtxError> {
    match mode {
        None => Ok(ImportMode::Bare),
        Some("replace") => Ok(ImportMode::Replace),
        Some("merge") => Ok(ImportMode::Merge),
        Some(other) => Err(CarryCtxError::invalid_arguments(format!(
            "Unknown import mode '{other}'; expected 'replace' or 'merge'."
        ))),
    }
}

/// Read the local project id when a database exists.
fn local_project_id(db_path: &Path) -> Result<Option<String>, CarryCtxError> {
    if !db_path.exists() {
        return Ok(None);
    }
    let db = ProjectDatabase::open_readonly(db_path)?;
    let id: Result<String, _> =
        db.connection()
            .query_row("SELECT id FROM projects LIMIT 1", [], |row| row.get(0));
    match id {
        Ok(id) => Ok(Some(id)),
        Err(_) => Ok(None),
    }
}

/// Read `project.id` from an existing `.carryctx/config.toml`, if present.
///
/// A missing file, an unparsable file, or a file without `project.id` yields
/// `None` (the fresh path overwrites it); only a present non-empty id can
/// conflict with the bundle identity.
fn config_project_id(config_path: &Path) -> Option<String> {
    let raw = fs::read_to_string(config_path).ok()?;
    let value: toml::Value = toml::from_str(&raw).ok()?;
    value
        .get("project")?
        .get("id")?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Top-level import entry point (Section 4 state machine).
///
/// `project_path` is the caller's work dir (`--project` or cwd);
/// `bundle_dir` is the positional `<dir>` argument. `mode` is the raw
/// `--mode` flag value. `dry_run`/`yes` come from the global flags.
/// `merge_options` carries the `--mode merge` knobs. Writes nothing when
/// `dry_run` is true.
#[allow(clippy::too_many_arguments)]
pub fn import_project(
    project_path: &Path,
    bundle_dir: &Path,
    mode: Option<&str>,
    dry_run: bool,
    yes: bool,
    merge_options: &crate::application::merge_import::MergeImportOptions<'_>,
    actor_agent_id: Option<String>,
    session_id: Option<String>,
) -> Result<serde_json::Value, CarryCtxError> {
    // 1. Validate the bundle first (fail closed: VALIDATION_FAILED exit 8,
    //    future format_version -> UNSUPPORTED_OPERATION exit 10).
    let bundle = read_bundle(bundle_dir)?;
    let requested = resolve_mode(mode)?;

    if requested != ImportMode::Merge
        && (merge_options.base.is_some()
            || merge_options.require_base
            || merge_options.strict_edits)
    {
        return Err(CarryCtxError::invalid_arguments(
            "--base, --require-base, and --strict-edits require --mode merge.",
        ));
    }
    // Redacted bundles are publication artifacts and never merge sources
    // (design §2.3, §3.6); fresh/replace may still accept them.
    if requested == ImportMode::Merge && bundle.manifest.redacted {
        return Err(CarryCtxError::unsupported_operation(
            "Redacted bundles are publication artifacts and cannot be used as merge sources; import the unredacted bundle instead.",
        ));
    }

    // 2. Require a git repo (GIT_ERROR exit 4 otherwise).
    let git = GitCli::new();
    let gp = git.discover(project_path)?;
    let xdg = XdgPaths::new();
    let db_path = xdg.project_db(&gp.git_common_dir);
    let initialized = db_path.exists();

    // Bundle identity must match the project row it carries; otherwise a
    // hand-edited directory could silently fork identity.
    bundle_project_matches_manifest(&bundle)?;

    if !initialized {
        // `--mode merge` needs a live database to merge into; a fresh target
        // must go through a bare import (or `init`) first. Refuse instead of
        // silently treating merge as a fresh import.
        if requested == ImportMode::Merge {
            return Err(CarryCtxError::state_conflict(format!(
                "Project at '{}' is not initialized; `--mode merge` requires an existing state.sqlite. Initialize with a bare import (`carryctx import <dir>`) first.",
                gp.repository_root.display()
            ))
            .with_suggestions([
                "Initialize the target with `carryctx init` or a bare import, then re-run with --mode merge.".to_string(),
            ]));
        }
        if dry_run {
            return dry_run_diff(&bundle, &gp, &db_path, &requested);
        }
        return fresh_import(&bundle, &gp, &xdg, &db_path);
    }

    // Initialized target.
    match requested {
        ImportMode::Bare => {
            if dry_run {
                return dry_run_diff(&bundle, &gp, &db_path, &requested);
            }
            Err(CarryCtxError::state_conflict(format!(
                "Project at '{}' is already initialized; refusing to overwrite. Re-run with --mode replace --yes to replace it from '{}'.",
                gp.repository_root.display(),
                bundle.dir.display(),
            ))
            .with_suggestions(["Re-run with --mode replace --yes to replace the project state.".to_string()]))
        }
        ImportMode::Replace => {
            if dry_run {
                return dry_run_diff(&bundle, &gp, &db_path, &requested);
            }
            if !yes {
                return Err(CarryCtxError::state_conflict(
                    "Replacing project state requires explicit confirmation with --mode replace --yes.",
                )
                .with_suggestions([
                    "Re-run with --mode replace --yes to replace the project state.".to_string(),
                ]));
            }
            replace_import(&bundle, &gp, &xdg, &db_path)
        }
        ImportMode::Merge => crate::application::merge_import::merge_import(
            &bundle,
            &gp,
            &xdg,
            &db_path,
            merge_options,
            dry_run,
            actor_agent_id,
            session_id,
        ),
    }
}

/// The bundle's `project.json` id must equal the manifest `project_id`.
pub(crate) fn bundle_project_matches_manifest(bundle: &PackBundle) -> Result<(), CarryCtxError> {
    let row_id = bundle
        .project
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if row_id.is_empty() {
        return Err(CarryCtxError::validation_error(
            "Pack project row is missing a non-empty 'id'.".to_string(),
        ));
    }
    if row_id != bundle.manifest.project_id {
        return Err(CarryCtxError::validation_error(format!(
            "Pack project id '{row_id}' does not match manifest project_id '{}'.",
            bundle.manifest.project_id
        )));
    }
    Ok(())
}

/// `--dry-run`: full validation + diff summary, no writes.
///
/// `would_replace` is true exactly when a database already exists at the
/// target. `dropped_worktrees` lists the bundle worktree rows whose
/// `normalized_path` does not exist at this machine (they would leave the
/// live table under the Section 4 policy).
fn dry_run_diff(
    bundle: &PackBundle,
    gp: &crate::adapter::git::GitProject,
    db_path: &Path,
    requested: &ImportMode,
) -> Result<serde_json::Value, CarryCtxError> {
    let initialized = db_path.exists();
    let local = local_project_id(db_path)?;
    let worktree_rows = bundle.tables.get("worktrees").cloned().unwrap_or_default();
    let (_, pruned) = pack::prune_worktrees(worktree_rows, |path| Path::new(path).exists());
    let dropped: Vec<serde_json::Value> = pruned
        .iter()
        .map(|row| {
            serde_json::json!({
                "id": row.get("id").cloned().unwrap_or(serde_json::Value::Null),
                "normalized_path": row.get("normalized_path").cloned().unwrap_or(serde_json::Value::Null),
            })
        })
        .collect();
    Ok(serde_json::json!({
        "bundleDir": bundle.dir.to_string_lossy(),
        "bundleProjectId": bundle.manifest.project_id,
        "localProjectId": local,
        "would_replace": initialized,
        "mode": match requested {
            ImportMode::Bare => serde_json::Value::Null,
            ImportMode::Replace => serde_json::json!("replace"),
            ImportMode::Merge => serde_json::json!("merge"),
        },
        "counts": bundle.actual_counts(),
        "droppedWorktrees": dropped,
        "repositoryRoot": gp.repository_root.to_string_lossy(),
        "gitCommonDir": gp.git_common_dir.to_string_lossy(),
        "operation": {"applied": false},
    }))
}

/// Fresh-repo path: `init` reusing the bundle identity, then load, then
/// re-anchor, then `project.imported`.
fn fresh_import(
    bundle: &PackBundle,
    gp: &crate::adapter::git::GitProject,
    xdg: &XdgPaths,
    db_path: &Path,
) -> Result<serde_json::Value, CarryCtxError> {
    let repository_root = &gp.repository_root;
    let git_common_dir = &gp.git_common_dir;

    // Never silently fork identity: an existing config.toml with a different
    // project.id refuses with STATE_CONFLICT.
    let config_path = repository_root.join(".carryctx").join("config.toml");
    if let Some(config_id) = config_project_id(&config_path) {
        if config_id != bundle.manifest.project_id {
            return Err(CarryCtxError::state_conflict(format!(
                "Bundle project '{}' does not match existing config project '{config_id}'; refusing to fork identity.",
                bundle.manifest.project_id
            )));
        }
    }

    let _admission_lock = filesystem::AdmissionLock::acquire(
        &xdg.admission_lock_dir(git_common_dir),
        &new_id(),
        std::process::id(),
        &hostname(),
        &now(),
    )?;

    // Re-check after acquiring the lock: a concurrent init could have
    // created the database between our first check and now.
    if db_path.exists() {
        return Err(CarryCtxError::state_conflict(format!(
            "Project at '{}' is already initialized; refusing to overwrite. Re-run with --mode replace --yes to replace it.",
            repository_root.display()
        )));
    }

    let project_obj = bundle.project.as_object().cloned().ok_or_else(|| {
        CarryCtxError::validation_error("Pack project row must be a JSON object.".to_string())
    })?;
    let project_name = project_obj
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("imported-project")
        .to_string();
    let task_prefix = project_obj
        .get("task_prefix")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("CTX")
        .to_string();
    if let Err(msg) = crate::domain::ids::validate_task_prefix(&task_prefix) {
        return Err(CarryCtxError::validation_error(format!(
            "Bundle task prefix '{task_prefix}' is invalid: {msg}"
        )));
    }

    // Mirror `init`: config + README + .gitignore + registry, but reusing
    // the bundle identity instead of minting a fresh one.
    let carryctx_dir = repository_root.join(".carryctx");
    filesystem::ensure_dir(&carryctx_dir)?;
    let config_content = crate::application::init::build_config_toml(
        &bundle.manifest.project_id,
        &project_name,
        &task_prefix,
        gp,
    )?;
    filesystem::write_atomic(&config_path, config_content.as_bytes())?;
    let readme_path = carryctx_dir.join("README.md");
    if !readme_path.exists() {
        let readme_content = [
            "# CarryCtx\n",
            "\n",
            "<!-- carryctx:v1 -->\n",
            "\n",
            "This directory stores versioned CarryCtx project configuration committed to Git.\n",
            "It is safe to commit `.carryctx/` — it holds shared config, presets, and rules.\n",
            "\n",
            "Runtime state (tasks, agents, events, sessions) lives in\n",
            "`<git-common-dir>/carryctx/state.sqlite`, not in `.carryctx`.\n",
            "Do not edit `state.sqlite` by hand; use `carryctx` commands or MCP tools.\n",
            "\n",
            "- Repository: https://github.com/Xuepoo/carryctx\n",
            "- Documentation: https://carryctx.xuepoo.xyz\n",
            "  (see `carryctx-docs/configuration.md` for storage and XDG layout)\n",
        ]
        .concat();
        filesystem::write_atomic(&readme_path, readme_content.as_bytes())?;
    }
    crate::application::init::ensure_gitignore_rule(&repository_root.join(".gitignore"))?;

    let state_dir = xdg.project_state_dir(git_common_dir);
    filesystem::ensure_dir(&state_dir)?;
    let mut db = ProjectDatabase::create_fresh(db_path)?;

    let warnings = {
        let uow = db.begin_unit_of_work()?;
        let warnings = load_bundle_into_db(
            uow.connection(),
            bundle,
            &repository_root.to_string_lossy(),
            &git_common_dir.to_string_lossy(),
        )?;
        uow.commit()?;
        warnings
    };

    crate::application::init::register_in_registry(
        &xdg.registry_db(),
        &bundle.manifest.project_id,
        repository_root,
        git_common_dir,
        &config_path,
        &now(),
    )?;

    Ok(serde_json::json!({
        "projectId": bundle.manifest.project_id,
        "projectName": project_name,
        "taskPrefix": task_prefix,
        "mode": "init",
        "counts": bundle.actual_counts(),
        "path": db_path.to_string_lossy(),
        "warnings": warnings,
        "operation": {"applied": true},
    }))
}

/// Initialized `--mode replace --yes` path: verified `pre_import_*` backup,
/// candidate built from the bundle, same validation gate as `sync pull`,
/// then an atomic restore-journal swap.
#[allow(clippy::too_many_lines)]
fn replace_import(
    bundle: &PackBundle,
    gp: &crate::adapter::git::GitProject,
    xdg: &XdgPaths,
    db_path: &Path,
) -> Result<serde_json::Value, CarryCtxError> {
    let repository_root = &gp.repository_root;
    let git_common_dir = &gp.git_common_dir;
    let _admission_lock = filesystem::AdmissionLock::acquire(
        &xdg.admission_lock_dir(git_common_dir),
        &new_id(),
        std::process::id(),
        &hostname(),
        &now(),
    )?;
    let operation_id = new_id();

    // Verified pre-import backup (mirrors `sync pull`).
    let backup_dir = xdg.backup_dir(git_common_dir);
    filesystem::ensure_dir(&backup_dir)?;
    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let pre_backup_path = backup_dir.join(format!("pre_import_{timestamp}_{operation_id}.sqlite"));
    {
        let current = ProjectDatabase::open_readonly(db_path)?;
        current.create_backup(&pre_backup_path)?;
        drop(current);
        crate::application::project_mgmt::validate_database_for_sync(&pre_backup_path)?;
        checkpoint_database(db_path)?;
        remove_sidecars(db_path);
    }

    // Stage the candidate beside the live database and journal the swap via
    // the restore pattern, so an interrupted import heals on next open.
    let candidate_path = sibling_path(db_path, &format!("restore_{operation_id}"));
    let original_path = sibling_path(db_path, &format!("original_{operation_id}"));
    let journal_dir = xdg.journal_dir(git_common_dir);
    filesystem::write_journal(
        &journal_dir,
        &filesystem::JournalEntry {
            operation_id: operation_id.clone(),
            kind: "project.restore".into(),
            status: "prepared".into(),
            created_at: now(),
            metadata: serde_json::json!({
                "backupPath": bundle.dir.to_string_lossy(),
                "databasePath": db_path.to_string_lossy(),
                "candidatePath": candidate_path.to_string_lossy(),
                "originalPath": original_path.to_string_lossy(),
                "importBundle": bundle.dir.to_string_lossy(),
                "preImportBackupPath": pre_backup_path.to_string_lossy(),
            }),
        },
    )?;

    let build_result = (|| -> Result<Vec<String>, CarryCtxError> {
        // Fresh candidate at the staged path, then load the bundle into it.
        let mut candidate = ProjectDatabase::create_fresh(&candidate_path)?;
        let warnings = {
            // `create_fresh` leaves a WAL sidecar; checkpoint it away before
            // the bulk load so the candidate starts from a clean image.
            candidate
                .connection()
                .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
                .map_err(|e| {
                    CarryCtxError::database_error(format!("Candidate checkpoint failed: {e}"))
                })?;
            // Bulk load inside one immediate transaction with the audit
            // events appended atomically alongside the rows.
            let tx = candidate.connection_mut().transaction().map_err(|e| {
                CarryCtxError::database_error(format!("Candidate transaction failed: {e}"))
            })?;
            let warnings = load_bundle_into_db(
                &tx,
                bundle,
                &repository_root.to_string_lossy(),
                &git_common_dir.to_string_lossy(),
            )?;
            tx.commit().map_err(|e| {
                CarryCtxError::database_error(format!("Candidate commit failed: {e}"))
            })?;
            warnings
        };
        candidate
            .connection()
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .map_err(|e| {
                CarryCtxError::database_error(format!("Candidate checkpoint failed: {e}"))
            })?;
        drop(candidate);
        // Same gate as `sync pull`: integrity, FK, exactly one well-formed
        // project row, migration compat.
        crate::application::project_mgmt::validate_database_for_sync(&candidate_path)?;
        Ok(warnings)
    })();
    let warnings = match build_result {
        Ok(warnings) => warnings,
        Err(error) => {
            remove_database_files(&candidate_path);
            let _ = filesystem::remove_journal(&journal_dir, &operation_id);
            return Err(error);
        }
    };

    if let Err(error) = fs::hard_link(db_path, &original_path) {
        remove_database_files(&candidate_path);
        let _ = filesystem::remove_journal(&journal_dir, &operation_id);
        return Err(CarryCtxError::database_error(format!(
            "Failed to preserve active database: {error}"
        )));
    }
    if let Err(error) = fs::rename(&candidate_path, db_path) {
        remove_database_files(&candidate_path);
        let _ = fs::remove_file(&original_path);
        let _ = filesystem::remove_journal(&journal_dir, &operation_id);
        return Err(CarryCtxError::database_error(format!(
            "Failed to atomically swap imported database: {error}"
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
            metadata: serde_json::json!({
                "backupPath": bundle.dir.to_string_lossy(),
                "databasePath": db_path.to_string_lossy(),
                "preImportBackupPath": pre_backup_path.to_string_lossy(),
            }),
        },
    )?;
    filesystem::remove_journal(&journal_dir, &operation_id)?;

    Ok(serde_json::json!({
        "projectId": bundle.manifest.project_id,
        "mode": "replace",
        "counts": bundle.actual_counts(),
        "path": db_path.to_string_lossy(),
        "preImportBackupPath": pre_backup_path.to_string_lossy(),
        "warnings": warnings,
        "operation": {"applied": true},
    }))
}

/// Load every bundle row into an empty database connection (fresh or
/// candidate), applying the Section 4 re-anchor policy.
///
/// The caller owns the surrounding transaction; the `project.imported` and
/// `worktree.pruned` audit events are appended in the same transaction so a
/// failure rolls back rows and events together. Returns non-fatal warnings
/// (pruned worktrees) for the envelope.
fn load_bundle_into_db(
    conn: &rusqlite::Connection,
    bundle: &PackBundle,
    repository_root: &str,
    git_common_dir: &str,
) -> Result<Vec<String>, CarryCtxError> {
    // Project row first (every other row references it).
    let mut project_map = bundle.project.as_object().cloned().ok_or_else(|| {
        CarryCtxError::validation_error("Pack project row must be a JSON object.".to_string())
    })?;
    pack::reanchor_project(&mut project_map, repository_root, git_common_dir);
    insert_row(conn, "projects", &serde_json::Value::Object(project_map))?;

    let mut warnings = Vec::new();

    // Worktree prune per the re-anchor policy: rows whose normalized_path
    // does not exist at the target leave the live table. Absolute paths are
    // never compared across machines; existence here is the only signal.
    let worktree_rows = bundle.tables.get("worktrees").cloned().unwrap_or_default();
    let (kept_worktrees, pruned_worktrees) =
        pack::prune_worktrees(worktree_rows, |path| Path::new(path).exists());
    for row in &pruned_worktrees {
        let id = row
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("<unknown>");
        let path = row
            .get("normalized_path")
            .and_then(|v| v.as_str())
            .unwrap_or("<missing>");
        warnings.push(format!(
            "Pruned worktree {id} ({path}): directory missing at import target."
        ));
    }
    let kept_worktree_ids: std::collections::HashSet<&str> = kept_worktrees
        .iter()
        .filter_map(|row| row.get("id").and_then(|v| v.as_str()))
        .collect();

    for table in LOAD_ORDER {
        match *table {
            "worktrees" => {
                for row in &kept_worktrees {
                    insert_row(conn, table, row)?;
                }
            }
            // `sessions.worktree_id` and `checkpoints.worktree_id` reference
            // worktrees that the re-anchor policy may have pruned on this
            // machine (Section 4). Null those links and keep the history
            // rows; every other FK stays strictly enforced (fail closed).
            "sessions" | "checkpoints" => {
                insert_rows_nulling_pruned_worktree_refs(
                    conn,
                    table,
                    bundle.tables.get(*table).map(Vec::as_slice),
                    &kept_worktree_ids,
                    &mut warnings,
                )?;
            }
            // `teams`/`team_members` form a cycle: `team_members` references
            // `teams`, while `teams.commander_agent_id` references
            // `team_members`. Mirror the live `create` path: insert teams
            // with a NULL commander, insert members, then restore each
            // commander via UPDATE (fail-closed on dangling references).
            // `tasks` sorts after both in LOAD_ORDER so its
            // same-project-team trigger sees the team rows.
            "teams" => {
                load_teams_cycle(
                    conn,
                    bundle.tables.get("teams").map(Vec::as_slice),
                    bundle.tables.get("team_members").map(Vec::as_slice),
                )?;
            }
            "team_members" => {
                // Already loaded alongside `teams` above (cycle-safe order).
            }
            // Legacy sources can carry `events.task_id` values whose task is
            // gone (manual deletes predate the prune-time unlink;
            // `prune_project` nulls these links while keeping the audit
            // rows). Converge to that same state: null task links absent
            // from the bundle, keep every row. `project_id` and
            // `actor_agent_id` stay strictly enforced.
            "events" => {
                let mut known_tasks = std::collections::HashSet::new();
                if let Some(rows) = bundle.tables.get("tasks") {
                    for row in rows {
                        if let Some(id) = row.get("id").and_then(|v| v.as_str()) {
                            known_tasks.insert(id.to_string());
                        }
                    }
                }
                let mut nulled = 0u64;
                if let Some(rows) = bundle.tables.get("events") {
                    for row in rows {
                        let mut fixed = row.clone();
                        let dangling = row
                            .get("task_id")
                            .and_then(|v| v.as_str())
                            .is_some_and(|id| !known_tasks.contains(id));
                        if dangling {
                            if let Some(object) = fixed.as_object_mut() {
                                object.insert("task_id".to_string(), serde_json::Value::Null);
                            }
                            nulled += 1;
                        }
                        insert_row(conn, "events", &fixed)?;
                    }
                }
                if nulled > 0 {
                    warnings.push(format!(
                        "Nulled {nulled} dangling event task reference(s) (referenced task absent from bundle; audit rows kept)."
                    ));
                }
            }
            // Sequences are reconciled as max+1 below; skip the raw rows
            // here so a stale bundle value can never rewind the counters.
            "sequences" => {}
            _ => {
                if let Some(rows) = bundle.tables.get(*table) {
                    for row in rows {
                        insert_row(conn, table, row)?;
                    }
                }
            }
        }
    }

    reconcile_sequences(conn, &bundle.manifest.project_id, bundle)?;

    // Append-only audit: historical rows are already in `events`; the import
    // itself appends exactly one `project.imported` event (same transaction).
    let project_id = bundle.manifest.project_id.clone();
    SqliteEventRepository::new(conn)
        .append(&NewEvent {
            id: new_id(),
            project_id: project_id.clone(),
            event_type: "project.imported".into(),
            actor_agent_id: None,
            session_id: None,
            task_id: None,
            payload: serde_json::json!({
                "exportId": bundle.manifest.export_id,
                "formatVersion": bundle.source_format_version,
                "counts": bundle.actual_counts(),
            }),
            occurred_at: now(),
        })
        .map_err(|e| {
            CarryCtxError::database_error(format!("Failed to append project.imported event: {e}"))
        })?;

    // Each pruned worktree is audited as `worktree.pruned` (same shape as the
    // stale-worktree pruner so `doctor` and event queries stay uniform).
    for row in &pruned_worktrees {
        let worktree_id = row.get("id").and_then(|v| v.as_str()).unwrap_or_default();
        if worktree_id.is_empty() {
            continue;
        }
        let task_id = row
            .get("task_id")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let path = row
            .get("normalized_path")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let payload = serde_json::json!({
            "worktree_id": worktree_id,
            "path": path,
            "task_id": task_id,
            "reason": "directory_missing",
        });
        let payload_str = serde_json::to_string(&payload).map_err(|e| {
            CarryCtxError::database_error(format!("Failed to serialize prune payload: {e}"))
        })?;
        conn.execute(
            "INSERT INTO events (id, project_id, type, aggregate_type, aggregate_id, payload_json, actor_agent_id, session_id, task_id, occurred_at)
             VALUES (?1, ?2, 'worktree.pruned', 'worktree', ?3, ?4, NULL, NULL, ?5, ?6)",
            rusqlite::params![
                new_id(),
                project_id,
                worktree_id,
                payload_str,
                task_id,
                now(),
            ],
        )
        .map_err(|e| {
            CarryCtxError::database_error(format!("Failed to append worktree.pruned event: {e}"))
        })?;
    }

    Ok(warnings)
}

/// Insert `teams` and `team_members` across their mutual foreign key: teams
/// first with a NULL commander, then members, then restore each commander.
///
/// Extracted from [`load_bundle_into_db`] so the CTX-0142 merge candidate
/// builder inserts the merged rows through the same cycle-safe path.
pub(crate) fn load_teams_cycle(
    conn: &rusqlite::Connection,
    teams: Option<&[serde_json::Value]>,
    members: Option<&[serde_json::Value]>,
) -> Result<(), CarryCtxError> {
    let mut deferred: Vec<(String, String, String)> = Vec::new();
    if let Some(rows) = teams {
        for row in rows {
            let mut nulled = row.clone();
            if let Some(object) = nulled.as_object_mut() {
                let commander = object
                    .get("commander_agent_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                if let Some(commander) = commander {
                    let project_id = object
                        .get("project_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let team_id = object
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    deferred.push((project_id, team_id, commander));
                    object.insert("commander_agent_id".to_string(), serde_json::Value::Null);
                }
            }
            insert_row(conn, "teams", &nulled)?;
        }
    }
    if let Some(rows) = members {
        for row in rows {
            insert_row(conn, "team_members", row)?;
        }
    }
    for (project_id, team_id, commander) in &deferred {
        conn.execute(
            "UPDATE teams SET commander_agent_id = ?1 WHERE project_id = ?2 AND id = ?3",
            rusqlite::params![commander, project_id, team_id],
        )
        .map_err(|e| {
            CarryCtxError::database_error(format!("Failed to load pack table 'teams': {e}"))
        })?;
    }
    Ok(())
}

/// Insert one bundle row into `table`.
///
/// Columns come from `PRAGMA table_info` (never from the bundle), and every
/// value is bound as a parameter. Bundle keys that name no such column
/// refuse the bundle fail-closed; missing columns are omitted so column
/// defaults apply (an older bundle stays loadable after additive
/// migrations). JSON objects/arrays serialize to their TEXT columns
/// (`*_json`, `metadata`, ...); booleans become 0/1.
pub(crate) fn insert_row(
    conn: &rusqlite::Connection,
    table: &str,
    row: &serde_json::Value,
) -> Result<(), CarryCtxError> {
    if !is_known_table(table) {
        return Err(CarryCtxError::validation_error(format!(
            "Pack table '{table}' is not a known interchange table."
        )));
    }
    let object = row.as_object().ok_or_else(|| {
        CarryCtxError::validation_error(format!("Pack table '{table}' row must be a JSON object."))
    })?;
    for key in object.keys() {
        if key == "rowid" {
            return Err(CarryCtxError::validation_error(format!(
                "Pack table '{table}' row must not carry 'rowid'."
            )));
        }
    }

    let columns = table_columns(conn, table)?;
    let column_set: std::collections::HashSet<&str> = columns.iter().map(String::as_str).collect();
    for key in object.keys() {
        if !column_set.contains(key.as_str()) {
            return Err(CarryCtxError::validation_error(format!(
                "Pack table '{table}' row has unknown column '{key}'."
            )));
        }
    }

    // Only present columns enter the INSERT so absent ones fall back to
    // their schema defaults; explicit JSON nulls bind as SQL NULL.
    let mut names: Vec<&str> = Vec::new();
    let mut values: Vec<rusqlite::types::Value> = Vec::new();
    for column in &columns {
        if let Some(json) = object.get(column) {
            names.push(column.as_str());
            values.push(json_to_sql(json)?);
        }
    }
    if names.is_empty() {
        return Err(CarryCtxError::validation_error(format!(
            "Pack table '{table}' row carries no known columns."
        )));
    }
    let placeholders: Vec<String> = (1..=names.len()).map(|i| format!("?{i}")).collect();
    let sql = format!(
        "INSERT INTO {table} ({}) VALUES ({})",
        names.join(", "),
        placeholders.join(", ")
    );
    let params: Vec<&dyn rusqlite::ToSql> =
        values.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
    conn.execute(&sql, params.as_slice()).map_err(|e| {
        CarryCtxError::database_error(format!("Failed to load pack table '{table}': {e}"))
    })?;
    Ok(())
}

/// Insert `sessions`/`checkpoints` rows, nulling `worktree_id` links that
/// name a worktree which is not live at the import target (dropped by the
/// Section 4 re-anchor prune, or absent from the bundle) and recording an
/// aggregated warning.
///
/// The history row is always kept; only the link to an entity deliberately
/// dropped at the target is removed, matching the dangling-`events.task_id`
/// convergence above. Export stays lossless — whether a worktree path exists
/// is a property of the import machine, not of the bundle — and every other
/// foreign key remains strictly enforced so a genuinely inconsistent bundle
/// still fails closed.
pub(crate) fn insert_rows_nulling_pruned_worktree_refs(
    conn: &rusqlite::Connection,
    table: &str,
    rows: Option<&[serde_json::Value]>,
    kept_worktree_ids: &std::collections::HashSet<&str>,
    warnings: &mut Vec<String>,
) -> Result<(), CarryCtxError> {
    let Some(rows) = rows else {
        return Ok(());
    };
    let mut nulled = 0u64;
    for row in rows {
        let dangling = row
            .get("worktree_id")
            .and_then(|v| v.as_str())
            .is_some_and(|id| !kept_worktree_ids.contains(id));
        if dangling {
            let mut fixed = row.clone();
            if let Some(object) = fixed.as_object_mut() {
                object.insert("worktree_id".to_string(), serde_json::Value::Null);
            }
            nulled += 1;
            insert_row(conn, table, &fixed)?;
        } else {
            insert_row(conn, table, row)?;
        }
    }
    if nulled > 0 {
        let entity = match table {
            "sessions" => "session",
            "checkpoints" => "checkpoint",
            other => other,
        };
        warnings.push(format!(
            "Nulled {nulled} {entity} worktree reference(s) to worktrees not live at the import target (pruned or absent from bundle); history rows kept."
        ));
    }
    Ok(())
}

pub(crate) fn is_known_table(table: &str) -> bool {
    matches!(
        table,
        "projects"
            | "agents"
            | "tasks"
            | "task_dependencies"
            | "progress_items"
            | "sessions"
            | "worktrees"
            | "checkpoints"
            | "checkpoint_corrections"
            | "scopes"
            | "decisions"
            | "handoffs"
            | "teams"
            | "team_members"
            | "graph_nodes"
            | "graph_edges"
            | "events"
            | "sequences"
            | "tombstones"
    )
}

fn table_columns(conn: &rusqlite::Connection, table: &str) -> Result<Vec<String>, CarryCtxError> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|e| {
            CarryCtxError::database_error(format!("Failed to inspect table '{table}': {e}"))
        })?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| {
            CarryCtxError::database_error(format!("Failed to inspect table '{table}': {e}"))
        })?;
    let mut columns = Vec::new();
    for row in rows {
        columns.push(row.map_err(|e| {
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

pub(crate) fn json_to_sql(
    value: &serde_json::Value,
) -> Result<rusqlite::types::Value, CarryCtxError> {
    match value {
        serde_json::Value::Null => Ok(rusqlite::types::Value::Null),
        serde_json::Value::Bool(b) => Ok(rusqlite::types::Value::Integer(i64::from(*b))),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(rusqlite::types::Value::Integer(i))
            } else if let Some(u) = n.as_u64() {
                i64::try_from(u)
                    .map(rusqlite::types::Value::Integer)
                    .map_err(|_| {
                        CarryCtxError::validation_error(
                            "Pack numeric value is out of range.".to_string(),
                        )
                    })
            } else if let Some(f) = n.as_f64() {
                Ok(rusqlite::types::Value::Real(f))
            } else {
                Err(CarryCtxError::validation_error(
                    "Pack numeric value is invalid.".to_string(),
                ))
            }
        }
        serde_json::Value::String(s) => Ok(rusqlite::types::Value::Text(s.clone())),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => serde_json::to_string(value)
            .map(rusqlite::types::Value::Text)
            .map_err(|e| {
                CarryCtxError::validation_error(format!("Pack JSON value is invalid: {e}"))
            }),
    }
}

/// Reconcile `sequences` as `max+1` (display IDs are not identity).
///
/// Bundle `sequences` rows are loaded at `max(bundle value, computed max+1)`
/// so a bundle that already ran ahead is preserved while a stale or
/// hand-edited value can never rewind the counters and collide with future
/// allocations. Kinds:
/// - tasks: `display_id_<TASK_PREFIX>` (grouped by the prefix found in data)
/// - progress: `display_id_progress` (`PX-NNNN`)
/// - decisions: `display_id_decision` (`DEC-NNNN`)
/// - handoffs: `display_id_handoff` (`HO-NNNN`)
fn reconcile_sequences(
    conn: &rusqlite::Connection,
    project_id: &str,
    bundle: &PackBundle,
) -> Result<(), CarryCtxError> {
    reconcile_sequence_rows(
        conn,
        project_id,
        bundle.tables.get("sequences").map(Vec::as_slice),
    )
}

/// Reconcile `sequences` from a raw row slice, shared by replace import (the
/// bundle's rows) and the CTX-0142 merge candidate (the merged rows).
pub(crate) fn reconcile_sequence_rows(
    conn: &rusqlite::Connection,
    project_id: &str,
    sequences: Option<&[serde_json::Value]>,
) -> Result<(), CarryCtxError> {
    let mut bundle_sequences: BTreeMap<String, i64> = BTreeMap::new();
    if let Some(rows) = sequences {
        for row in rows {
            let (Some(kind), Some(next)) = (
                row.get("kind").and_then(|v| v.as_str()),
                row.get("next_value").and_then(|v| v.as_u64()),
            ) else {
                continue;
            };
            let next = i64::try_from(next).unwrap_or(i64::MAX);
            bundle_sequences
                .entry(kind.to_string())
                .and_modify(|v| *v = (*v).max(next))
                .or_insert(next);
        }
    }

    let mut required: BTreeMap<String, i64> = BTreeMap::new();
    // Tasks, grouped by display prefix.
    {
        let mut stmt = conn
            .prepare("SELECT display_id FROM tasks WHERE project_id = ?1")
            .map_err(|e| CarryCtxError::database_error(format!("Sequence scan failed: {e}")))?;
        let mut max_by_prefix: BTreeMap<String, i64> = BTreeMap::new();
        let rows = stmt
            .query_map([project_id], |row| row.get::<_, String>(0))
            .map_err(|e| CarryCtxError::database_error(format!("Sequence scan failed: {e}")))?;
        for row in rows {
            let display_id: String = row
                .map_err(|e| CarryCtxError::database_error(format!("Sequence scan failed: {e}")))?;
            if let Some((prefix, seq)) = split_display_id(&display_id) {
                max_by_prefix
                    .entry(prefix)
                    .and_modify(|v| *v = (*v).max(seq))
                    .or_insert(seq);
            }
        }
        for (prefix, max) in max_by_prefix {
            required.insert(format!("display_id_{prefix}"), max + 1);
        }
    }
    for (table, prefix, kind) in [
        ("progress_items", "PX", "display_id_progress"),
        ("decisions", "DEC", "display_id_decision"),
        ("handoffs", "HO", "display_id_handoff"),
    ] {
        let max = max_display_seq(conn, table, project_id, prefix)?;
        if let Some(max) = max {
            required.insert(kind.to_string(), max + 1);
        }
    }

    // Union of bundle kinds and computed kinds: never rewind below the
    // bundle value, never stay below computed max+1.
    let mut kinds: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    kinds.extend(bundle_sequences.keys().cloned());
    kinds.extend(required.keys().cloned());
    for kind in kinds {
        let floor = required.get(&kind).copied().unwrap_or(1).max(1);
        let next = bundle_sequences
            .get(&kind)
            .copied()
            .unwrap_or(1)
            .max(floor)
            .max(1);
        conn.execute(
            "INSERT INTO sequences (project_id, kind, next_value) VALUES (?1, ?2, ?3)
             ON CONFLICT(project_id, kind) DO UPDATE SET next_value = excluded.next_value",
            rusqlite::params![project_id, kind, next],
        )
        .map_err(|e| CarryCtxError::database_error(format!("Sequence reconcile failed: {e}")))?;
    }
    Ok(())
}

/// Split `PREFIX-NNNN` into `(prefix, number)`.
fn split_display_id(display_id: &str) -> Option<(String, i64)> {
    let dash = display_id.rfind('-')?;
    let prefix = display_id[..dash].trim();
    let num: i64 = display_id[dash + 1..].trim().parse().ok()?;
    if prefix.is_empty() || num < 0 {
        return None;
    }
    Some((prefix.to_string(), num))
}

fn max_display_seq(
    conn: &rusqlite::Connection,
    table: &str,
    project_id: &str,
    prefix: &str,
) -> Result<Option<i64>, CarryCtxError> {
    if !matches!(table, "progress_items" | "decisions" | "handoffs" | "tasks") {
        return Err(CarryCtxError::validation_error(format!(
            "Sequence scan of unknown table '{table}'."
        )));
    }
    let sql = format!("SELECT display_id FROM {table} WHERE project_id = ?1");
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| CarryCtxError::database_error(format!("Sequence scan failed: {e}")))?;
    let rows = stmt
        .query_map([project_id], |row| row.get::<_, String>(0))
        .map_err(|e| CarryCtxError::database_error(format!("Sequence scan failed: {e}")))?;
    let mut max: Option<i64> = None;
    for row in rows {
        let display_id: String =
            row.map_err(|e| CarryCtxError::database_error(format!("Sequence scan failed: {e}")))?;
        if let Some((found_prefix, seq)) = split_display_id(&display_id) {
            if found_prefix == prefix {
                max = Some(max.map_or(seq, |m: i64| m.max(seq)));
            }
        }
    }
    Ok(max)
}

fn checkpoint_database(path: &Path) -> Result<(), CarryCtxError> {
    let database = ProjectDatabase::open(path)?;
    database
        .connection()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .map_err(|e| CarryCtxError::database_error(format!("Database checkpoint failed: {e}")))
}

pub(crate) fn sibling_path(path: &Path, suffix: &str) -> PathBuf {
    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!("{file_name}.{suffix}"))
}

fn remove_database_files(path: &Path) {
    let _ = fs::remove_file(path);
    remove_sidecars(path);
}

pub(crate) fn remove_sidecars(path: &Path) {
    let _ = fs::remove_file(path.with_file_name(format!(
        "{}-wal",
        path.file_name().unwrap_or_default().to_string_lossy()
    )));
    let _ = fs::remove_file(path.with_file_name(format!(
        "{}-shm",
        path.file_name().unwrap_or_default().to_string_lossy()
    )));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn merge_mode_resolves() {
        assert_eq!(resolve_mode(Some("merge")).unwrap(), ImportMode::Merge);
    }

    #[test]
    fn unknown_mode_is_invalid_arguments() {
        let error = resolve_mode(Some("theirs")).unwrap_err();
        assert_eq!(error.code, "INVALID_ARGUMENTS");
    }

    #[test]
    fn bare_and_replace_modes_resolve() {
        assert_eq!(resolve_mode(None).unwrap(), ImportMode::Bare);
        assert_eq!(resolve_mode(Some("replace")).unwrap(), ImportMode::Replace);
    }

    #[test]
    fn reanchor_rewrites_only_anchors() {
        let mut project = serde_json::json!({
            "id": "01TEST",
            "name": "demo",
            "repository_root": "/old/root",
            "git_common_dir": "/old/root/.git",
        });
        let map = project.as_object_mut().unwrap();
        pack::reanchor_project(map, "/new/root", "/new/root/.git");
        assert_eq!(map["id"], "01TEST");
        assert_eq!(map["repository_root"], "/new/root");
        assert_eq!(map["git_common_dir"], "/new/root/.git");
    }

    #[test]
    fn prune_policy_drops_missing_paths() {
        let rows = vec![
            serde_json::json!({"id": "keep", "normalized_path": "/live"}),
            serde_json::json!({"id": "drop", "normalized_path": "/gone"}),
            serde_json::json!({"id": "no-path"}),
        ];
        let (kept, pruned) = pack::prune_worktrees(rows, |p| p == "/live");
        assert_eq!(kept.len(), 1);
        assert_eq!(pruned.len(), 2);
    }

    #[test]
    fn project_id_mismatch_between_row_and_manifest_refuses() {
        let dir = tempfile::tempdir().unwrap();
        write_minimal_bundle(
            dir.path(),
            "01BUNDLE",
            &serde_json::json!({"id": "01OTHER", "name": "x"}),
        );
        // read_bundle succeeds (it does not cross-check project.json id);
        // the import gate must catch the fork.
        let bundle = read_bundle(dir.path()).unwrap();
        let error = bundle_project_matches_manifest(&bundle).unwrap_err();
        assert_eq!(error.code, "VALIDATION_FAILED");
    }

    #[test]
    fn tampered_counts_refuse_with_validation_failed() {
        let dir = tempfile::tempdir().unwrap();
        let mut manifest = minimal_manifest("01BUNDLE", 5);
        manifest.counts.insert("tasks".to_string(), 5);
        write_bundle_with_manifest(dir.path(), &manifest, &[]);
        let error = read_bundle(dir.path()).unwrap_err();
        assert_eq!(error.code, "VALIDATION_FAILED");
    }

    #[test]
    fn future_format_version_maps_to_unsupported_operation() {
        let dir = tempfile::tempdir().unwrap();
        let mut manifest = minimal_manifest("01BUNDLE", 0);
        manifest.format_version = 999;
        write_bundle_with_manifest(dir.path(), &manifest, &[]);
        let error = read_bundle(dir.path()).unwrap_err();
        assert_eq!(error.code, "UNSUPPORTED_OPERATION");
    }

    #[test]
    fn sequences_reconcile_never_rewinds() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("seq.sqlite");
        let mut db = ProjectDatabase::create_fresh(&db_path).unwrap();
        let project_id = "01SEQ";
        db.connection_mut()
            .execute(
                "INSERT INTO projects (id, name, task_prefix, repository_root, git_common_dir, main_branch, schema_version, created_at, updated_at)
                 VALUES (?1, 's', 'CTX', '/r', '/r/.git', 'main', 17, 'now', 'now')",
                [project_id],
            )
            .unwrap();
        db.connection_mut()
            .execute(
                "INSERT INTO tasks (id, project_id, display_id, title, status, priority, metadata_json, created_at, updated_at)
                 VALUES ('t1', ?1, 'CTX-0041', 't', 'planned', 'normal', '{}', 'now', 'now')",
                [project_id],
            )
            .unwrap();
        // Bundle claims next_value 2 (stale); data needs 42.
        let mut tables = BTreeMap::new();
        tables.insert(
            "sequences".to_string(),
            vec![serde_json::json!({"project_id": project_id, "kind": "display_id_CTX", "next_value": 2})],
        );
        for table in pack::PACK_TABLE_FILES {
            tables.entry((*table).to_string()).or_insert_with(Vec::new);
        }
        let bundle = PackBundle {
            dir: dir.path().to_path_buf(),
            manifest: minimal_manifest(project_id, 0),
            source_format_version: pack::PACK_FORMAT_VERSION_V1,
            project: serde_json::json!({"id": project_id}),
            tables,
        };
        reconcile_sequences(db.connection(), project_id, &bundle).unwrap();
        let next: i64 = db
            .connection()
            .query_row(
                "SELECT next_value FROM sequences WHERE project_id = ?1 AND kind = 'display_id_CTX'",
                [project_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(next, 42);
    }

    // --- minimal bundle helpers (mirror interchange test shape) ---

    /// Minimal legacy (v1) bundle manifest, as an older writer would emit:
    /// no parents/tombstones/redaction fields, counts only for non-empty
    /// tables. Exercises the read-side v1 compatibility window.
    fn minimal_manifest(project_id: &str, tasks: u64) -> pack::PackManifest {
        pack::PackManifest::new_v1(
            "0.8.1",
            17,
            project_id,
            "01EXPORT",
            "2026-09-09T06:00:00Z",
            pack::PackSource {
                git_branch: None,
                git_commit: None,
                hostname: None,
            },
            if tasks > 0 {
                BTreeMap::from([("tasks".to_string(), tasks)])
            } else {
                BTreeMap::new()
            },
        )
    }

    fn write_bundle_with_manifest(
        root: &Path,
        manifest: &pack::PackManifest,
        task_rows: &[serde_json::Value],
    ) {
        fs::write(
            root.join(pack::PACK_MANIFEST_FILE),
            serde_json::to_string_pretty(manifest).unwrap(),
        )
        .unwrap();
        fs::write(
            root.join(pack::PACK_PROJECT_FILE),
            serde_json::json!({"id": manifest.project_id, "name": "demo"}).to_string(),
        )
        .unwrap();
        for table in pack::pack_table_files(manifest.format_version) {
            let rows: &[serde_json::Value] = if *table == "tasks" { task_rows } else { &[] };
            let mut text = String::new();
            for row in rows {
                text.push_str(&serde_json::to_string(row).unwrap());
                text.push('\n');
            }
            fs::write(root.join(format!("{table}.jsonl")), text).unwrap();
        }
    }

    fn write_minimal_bundle(root: &Path, manifest_id: &str, project_row: &serde_json::Value) {
        let manifest = minimal_manifest(manifest_id, 0);
        fs::write(
            root.join(pack::PACK_MANIFEST_FILE),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
        fs::write(
            root.join(pack::PACK_PROJECT_FILE),
            serde_json::to_string(project_row).unwrap(),
        )
        .unwrap();
        for table in pack::pack_table_files(manifest.format_version) {
            fs::write(root.join(format!("{table}.jsonl")), "").unwrap();
        }
    }
}
