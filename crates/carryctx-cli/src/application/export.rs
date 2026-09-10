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

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use rusqlite::{Connection, Row};

use crate::adapter::git::GitCli;
use crate::adapter::sqlite::ProjectDatabase;
use crate::adapter::sqlite_repos::SqliteEventRepository;
use crate::adapter::xdg::XdgPaths;
use crate::application::interchange::read_bundle;
use crate::domain::pack::{self, PackManifest, PackSource};
use crate::error::CarryCtxError;
use crate::repository::event::{EventRepository, NewEvent};

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
fn detect_format_version(conn: &Connection) -> Result<u32, CarryCtxError> {
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
fn counts_of(tables: &BTreeMap<String, Vec<serde_json::Value>>) -> BTreeMap<String, u64> {
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

fn build_manifest(
    snapshot: &Snapshot,
    counts: BTreeMap<String, u64>,
    export_id: String,
    created_at: String,
    git_branch: Option<String>,
    git_commit: Option<String>,
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
    manifest
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
    let write = |name: &str, content: String| {
        fs::write(out_dir.join(name), content).map_err(|e| {
            CarryCtxError::io_error(format!("Failed to write pack file '{name}': {e}"))
        })
    };
    write(
        pack::PACK_MANIFEST_FILE,
        serde_json::to_string_pretty(manifest)
            .map_err(|e| CarryCtxError::io_error(format!("Failed to encode manifest: {e}")))?,
    )?;
    write(
        pack::PACK_PROJECT_FILE,
        serde_json::to_string_pretty(project)
            .map_err(|e| CarryCtxError::io_error(format!("Failed to encode project row: {e}")))?,
    )?;
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
        write(&format!("{table}.jsonl"), text)?;
    }
    Ok(())
}

/// Validate + print the export plan without writing anything: no SQLite
/// writes (read-only connection, no migration), no directories, no events.
pub fn plan_export(
    project_path: &Path,
    pack_format: &str,
    out_dir: &Path,
) -> Result<serde_json::Value, CarryCtxError> {
    require_dir_format(pack_format)?;
    reject_file_target(out_dir)?;
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
    let snapshot = collect_snapshot(database.connection())?;
    let counts = counts_of(&snapshot.tables);
    // Plan-only id: no `project.exported` event is appended on this path,
    // so this export_id is never recorded anywhere.
    let manifest = build_manifest(
        &snapshot,
        counts.clone(),
        ulid::Ulid::generate().to_string(),
        chrono::Utc::now().to_rfc3339(),
        gp.branch.clone(),
        gp.head.clone(),
    );
    pack::check_counts(&manifest, &counts)?;
    Ok(serde_json::json!({
        "manifest": manifest,
        "counts": counts,
        "path": display_path(out_dir)?,
        "operation": {"applied": false},
    }))
}

/// Export the whole project to `<out_dir>/` and append one
/// `project.exported` audit event in the same transaction.
///
/// Append-first ordering: the event is appended before the dump on the same
/// unit-of-work connection, so the bundle includes its own export event
/// and the manifest `counts` (mirrored into the event payload) describe
/// exactly the rows written. Success envelope data is
/// `{manifest, counts, path}` per the design Section 3 contract.
pub fn run_export(
    project_path: &Path,
    pack_format: &str,
    out_dir: &Path,
    actor_agent_id: Option<String>,
    session_id: Option<String>,
) -> Result<serde_json::Value, CarryCtxError> {
    require_dir_format(pack_format)?;
    reject_file_target(out_dir)?;
    let git = GitCli::new();
    let gp = git.discover(project_path)?;
    let xdg = XdgPaths::new();
    let db_path = xdg.project_db(&gp.git_common_dir);
    if !db_path.exists() {
        return Err(CarryCtxError::resource_not_found(
            "No CarryCtx project database found; run `carryctx init` first.",
        ));
    }
    // `open` would create a missing file; the exists() gate above keeps a
    // failed export from conjuring an empty database.
    let mut database = ProjectDatabase::open(&db_path)?;
    database.migrate()?;
    let path = display_path(out_dir)?;

    let uow = database.begin_unit_of_work()?;
    let conn = uow.connection();
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

        let snapshot = collect_snapshot(conn)?;
        let counts = counts_of(&snapshot.tables);
        // The dump must observe exactly the payload's counts (same
        // transaction, admission-locked): otherwise something wrote
        // concurrently and the bundle would misdescribe itself.
        if counts != payload_counts {
            return Err(CarryCtxError::validation_error(
                "Export count skew between audit payload and dump; retry the export.",
            ));
        }
        let manifest = build_manifest(
            &snapshot,
            counts.clone(),
            export_id,
            created_at,
            gp.branch.clone(),
            gp.head.clone(),
        );
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
        Ok::<(PackManifest, BTreeMap<String, u64>), CarryCtxError>((manifest, counts))
    })();
    // Commit only on full success: Drop rolls back on any error above, so
    // a failed export never leaves a phantom `project.exported` row.
    let (manifest, counts) = result?;
    uow.commit()?;
    Ok(serde_json::json!({
        "manifest": manifest,
        "counts": counts,
        "path": path,
    }))
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
