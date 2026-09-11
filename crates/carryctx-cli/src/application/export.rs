//! ctxpack directory-layout writer (format `carryctx-pack-dir` v1/v2).
//!
//! Filesystem half of CTX-0112: dumps the live SQLite project database to
//! `<export-dir>/` per Section 2 of the design
//! (`2026-09-09-ctxpack-export-import.md`): `manifest.json`, `project.json`,
//! and one `*.jsonl` per table in [`crate::domain::pack::pack_table_files`].
//! The reader/validator half lives in
//! [`crate::application::interchange::read_bundle`], which re-validates what
//! was written before the transaction commits.
//!
//! Format selection (CTX-0139): v2 is emitted once the tombstone side table
//! exists (schema 0018); before that the writer stays on the v1 layout so
//! pre-tombstone databases keep exporting byte-compatible bundles. The
//! format layer accepts both; v2 is required for merge-grade deletes.
//!
//! Transaction discipline: [`run_export`] appends the `project.exported`
//! audit event FIRST inside a unit-of-work immediate transaction, then
//! dumps from the same connection (so the bundle includes its own export
//! event and the manifest counts describe exactly the rows on disk), writes
//! the directory, re-validates it, and commits once. Any failure before the
//! commit rolls the event back, so a failed export never leaves a phantom
//! audit row. [`plan_export`] opens the database read-only, appends nothing,
//! and creates no directories.
//!
//! Snapshot ref (CTX-0144, design `2026-09-10-mergeable-git-managed-state.md`
//! §3.1–§3.2, §3.6; decisions DEC-0051/DEC-0052): with `--snapshot`, after the
//! bundle is written, re-validated, and the transaction is committed, the
//! bundle directory is committed to the local-only snapshot ref (one commit
//! per snapshot, Git plumbing only, compare-and-swap) and
//! `snapshot_state.last_export_id`/`last_snapshot_commit` are updated. The ref
//! lives under `refs/carryctx/` and is never pushed by the binary; it MUST NOT
//! be pushed to a public repository unredacted.
//!
//! Publication ref (CTX-0155, DEC-0052/issue #138): with `--publication`, every
//! snapshot row is redacted ([`carryctx_pack::redact`]) before the bundle is
//! written, `manifest.redacted` is stamped, and the artifact is committed to
//! the distinct public ref `refs/heads/carryctx-snapshots` with the same
//! plumbing and compare-and-swap as local snapshots. `snapshot_state` is not
//! moved (the public DAG is separate) and the binary still never pushes;
//! explicit user transport publishes the branch, whose rows are redacted by
//! then. Redacted bundles remain publication artifacts that are refused as
//! merge sources.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use rusqlite::{Connection, Row};

use crate::adapter::git::{
    GitCli, GitProject, LOCAL_SNAPSHOT_REF_PREFIX, PUBLIC_SNAPSHOT_REF, SNAPSHOT_REF_DEFAULT,
    VcsBackend,
};
use crate::adapter::sqlite::ProjectDatabase;
use crate::adapter::sqlite_repos::{SqliteEventRepository, SqliteSnapshotStateRepository};
use crate::adapter::xdg::XdgPaths;
use crate::application::interchange::read_bundle;
use crate::domain::pack::{self, PackManifest, PackSource};
use crate::error::CarryCtxError;
use crate::repository::event::{EventRepository, NewEvent};
use carryctx_core::repository::snapshot_state::{
    LAST_EXPORT_ID, LAST_SNAPSHOT_COMMIT, SnapshotStateRepository,
};
use carryctx_pack::redact;

/// Where a successful export writes its snapshot commit (design
/// §3.1/§3.2/§3.6). `None` is a plain bundle export that writes no ref.
#[derive(Debug, Clone, Copy)]
pub enum ExportTarget<'a> {
    /// Unredacted, local-only snapshot ref (CTX-0144). Must live under
    /// `refs/carryctx/`; never pushed by CarryCtx.
    LocalSnapshot { git_ref: &'a str },
    /// Redacted publication on the dedicated public ref
    /// `refs/heads/carryctx-snapshots` (CTX-0155, DEC-0052/issue #138). The
    /// bundle is redacted before validation, `manifest.redacted` is stamped,
    /// and `snapshot_state` is not moved: the publication DAG is separate from
    /// the local snapshot DAG.
    Publication,
}

/// A validated [`ExportTarget`]: the ref to write plus whether the redaction
/// pass applies. Only [`resolve_target`] constructs one.
struct ResolvedTarget<'a> {
    git_ref: &'a str,
    redacted: bool,
}

/// Validate the export target before any database, file, or ref write.
fn resolve_target<'a>(
    project_path: &Path,
    target: Option<ExportTarget<'a>>,
) -> Result<Option<ResolvedTarget<'a>>, CarryCtxError> {
    match target {
        None => Ok(None),
        Some(ExportTarget::LocalSnapshot { git_ref }) => {
            validate_snapshot_ref(project_path, git_ref)?;
            Ok(Some(ResolvedTarget {
                git_ref,
                redacted: false,
            }))
        }
        Some(ExportTarget::Publication) => {
            validate_publication_ref(project_path)?;
            Ok(Some(ResolvedTarget {
                git_ref: PUBLIC_SNAPSHOT_REF,
                redacted: true,
            }))
        }
    }
}

/// Apply the publication redaction pass to every dumped table; returns the
/// replacement count. Row counts and order are preserved.
fn redact_tables(tables: &mut BTreeMap<String, Vec<serde_json::Value>>) -> u64 {
    tables
        .values_mut()
        .map(|rows| redact::redact_rows(rows))
        .sum()
}

/// Redaction is only representable in the v2 manifest (`redacted` flag), so a
/// v1 database cannot publish until it is migrated.
fn require_v2_for_publication(snapshot: &Snapshot) -> Result<(), CarryCtxError> {
    if snapshot.format_version < pack::PACK_FORMAT_VERSION {
        return Err(CarryCtxError::unsupported_operation(
            "Redacted publication requires ctxpack format v2 (schema 0018); this database still emits v1. Migrate the project database before publishing.",
        ));
    }
    Ok(())
}

fn hostname() -> String {
    std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".into())
}

fn db_err(context: &str, error: impl std::fmt::Display) -> CarryCtxError {
    CarryCtxError::database_error(format!("{context}: {error}"))
}

/// v1 ships `--pack-format dir` only. Any other layout name refuses with
/// `UNSUPPORTED_OPERATION` (exit 10): it names a real future format, not a
/// typo, so it must not masquerade as `INVALID_ARGUMENTS`.
fn require_dir_format(pack_format: &str) -> Result<(), CarryCtxError> {
    if pack_format == "dir" {
        Ok(())
    } else {
        Err(CarryCtxError::unsupported_operation(format!(
            "Pack format '{pack_format}' is not supported in v1; only '--pack-format dir' is available. Single-file transport stays external: 'tar -cf - <dir> | ...'."
        )))
    }
}

/// Fail closed before touching the database: an existing non-directory at
/// the target path can never receive a bundle.
fn reject_file_target(out_dir: &Path) -> Result<(), CarryCtxError> {
    if out_dir.exists() && !out_dir.is_dir() {
        return Err(CarryCtxError::invalid_arguments(format!(
            "Export target '{}' exists and is not a directory.",
            out_dir.display()
        )));
    }
    Ok(())
}

/// Render the target path the way it will appear in the envelope and the
/// audit payload: absolute (anchored at the caller's cwd when relative) but
/// never canonicalized, since the directory may not exist yet.
fn display_path(out_dir: &Path) -> Result<String, CarryCtxError> {
    if out_dir.is_absolute() {
        return Ok(out_dir.to_string_lossy().into_owned());
    }
    let cwd = std::env::current_dir().map_err(|e| {
        CarryCtxError::io_error(format!("Failed to resolve working directory: {e}"))
    })?;
    Ok(cwd.join(out_dir).to_string_lossy().into_owned())
}

fn sql_value_to_json(value: rusqlite::types::Value) -> serde_json::Value {
    match value {
        rusqlite::types::Value::Null => serde_json::Value::Null,
        rusqlite::types::Value::Integer(i) => serde_json::json!(i),
        rusqlite::types::Value::Real(f) => serde_json::Number::from_f64(f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        rusqlite::types::Value::Text(s) => serde_json::Value::String(s),
        // No blob columns exist in the v1 schema; hex keeps the dump
        // lossless and re-insertable if one ever appears.
        rusqlite::types::Value::Blob(bytes) => serde_json::Value::String(hex::encode(bytes)),
    }
}

pub(crate) fn row_to_json(names: &[String], row: &Row) -> rusqlite::Result<serde_json::Value> {
    let mut map = serde_json::Map::with_capacity(names.len());
    for (index, name) in names.iter().enumerate() {
        let value: rusqlite::types::Value = row.get(index)?;
        map.insert(name.clone(), sql_value_to_json(value));
    }
    Ok(serde_json::Value::Object(map))
}

/// Dump one table to raw row objects (`column -> value`, keys already
/// `snake_case`). The table name is whitelisted against
/// [`pack::PACK_TABLE_FILES`] plus `projects`: dynamic identifiers are never
/// interpolated from caller input.
fn dump_table(conn: &Connection, table: &str) -> Result<Vec<serde_json::Value>, CarryCtxError> {
    if table != "projects" && !pack::PACK_TABLE_FILES.contains(&table) {
        return Err(CarryCtxError::database_error(format!(
            "Refusing to dump unknown table '{table}'."
        )));
    }
    let mut stmt = conn
        .prepare(&format!("SELECT * FROM \"{table}\" ORDER BY rowid"))
        .map_err(|e| db_err(&format!("Failed to dump table '{table}'"), e))?;
    let names: Vec<String> = stmt
        .column_names()
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    stmt.query_map([], |row| row_to_json(&names, row))
        .map_err(|e| db_err(&format!("Failed to dump table '{table}'"), e))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| db_err(&format!("Failed to read row of table '{table}'"), e))
}

/// True when `table` exists in the attached schema.
pub(crate) fn table_exists(conn: &Connection, table: &str) -> Result<bool, CarryCtxError> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get(0),
        )
        .map_err(|e| db_err("Failed to inspect schema", e))?;
    Ok(count > 0)
}

/// Interchange format version this database can emit: v2 once the tombstone
/// side table exists (schema 0018), v1 before that. The format layer reads
/// both; emitting v1 keeps pre-tombstone exports byte-compatible.
pub(crate) fn detect_format_version(conn: &Connection) -> Result<u32, CarryCtxError> {
    if table_exists(conn, "tombstones")? {
        Ok(pack::PACK_FORMAT_VERSION)
    } else {
        Ok(pack::PACK_FORMAT_VERSION_V1)
    }
}

/// Fast row counts without materializing rows, for the pre-append payload.
fn count_table(conn: &Connection, table: &str) -> Result<u64, CarryCtxError> {
    debug_assert!(pack::PACK_TABLE_FILES.contains(&table));
    let count: i64 = conn
        .query_row(&format!("SELECT COUNT(*) FROM \"{table}\""), [], |row| {
            row.get(0)
        })
        .map_err(|e| db_err(&format!("Failed to count table '{table}'"), e))?;
    Ok(count.max(0) as u64)
}

/// Row count per dumped table. The map covers exactly the tables present
/// for the snapshot's format version (v1: 17, v2: 18 including an explicit
/// zero for `tombstones`).
pub(crate) fn counts_of(
    tables: &BTreeMap<String, Vec<serde_json::Value>>,
) -> BTreeMap<String, u64> {
    tables
        .iter()
        .map(|(table, rows)| (table.clone(), rows.len() as u64))
        .collect()
}

#[derive(Debug)]
pub(crate) struct Snapshot {
    pub project_id: String,
    pub project: serde_json::Value,
    pub tables: BTreeMap<String, Vec<serde_json::Value>>,
    pub sequences: BTreeMap<String, u64>,
    pub schema_version: u32,
    pub format_version: u32,
}

/// Dump the whole project: exactly one `projects` row plus every table of
/// the database's writable pack format (empty tables dump as zero rows,
/// never as missing files).
pub(crate) fn collect_snapshot(conn: &Connection) -> Result<Snapshot, CarryCtxError> {
    let project_rows = dump_table(conn, "projects")?;
    if project_rows.is_empty() {
        return Err(CarryCtxError::resource_not_found(
            "No CarryCtx project is initialized here; run `carryctx init` first.",
        ));
    }
    if project_rows.len() != 1 {
        return Err(CarryCtxError::database_error(
            "Project table must contain exactly one row.",
        ));
    }
    let project = project_rows.into_iter().next().expect("checked");
    let project_id = project
        .get("id")
        .and_then(|v| v.as_str())
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| CarryCtxError::database_error("Project row has no id."))?
        .to_string();

    let format_version = detect_format_version(conn)?;
    let mut tables = BTreeMap::new();
    for table in pack::pack_table_files(format_version) {
        tables.insert((*table).to_string(), dump_table(conn, table)?);
    }

    let mut sequences = BTreeMap::new();
    {
        let mut stmt = conn
            .prepare("SELECT kind, next_value FROM sequences WHERE project_id = ?1")
            .map_err(|e| db_err("Failed to read sequences", e))?;
        let rows = stmt
            .query_map([&project_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|e| db_err("Failed to read sequences", e))?;
        for row in rows {
            let (kind, next) = row.map_err(|e| db_err("Failed to read sequence row", e))?;
            sequences.insert(kind, next.max(0) as u64);
        }
    }

    let applied: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )
        .map_err(|e| db_err("Failed to read schema version", e))?;
    let schema_version = if applied > 0 {
        applied as u32
    } else {
        project
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as u32
    };

    Ok(Snapshot {
        project_id,
        project,
        tables,
        sequences,
        schema_version,
        format_version,
    })
}

pub(crate) fn build_manifest(
    snapshot: &Snapshot,
    counts: BTreeMap<String, u64>,
    export_id: String,
    created_at: String,
    git_branch: Option<String>,
    git_commit: Option<String>,
    parents: Vec<String>,
) -> PackManifest {
    let source = PackSource {
        git_branch,
        git_commit,
        hostname: Some(hostname()),
    };
    let mut manifest = if snapshot.format_version >= pack::PACK_FORMAT_VERSION {
        PackManifest::new(
            env!("CARGO_PKG_VERSION"),
            snapshot.schema_version,
            snapshot.project_id.clone(),
            export_id,
            created_at,
            source,
            counts,
        )
    } else {
        PackManifest::new_v1(
            env!("CARGO_PKG_VERSION"),
            snapshot.schema_version,
            snapshot.project_id.clone(),
            export_id,
            created_at,
            source,
            counts,
        )
    };
    manifest.sequences = snapshot.sequences.clone();
    manifest.parents = parents;
    manifest
}

/// The current snapshot-ref tip `(commit sha, export id)`, or `None` when the
/// ref does not exist yet. The ref is read with Git plumbing only and never
/// mutated here.
pub(crate) fn read_snapshot_tip(
    git: &GitCli,
    repo_root: &Path,
    git_ref: &str,
) -> Result<Option<(String, String)>, CarryCtxError> {
    let (bytes, commit) = git.read_snapshot_manifest(repo_root, git_ref)?;
    let Some(commit) = commit else {
        return Ok(None);
    };
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| {
        CarryCtxError::git_error(format!(
            "Snapshot ref '{git_ref}' has an unreadable manifest.json: {e}"
        ))
    })?;
    let export_id = value
        .get("export_id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| {
            CarryCtxError::git_error(format!(
                "Snapshot ref '{git_ref}' manifest.json has no export_id."
            ))
        })?;
    Ok(Some((commit, export_id.to_string())))
}

/// Human-readable `CarryCtx-Source` trailer label:
/// `<repo>@<short-sha> (<branch>)`.
pub(crate) fn snapshot_source_label(gp: &GitProject) -> String {
    let repo = gp
        .repository_root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "repo".to_string());
    let short = short_head(gp);
    match &gp.branch {
        Some(branch) => format!("{repo}@{short} ({branch})"),
        None => format!("{repo}@{short}"),
    }
}

/// Snapshot subject descriptor: `<branch> @ <short-sha>` (design §3.1).
pub(crate) fn snapshot_subject_label(gp: &GitProject) -> String {
    let branch = gp.branch.clone().unwrap_or_else(|| "detached".to_string());
    format!("{branch} @ {}", short_head(gp))
}

fn short_head(gp: &GitProject) -> String {
    gp.head
        .as_deref()
        .map(|head| head.chars().take(7).collect::<String>())
        .filter(|head| !head.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Validate `--snapshot-ref` before any ref or database write.
///
/// The unredacted snapshot ref must be local-only (DEC-0052, issue #138):
///
/// - the value must be a full `refs/...` name that `git check-ref-format`
///   accepts (bare names, `HEAD`, short revs, and leading `-` are refused);
/// - `refs/heads/carryctx-snapshots` is reserved for redacted publication and
///   is refused with a dedicated message;
/// - every other name must live under `refs/carryctx/`; a `refs/heads/*`
///   branch would be moved by a plain `git push` and is refused.
///
/// Returns `INVALID_ARGUMENTS` (exit 2) on any violation.
pub fn validate_snapshot_ref(project_path: &Path, git_ref: &str) -> Result<(), CarryCtxError> {
    if !git_ref.starts_with("refs/") {
        return Err(CarryCtxError::invalid_arguments(format!(
            "Snapshot ref '{git_ref}' must be a full ref name starting with 'refs/' (e.g. '{SNAPSHOT_REF_DEFAULT}')."
        )));
    }
    let git = GitCli::new();
    let gp = git.discover(project_path)?;
    if !git.check_ref_format(&gp.repository_root, git_ref)? {
        return Err(CarryCtxError::invalid_arguments(format!(
            "Snapshot ref '{git_ref}' is not a valid Git ref name."
        )));
    }
    if git_ref == PUBLIC_SNAPSHOT_REF {
        return Err(CarryCtxError::invalid_arguments(format!(
            "Snapshot ref '{git_ref}' is reserved for redacted publication (DEC-0052); unredacted local snapshots must use '{SNAPSHOT_REF_DEFAULT}'."
        )));
    }
    if !git_ref.starts_with(LOCAL_SNAPSHOT_REF_PREFIX) {
        return Err(CarryCtxError::invalid_arguments(format!(
            "Snapshot ref '{git_ref}' must live under '{LOCAL_SNAPSHOT_REF_PREFIX}' (local-only, never pushed by CarryCtx); 'refs/heads/*' branches would be moved by a plain 'git push' and are refused."
        )));
    }
    Ok(())
}

/// Validate the fixed public redacted publication ref before any write.
///
/// Publication always targets [`PUBLIC_SNAPSHOT_REF`] (DEC-0052, issue #138):
/// the ref is reserved for redacted artifacts and is deliberately a normal
/// `refs/heads/*` branch so explicit user transport can publish it. Callers
/// cannot redirect it; this check is defense in depth around the constant.
pub fn validate_publication_ref(project_path: &Path) -> Result<(), CarryCtxError> {
    let git = GitCli::new();
    let gp = git.discover(project_path)?;
    if !git.check_ref_format(&gp.repository_root, PUBLIC_SNAPSHOT_REF)? {
        return Err(CarryCtxError::git_error(format!(
            "Publication ref '{PUBLIC_SNAPSHOT_REF}' is not a valid Git ref name."
        )));
    }
    Ok(())
}

/// Read the just-written bundle files into `(name, bytes)` pairs for the
/// commit tree; `manifest.json`, `project.json`, and one `*.jsonl` per table.
fn read_bundle_files(
    out_dir: &Path,
    format_version: u32,
) -> Result<Vec<(String, Vec<u8>)>, CarryCtxError> {
    let mut names: Vec<String> = vec![
        pack::PACK_MANIFEST_FILE.to_string(),
        pack::PACK_PROJECT_FILE.to_string(),
    ];
    for table in pack::pack_table_files(format_version) {
        names.push(format!("{table}.jsonl"));
    }
    let mut files = Vec::with_capacity(names.len());
    for name in names {
        let bytes = fs::read(out_dir.join(&name)).map_err(|e| {
            CarryCtxError::io_error(format!(
                "Failed to read exported file '{name}' for the snapshot commit: {e}"
            ))
        })?;
        files.push((name, bytes));
    }
    Ok(files)
}

/// Serialize a bundle into the exact `(file name, bytes)` set the ctxpack
/// directory layout uses: `manifest.json`, `project.json`, and one `*.jsonl`
/// per table. This is the single serialization source of truth shared by
/// [`write_bundle`] (which writes the bytes to disk) and the CTX-0145 merge
/// snapshot writer (which commits them without a directory).
pub(crate) fn serialize_bundle_files(
    manifest: &PackManifest,
    project: &serde_json::Value,
    tables: &BTreeMap<String, Vec<serde_json::Value>>,
) -> Result<Vec<(String, Vec<u8>)>, CarryCtxError> {
    let mut files = Vec::new();
    files.push((
        pack::PACK_MANIFEST_FILE.to_string(),
        serde_json::to_string_pretty(manifest)
            .map_err(|e| CarryCtxError::io_error(format!("Failed to encode manifest: {e}")))?
            .into_bytes(),
    ));
    files.push((
        pack::PACK_PROJECT_FILE.to_string(),
        serde_json::to_string_pretty(project)
            .map_err(|e| CarryCtxError::io_error(format!("Failed to encode project row: {e}")))?
            .into_bytes(),
    ));
    for table in pack::pack_table_files(manifest.format_version) {
        let mut text = String::new();
        if let Some(rows) = tables.get(*table) {
            for row in rows {
                text.push_str(&serde_json::to_string(row).map_err(|e| {
                    CarryCtxError::io_error(format!("Failed to encode '{table}.jsonl' row: {e}"))
                })?);
                text.push('\n');
            }
        }
        files.push((format!("{table}.jsonl"), text.into_bytes()));
    }
    Ok(files)
}

fn write_bundle(
    out_dir: &Path,
    manifest: &PackManifest,
    project: &serde_json::Value,
    tables: &BTreeMap<String, Vec<serde_json::Value>>,
) -> Result<(), CarryCtxError> {
    fs::create_dir_all(out_dir).map_err(|e| {
        CarryCtxError::io_error(format!(
            "Failed to create export directory '{}': {e}",
            out_dir.display()
        ))
    })?;
    for (name, bytes) in serialize_bundle_files(manifest, project, tables)? {
        fs::write(out_dir.join(&name), bytes).map_err(|e| {
            CarryCtxError::io_error(format!("Failed to write pack file '{name}': {e}"))
        })?;
    }
    Ok(())
}

/// Validate + print the export plan without writing anything: no SQLite
/// writes (read-only connection, no migration), no directories, no events, no
/// snapshot ref, no `snapshot_state`. With a target, the would-be parents,
/// current ref tip, and (for publication) the planned redaction count are
/// reported.
pub fn plan_export(
    project_path: &Path,
    pack_format: &str,
    out_dir: &Path,
    target: Option<ExportTarget<'_>>,
) -> Result<serde_json::Value, CarryCtxError> {
    require_dir_format(pack_format)?;
    reject_file_target(out_dir)?;
    let resolved = resolve_target(project_path, target)?;
    let git = GitCli::new();
    let gp = git.discover(project_path)?;
    let xdg = XdgPaths::new();
    let db_path = xdg.project_db(&gp.git_common_dir);
    if !db_path.exists() {
        return Err(CarryCtxError::resource_not_found(
            "No CarryCtx project database found; run `carryctx init` first.",
        ));
    }
    let database = ProjectDatabase::open_readonly(&db_path)?;
    let mut snapshot = collect_snapshot(database.connection())?;
    let redacted = resolved.as_ref().is_some_and(|target| target.redacted);
    if redacted {
        require_v2_for_publication(&snapshot)?;
    }
    // The redaction count is computed on the read-only dump so `--dry-run`
    // reports what would be replaced without writing anything.
    let redactions = if redacted {
        redact_tables(&mut snapshot.tables)
    } else {
        0
    };
    let counts = counts_of(&snapshot.tables);

    // Snapshot planning reads the ref tip (read-only) but writes nothing.
    let tip = match &resolved {
        Some(target) => read_snapshot_tip(&git, &gp.repository_root, target.git_ref)?,
        None => None,
    };
    let parents: Vec<String> = tip.iter().map(|(_, export_id)| export_id.clone()).collect();
    let target_plan = resolved.as_ref().map(|target| {
        let mut plan = serde_json::json!({
            "ref": target.git_ref,
            "tipCommit": tip.as_ref().map(|(commit, _)| commit.clone()),
            "tipExportId": tip.as_ref().map(|(_, export_id)| export_id.clone()),
            "parents": parents,
            "wouldCommit": true,
        });
        if target.redacted {
            plan["redacted"] = serde_json::json!(true);
            plan["redactions"] = serde_json::json!(redactions);
        }
        plan
    });

    // Plan-only id: no `project.exported` event is appended on this path,
    // so this export_id is never recorded anywhere.
    let mut manifest = build_manifest(
        &snapshot,
        counts.clone(),
        ulid::Ulid::generate().to_string(),
        chrono::Utc::now().to_rfc3339(),
        gp.branch.clone(),
        gp.head.clone(),
        parents,
    );
    manifest.redacted = redacted;
    pack::check_counts(&manifest, &counts)?;
    let mut data = serde_json::json!({
        "manifest": manifest,
        "counts": counts,
        "path": display_path(out_dir)?,
        "operation": {"applied": false},
    });
    if let Some(plan) = target_plan {
        if redacted {
            data["publication"] = plan;
        } else {
            data["snapshot"] = plan;
        }
    }
    Ok(data)
}

/// Export the whole project to `<out_dir>/` and append one
/// `project.exported` audit event in the same transaction.
///
/// Append-first ordering: the event is appended before the dump on the same
/// unit-of-work connection, so the bundle includes its own export event
/// and the manifest `counts` (mirrored into the event payload) describe
/// exactly the rows written. Success envelope data is
/// `{manifest, counts, path}` per the design Section 3 contract.
///
/// With a target, the manifest's `parents` records the target ref tip's
/// export id and, after the bundle is written, validated, and committed, one
/// commit is created on the ref (design §3.2). A local snapshot target also
/// moves `snapshot_state.last_export_id`/`last_snapshot_commit`; a publication
/// target leaves `snapshot_state` alone because the public DAG is separate.
/// The ref write uses a compare-and-swap and fails closed; a plain export with
/// no target keeps `parents = []` and writes no ref.
pub fn run_export(
    project_path: &Path,
    pack_format: &str,
    out_dir: &Path,
    actor_agent_id: Option<String>,
    session_id: Option<String>,
    target: Option<ExportTarget<'_>>,
) -> Result<serde_json::Value, CarryCtxError> {
    require_dir_format(pack_format)?;
    reject_file_target(out_dir)?;
    let resolved = resolve_target(project_path, target)?;
    let git = GitCli::new();
    let gp = git.discover(project_path)?;
    let xdg = XdgPaths::new();
    let db_path = xdg.project_db(&gp.git_common_dir);
    if !db_path.exists() {
        return Err(CarryCtxError::resource_not_found(
            "No CarryCtx project database found; run `carryctx init` first.",
        ));
    }
    // Read the snapshot tip before the bundle is built so `manifest.parents`
    // is self-describing. The ref write below re-reads it for the CAS guard.
    let tip = match &resolved {
        Some(target) => read_snapshot_tip(&git, &gp.repository_root, target.git_ref)?,
        None => None,
    };
    let parent_export_ids: Vec<String> =
        tip.iter().map(|(_, export_id)| export_id.clone()).collect();
    let parent_commits: Vec<String> = tip.iter().map(|(commit, _)| commit.clone()).collect();

    // `open` would create a missing file; the exists() gate above keeps a
    // failed export from conjuring an empty database.
    let mut database = ProjectDatabase::open(&db_path)?;
    database.migrate()?;
    let path = display_path(out_dir)?;

    let (manifest, counts, redactions) = {
        let uow = database.begin_unit_of_work()?;
        let conn = uow.connection();
        let parents = parent_export_ids.clone();
        let result = (|| {
            let project_id: String = conn
                .query_row("SELECT id FROM projects LIMIT 1", [], |row| row.get(0))
                .map_err(|e| {
                    if e == rusqlite::Error::QueryReturnedNoRows {
                        CarryCtxError::resource_not_found(
                            "No CarryCtx project is initialized here; run `carryctx init` first.",
                        )
                    } else {
                        db_err("Failed to resolve project id", e)
                    }
                })?;
            let export_id = ulid::Ulid::generate().to_string();
            let created_at = chrono::Utc::now().to_rfc3339();
            let format_version = detect_format_version(conn)?;

            // Provisional bundle counts for the payload: current rows, with
            // `events` one higher for the event appended just below.
            let mut payload_counts = BTreeMap::new();
            for table in pack::pack_table_files(format_version) {
                payload_counts.insert((*table).to_string(), count_table(conn, table)?);
            }
            *payload_counts
                .get_mut("events")
                .expect("events is a pack table") += 1;

            SqliteEventRepository::new(conn).append(&NewEvent {
                id: ulid::Ulid::generate().to_string(),
                project_id,
                event_type: "project.exported".into(),
                actor_agent_id,
                session_id,
                task_id: None,
                payload: serde_json::json!({
                    "exportId": export_id,
                    "format": pack::PACK_FORMAT,
                    "formatVersion": format_version,
                    "counts": payload_counts,
                    "path": path,
                }),
                occurred_at: created_at.clone(),
            })?;

            let mut snapshot = collect_snapshot(conn)?;
            let publish = resolved.as_ref().is_some_and(|target| target.redacted);
            if publish {
                require_v2_for_publication(&snapshot)?;
            }
            // Publication redacts every dumped row before the bundle is built
            // and validated; row counts/order are preserved (CTX-0155).
            let redactions = if publish {
                redact_tables(&mut snapshot.tables)
            } else {
                0
            };
            let counts = counts_of(&snapshot.tables);
            // The dump must observe exactly the payload's counts (same
            // transaction, admission-locked): otherwise something wrote
            // concurrently and the bundle would misdescribe itself.
            if counts != payload_counts {
                return Err(CarryCtxError::validation_error(
                    "Export count skew between audit payload and dump; retry the export.",
                ));
            }
            let mut manifest = build_manifest(
                &snapshot,
                counts.clone(),
                export_id,
                created_at,
                gp.branch.clone(),
                gp.head.clone(),
                parents,
            );
            manifest.redacted = publish;
            pack::check_counts(&manifest, &counts)?;
            write_bundle(out_dir, &manifest, &snapshot.project, &snapshot.tables)?;
            // Re-read through the T1 validator: the bytes on disk must parse
            // and match the manifest (v1 manifests are compared in their
            // migrated v2 view).
            let bundle = read_bundle(out_dir)?;
            pack::check_counts(&bundle.manifest, &bundle.actual_counts())?;
            if bundle.manifest != manifest.clone().into_current() {
                return Err(CarryCtxError::validation_error(
                    "Exported manifest does not round-trip; retry the export.",
                ));
            }
            Ok::<(PackManifest, BTreeMap<String, u64>, u64), CarryCtxError>((
                manifest, counts, redactions,
            ))
        })();
        // Commit only on full success: Drop rolls back on any error above, so
        // a failed export never leaves a phantom `project.exported` row.
        let result = result?;
        uow.commit()?;
        result
    };

    // The ref commit happens only after the bundle is on disk, validated, and
    // the export transaction has committed (design §3.2). A CAS failure leaves
    // the bundle and audit row in place and reports GIT_ERROR; the ref is
    // never force-moved.
    let target_data = match &resolved {
        None => None,
        Some(target) => {
            let files = read_bundle_files(out_dir, manifest.format_version)?;
            let source_label = snapshot_source_label(&gp);
            let subject_label = snapshot_subject_label(&gp);
            let commit = git.create_snapshot_commit(
                &gp.repository_root,
                target.git_ref,
                &files,
                &manifest.export_id,
                &parent_commits,
                &source_label,
                &subject_label,
            )?;
            if !target.redacted {
                let now = chrono::Utc::now().to_rfc3339();
                // Both keys move together: one transaction so a failure between
                // the two upserts can never leave `last_export_id` and
                // `last_snapshot_commit` describing different snapshots. The
                // publication DAG is separate, so `--publication` never moves
                // these local-only pointers.
                let tx = database.connection().unchecked_transaction().map_err(|e| {
                    CarryCtxError::database_error(format!(
                        "Failed to start the snapshot_state transaction: {e}"
                    ))
                })?;
                {
                    let state = SqliteSnapshotStateRepository::new(&tx);
                    state.set(
                        &manifest.project_id,
                        LAST_EXPORT_ID,
                        &manifest.export_id,
                        &now,
                    )?;
                    state.set(
                        &manifest.project_id,
                        LAST_SNAPSHOT_COMMIT,
                        &commit.commit,
                        &now,
                    )?;
                }
                tx.commit().map_err(|e| {
                    CarryCtxError::database_error(format!(
                        "Failed to commit the snapshot_state transaction: {e}"
                    ))
                })?;
            }
            let mut write = serde_json::json!({
                "ref": target.git_ref,
                "commit": commit.commit,
                "previousCommit": commit.previous,
                "parentExportIds": commit.parent_export_ids,
                "parents": parent_export_ids,
                "source": source_label,
            });
            if target.redacted {
                write["redacted"] = serde_json::json!(true);
                write["redactions"] = serde_json::json!(redactions);
            }
            Some(write)
        }
    };

    let mut data = serde_json::json!({
        "manifest": manifest,
        "counts": counts,
        "path": path,
    });
    if let Some(target_data) = target_data {
        if resolved.as_ref().is_some_and(|target| target.redacted) {
            data["publication"] = target_data;
        } else {
            data["snapshot"] = target_data;
        }
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_format_accepted_others_refused_as_unsupported() {
        assert!(require_dir_format("dir").is_ok());
        for bad in ["tar", "DIR", "", "zip"] {
            let error = require_dir_format(bad).unwrap_err();
            assert_eq!(error.code, "UNSUPPORTED_OPERATION", "format '{bad}'");
            assert_eq!(format!("{}", error.exit_code as i32), "10");
        }
    }

    #[test]
    fn file_target_refused_missing_path_allowed() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("file.sqlite");
        fs::write(&file, b"x").unwrap();
        let error = reject_file_target(&file).unwrap_err();
        assert_eq!(error.code, "INVALID_ARGUMENTS");
        assert!(reject_file_target(&root.path().join("absent")).is_ok());
        assert!(reject_file_target(root.path()).is_ok());
    }

    #[test]
    fn sql_values_convert_losslessly() {
        use rusqlite::types::Value as Sql;
        assert_eq!(sql_value_to_json(Sql::Null), serde_json::Value::Null);
        assert_eq!(sql_value_to_json(Sql::Integer(-7)), serde_json::json!(-7));
        assert_eq!(sql_value_to_json(Sql::Real(1.5)), serde_json::json!(1.5));
        // Non-finite reals have no JSON form; encode as null, never panic.
        assert_eq!(
            sql_value_to_json(Sql::Real(f64::INFINITY)),
            serde_json::Value::Null
        );
        assert_eq!(
            sql_value_to_json(Sql::Text("hi".into())),
            serde_json::json!("hi")
        );
        assert_eq!(
            sql_value_to_json(Sql::Blob(vec![0xab, 0xcd])),
            serde_json::json!("abcd")
        );
    }

    #[test]
    fn dump_refuses_tables_outside_the_pack_whitelist() {
        let conn = Connection::open_in_memory().unwrap();
        let error = dump_table(&conn, "sqlite_master").unwrap_err();
        assert_eq!(error.code, "DATABASE_ERROR");
        let error = dump_table(&conn, "operations").unwrap_err();
        assert_eq!(error.code, "DATABASE_ERROR");
    }

    #[test]
    fn snapshot_without_project_row_is_not_found() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE projects (id TEXT PRIMARY KEY, name TEXT);")
            .unwrap();
        let error = collect_snapshot(&conn).unwrap_err();
        assert_eq!(error.code, "RESOURCE_NOT_FOUND");
    }
}
