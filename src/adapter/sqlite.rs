use std::path::Path;

use rusqlite::{Connection, OpenFlags, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use crate::error::{CarryCtxError, ExitCode};

/// A migration entry stored in the schema_migrations table.
#[derive(Debug, Clone)]
pub struct Migration {
    pub version: i64,
    pub name: String,
    pub checksum: String,
    pub applied_at: String,
}

/// A migration source bundled with the binary via include_str!.
#[derive(Debug, Clone)]
pub struct MigrationSource {
    pub version: i64,
    pub name: String,
    pub sql: &'static str,
}

/// Checksum of a SQL string (hex-encoded SHA-256).
pub fn checksum_sql(sql: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(sql.as_bytes());
    hex::encode(hasher.finalize())
}

fn migration_sources() -> Vec<MigrationSource> {
    vec![
        MigrationSource {
            version: 1,
            name: "0001_foundation".into(),
            sql: include_str!("../../migrations/project/0001_foundation.sql"),
        },
        MigrationSource {
            version: 2,
            name: "0002_work_model".into(),
            sql: include_str!("../../migrations/project/0002_work_model.sql"),
        },
        MigrationSource {
            version: 3,
            name: "0003_progress".into(),
            sql: include_str!("../../migrations/project/0003_progress.sql"),
        },
        MigrationSource {
            version: 4,
            name: "0004_worktrees_sessions".into(),
            sql: include_str!("../../migrations/project/0004_worktrees_sessions.sql"),
        },
        MigrationSource {
            version: 5,
            name: "0005_checkpoints".into(),
            sql: include_str!("../../migrations/project/0005_checkpoints.sql"),
        },
        MigrationSource {
            version: 6,
            name: "0006_collaboration".into(),
            sql: include_str!("../../migrations/project/0006_collaboration.sql"),
        },
        MigrationSource {
            version: 7,
            name: "0007_context_graph".into(),
            sql: include_str!("../../migrations/project/0007_context_graph.sql"),
        },
        MigrationSource {
            version: 8,
            name: "0008_jj_compat".into(),
            sql: include_str!("../../migrations/project/0008_jj_compat.sql"),
        },
        MigrationSource {
            version: 9,
            name: "0009_search".into(),
            sql: include_str!("../../migrations/project/0009_search.sql"),
        },
        MigrationSource {
            version: 10,
            name: "0010_decision_rationale".into(),
            sql: include_str!("../../migrations/project/0010_decision_rationale.sql"),
        },
        MigrationSource {
            version: 11,
            name: "0011_backfill_session_ended_at".into(),
            sql: include_str!("../../migrations/project/0011_backfill_session_ended_at.sql"),
        },
        MigrationSource {
            version: 12,
            name: "0012_agent_teams".into(),
            sql: include_str!("../../migrations/project/0012_agent_teams.sql"),
        },
        MigrationSource {
            version: 13,
            name: "0013_agent_kind_constraint".into(),
            sql: include_str!("../../migrations/project/0013_agent_kind_constraint.sql"),
        },
    ]
}

/// Wraps a SQLite connection to a single project database.
pub struct ProjectDatabase {
    conn: Connection,
}

impl ProjectDatabase {
    /// Open or create a database at the given path, applying PRAGMAs.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, CarryCtxError> {
        let conn = Connection::open_with_flags(
            path.as_ref(),
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )
        .map_err(|e| {
            CarryCtxError::new(
                "DATABASE_OPEN",
                format!("Failed to open database: {e}"),
                ExitCode::Database,
            )
            .with_source(e)
        })?;

        let mut db = Self { conn };
        db.apply_pragmas()?;
        Ok(db)
    }

    /// Open an existing database in read-only mode.
    pub fn open_readonly(path: impl AsRef<Path>) -> Result<Self, CarryCtxError> {
        let conn = Connection::open_with_flags(path.as_ref(), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| {
                CarryCtxError::new(
                    "DATABASE_OPEN",
                    format!("Failed to open database: {e}"),
                    ExitCode::Database,
                )
                .with_source(e)
            })?;
        let mut db = Self { conn };
        db.apply_pragmas_readonly()?;
        Ok(db)
    }

    /// Apply standard PRAGMAs to the connection.
    fn apply_pragmas(&mut self) -> Result<(), CarryCtxError> {
        self.conn
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA foreign_keys=ON;
                 PRAGMA busy_timeout=10000;
                 PRAGMA synchronous=NORMAL;
                 PRAGMA journal_size_limit=67108864;",
            )
            .map_err(|e| {
                CarryCtxError::new(
                    "DATABASE_PRAGMA",
                    format!("Failed to set PRAGMAs: {e}"),
                    ExitCode::Database,
                )
                .with_source(e)
            })?;
        Ok(())
    }

    /// Apply PRAGMAs suitable for read-only connections.
    fn apply_pragmas_readonly(&mut self) -> Result<(), CarryCtxError> {
        self.conn
            .execute_batch(
                "PRAGMA foreign_keys=ON;
                 PRAGMA busy_timeout=10000;",
            )
            .map_err(|e| {
                CarryCtxError::new(
                    "DATABASE_PRAGMA",
                    format!("Failed to set PRAGMAs: {e}"),
                    ExitCode::Database,
                )
                .with_source(e)
            })?;
        Ok(())
    }

    /// Return a reference to the inner Connection.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Return a mutable reference to the inner Connection.
    pub fn connection_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    // ── Migration inspection ────────────────────────────────────────────

    /// List all applied migrations, ordered by version.
    pub fn list_applied_migrations(&self) -> Result<Vec<Migration>, CarryCtxError> {
        self.validate_schema_compatibility()?;
        self.list_applied_migrations_raw()
    }

    fn list_applied_migrations_raw(&self) -> Result<Vec<Migration>, CarryCtxError> {
        let has_table: bool = self
            .conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_migrations')",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false);

        if !has_table {
            return Ok(Vec::new());
        }

        let mut stmt = self
            .conn
            .prepare("SELECT version, name, checksum, applied_at FROM schema_migrations ORDER BY version")
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(Migration {
                    version: row.get(0)?,
                    name: row.get(1)?,
                    checksum: row.get(2)?,
                    applied_at: row.get(3)?,
                })
            })
            .map_err(db_err)?;
        let mut migrations = Vec::new();
        for row in rows {
            migrations.push(row.map_err(db_err)?);
        }
        Ok(migrations)
    }

    /// Return the highest applied migration version, or 0 if none.
    pub fn applied_version(&self) -> Result<i64, CarryCtxError> {
        self.validate_schema_compatibility()?;
        self.applied_version_raw()
    }

    fn applied_version_raw(&self) -> Result<i64, CarryCtxError> {
        let has_table: bool = self
            .conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_migrations')",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false);

        if !has_table {
            return Ok(0);
        }

        let version: Result<i64, _> = self.conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        );
        version.map_err(db_err)
    }

    /// Return the list of pending migration sources (not yet applied).
    pub fn pending_migrations(&self) -> Result<Vec<MigrationSource>, CarryCtxError> {
        self.validate_schema_compatibility()?;
        let applied = self.applied_version_raw()?;
        Ok(migration_sources()
            .into_iter()
            .filter(|m| m.version > applied)
            .collect())
    }

    // ── Migration execution ─────────────────────────────────────────────

    /// Run all pending migrations inside a single immediate transaction.
    /// Returns the list of migrations that were applied.
    pub fn migrate(&mut self) -> Result<Vec<MigrationSource>, CarryCtxError> {
        self.validate_schema_compatibility()?;
        let pending = self.pending_migrations_raw()?;
        if pending.is_empty() {
            return Ok(Vec::new());
        }
        self.apply_migrations_inner_with_backup(&pending)
    }

    /// Apply an explicit list of migrations in order.
    pub fn apply_migrations(&mut self, sources: &[MigrationSource]) -> Result<(), CarryCtxError> {
        self.apply_migrations_inner_with_backup(sources).map(|_| ())
    }

    fn apply_migrations_inner_with_backup(
        &mut self,
        sources: &[MigrationSource],
    ) -> Result<Vec<MigrationSource>, CarryCtxError> {
        if !sources.is_empty() {
            self.backup_before_migrations()?;
        }
        let rebuilds_tables = sources
            .iter()
            .any(|source| source.version == 12 || source.version == 13);
        let previous_foreign_keys = rebuilds_tables
            .then(|| {
                self.conn
                    .query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
                    .map(|value| value != 0)
                    .map_err(db_err)
            })
            .transpose()?;

        let migration_result = if rebuilds_tables {
            self.conn
                .execute_batch("PRAGMA foreign_keys=OFF")
                .map_err(db_err)
                .and_then(|_| self.apply_migrations_inner(sources))
        } else {
            self.apply_migrations_inner(sources)
        };

        let restoration_result = previous_foreign_keys.map(|enabled| {
            self.conn
                .execute_batch(if enabled {
                    "PRAGMA foreign_keys=ON"
                } else {
                    "PRAGMA foreign_keys=OFF"
                })
                .map_err(db_err)
        });

        match (migration_result, restoration_result) {
            (Err(migration_error), _) => Err(migration_error),
            (Ok(_), Some(Err(restoration_error))) => Err(restoration_error),
            (Ok(applied), _) => Ok(applied),
        }
    }

    fn backup_before_migrations(&self) -> Result<(), CarryCtxError> {
        let has_migration_table: bool = self
            .conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_migrations')",
                [],
                |row| row.get(0),
            )
            .map_err(db_err)?;
        if !has_migration_table || self.applied_version_raw()? == 0 {
            return Ok(());
        }

        let db_path = Path::new(self.conn.path().ok_or_else(|| {
            CarryCtxError::new(
                "BACKUP_FAILED",
                "Cannot determine the project database path for migration backup.",
                ExitCode::Database,
            )
        })?);
        let backup_dir = db_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("backups");
        std::fs::create_dir_all(&backup_dir).map_err(|e| {
            CarryCtxError::new(
                "BACKUP_FAILED",
                format!("Failed to create migration backup directory: {e}"),
                ExitCode::Database,
            )
            .with_source(e)
        })?;
        let mut backup_path = backup_dir.join(format!(
            "state_{}_{}.sqlite",
            chrono::Utc::now().format("%Y%m%d_%H%M%S_%f"),
            ulid::Ulid::generate()
        ));
        for attempt in 0..10 {
            match self.create_backup(&backup_path) {
                Ok(()) => break,
                Err(error) if error.code == "BACKUP_FAILED" && attempt < 9 => {
                    backup_path = backup_dir.join(format!(
                        "state_{}_{}.sqlite",
                        chrono::Utc::now().format("%Y%m%d_%H%M%S_%f"),
                        attempt + 1
                    ));
                }
                Err(error) => return Err(error),
            }
        }

        let backup = Self::open_readonly(&backup_path)?;
        let integrity: String = backup
            .connection()
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .map_err(db_err)?;
        if integrity != "ok" {
            return Err(CarryCtxError::new(
                "BACKUP_INTEGRITY_FAILED",
                format!("Integrity check failed on migration backup: {integrity}"),
                ExitCode::Database,
            ));
        }
        Ok(())
    }

    fn apply_migrations_inner(
        &mut self,
        sources: &[MigrationSource],
    ) -> Result<Vec<MigrationSource>, CarryCtxError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_err)?;

        let mut applied = Vec::new();
        for source in sources {
            let has_migration_table: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_migrations')",
                    [],
                    |row| row.get(0),
                )
                .map_err(db_err)?;
            if has_migration_table {
                let already_applied: bool = tx
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = ?1)",
                        [source.version],
                        |row| row.get(0),
                    )
                    .map_err(db_err)?;
                if already_applied {
                    continue;
                }
            }
            let cksum = checksum_sql(source.sql);
            if source.version == 12 {
                add_column_if_missing(&tx, "agents", "kind", "TEXT")?;
                add_column_if_missing(&tx, "tasks", "required_role", "TEXT")?;
                add_column_if_missing(&tx, "tasks", "team_id", "TEXT")?;
            }
            tx.execute_batch(source.sql).map_err(|e| {
                CarryCtxError::new(
                    "MIGRATION_FAILED",
                    format!("Migration {} failed: {e}", source.name),
                    ExitCode::Database,
                )
                .with_source(e)
            })?;

            let now = chrono::Utc::now().to_rfc3339();
            tx.execute(
                "INSERT INTO schema_migrations (version, name, checksum, applied_at) VALUES (?1, ?2, ?3, ?4)",
                params![source.version, source.name, cksum, now],
            )
            .map_err(db_err)?;
            applied.push(source.clone());
        }

        tx.commit().map_err(db_err)?;
        Ok(applied)
    }

    /// Apply a single migration by version (for targeted apply).
    pub fn apply_version(&mut self, version: i64) -> Result<(), CarryCtxError> {
        let source = migration_sources()
            .into_iter()
            .find(|m| m.version == version)
            .ok_or_else(|| {
                CarryCtxError::new(
                    "MIGRATION_NOT_FOUND",
                    format!("Migration version {version} not found"),
                    ExitCode::MigrationRequired,
                )
            })?;
        self.apply_migrations(&[source])
    }

    /// Check whether the database schema is fully up to date.
    pub fn is_up_to_date(&self) -> Result<bool, CarryCtxError> {
        self.validate_schema_compatibility()?;
        let pending = self.pending_migrations_raw()?.len();
        Ok(pending == 0)
    }

    /// Verify all applied migration checksums match the bundled sources.
    pub fn verify_checksums(&self) -> Result<Vec<String>, CarryCtxError> {
        self.validate_schema_compatibility()?;
        self.verify_checksums_raw()
    }

    fn verify_checksums_raw(&self) -> Result<Vec<String>, CarryCtxError> {
        let applied = self.list_applied_migrations_raw()?;
        let all_sources = migration_sources();
        let sources: std::collections::HashMap<i64, &MigrationSource> =
            all_sources.iter().map(|s| (s.version, s)).collect();

        let mut mismatches = Vec::new();
        for m in &applied {
            let expected = sources.get(&m.version).map(|s| checksum_sql(s.sql));
            match expected {
                Some(cksum) if cksum != m.checksum => {
                    mismatches.push(format!(
                        "Migration {} (v{}): stored={}, expected={}",
                        m.name, m.version, m.checksum, cksum
                    ));
                }
                None => {
                    mismatches.push(format!(
                        "Migration {} (v{}) has no matching source",
                        m.name, m.version
                    ));
                }
                _ => {}
            }
        }
        Ok(mismatches)
    }

    fn pending_migrations_raw(&self) -> Result<Vec<MigrationSource>, CarryCtxError> {
        let applied = self.applied_version_raw()?;
        Ok(migration_sources()
            .into_iter()
            .filter(|m| m.version > applied)
            .collect())
    }

    pub fn validate_schema_compatibility(&self) -> Result<(), CarryCtxError> {
        let bundled = migration_sources();
        let applied_migrations = self.list_applied_migrations_raw()?;
        let applied = applied_migrations.last().map_or(0, |m| m.version);
        let max_bundled = bundled.last().map_or(0, |m| m.version);
        if applied > max_bundled {
            return Err(CarryCtxError::migration_required(format!(
                "Database schema version {applied} is newer than this binary supports (max {max_bundled})."
            )));
        }
        for (index, migration) in applied_migrations.iter().enumerate() {
            if bundled.get(index).map(|source| source.version) != Some(migration.version) {
                return Err(CarryCtxError::migration_required(format!(
                    "Database migration history has a missing interior version before {}.",
                    migration.version
                )));
            }
        }
        if !self.verify_checksums_raw()?.is_empty() {
            return Err(CarryCtxError::migration_required(
                "Database migration checksum verification failed.",
            ));
        }
        Ok(())
    }

    /// Create a verified backup using VACUUM INTO.
    pub fn create_backup(&self, path: impl AsRef<Path>) -> Result<(), CarryCtxError> {
        let requested = path.as_ref();
        let mut destination = requested.to_path_buf();
        for attempt in 0..100 {
            if attempt > 0 {
                destination = requested.with_file_name(format!(
                    "{}_{}",
                    requested
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("backup.sqlite"),
                    attempt
                ));
            }
            if destination.exists() {
                continue;
            }

            let dest = destination.to_string_lossy().replace('\'', "''");
            match self.conn.execute_batch(&format!("VACUUM INTO '{dest}'")) {
                Ok(()) => {
                    let backup = Self::open_readonly(&destination)?;
                    let integrity: String = backup
                        .connection()
                        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
                        .map_err(db_err)?;
                    if integrity != "ok" {
                        return Err(CarryCtxError::new(
                            "BACKUP_INTEGRITY_FAILED",
                            format!("Integrity check failed on backup: {integrity}"),
                            ExitCode::Database,
                        ));
                    }
                    return Ok(());
                }
                Err(_error) if attempt < 99 => continue,
                Err(error) => {
                    return Err(CarryCtxError::new(
                        "BACKUP_FAILED",
                        format!("VACUUM INTO failed: {error}"),
                        ExitCode::Database,
                    )
                    .with_source(error));
                }
            }
        }
        Err(CarryCtxError::new(
            "BACKUP_FAILED",
            "Could not allocate a unique backup destination.",
            ExitCode::Database,
        ))
    }

    /// Create a fresh project database at the given path.
    pub fn create_fresh(path: impl AsRef<Path>) -> Result<Self, CarryCtxError> {
        let mut db = Self::open(path)?;
        db.migrate()?;
        Ok(db)
    }

    /// Begin an immediate transaction for a UnitOfWork.
    pub fn begin_unit_of_work(
        &mut self,
    ) -> Result<super::unit_of_work::UnitOfWork<'_>, CarryCtxError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_err)?;
        Ok(super::unit_of_work::UnitOfWork::new(tx))
    }
}

fn db_err(e: rusqlite::Error) -> CarryCtxError {
    CarryCtxError::database_error(format!("SQLite error: {e}")).with_source(e)
}

fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<(), CarryCtxError> {
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2)",
            params![table, column],
            |row| row.get(0),
        )
        .map_err(db_err)?;
    if !exists {
        let allowed_table = matches!(table, "agents" | "tasks");
        let allowed_column = matches!(column, "kind" | "required_role" | "team_id");
        if !allowed_table || !allowed_column {
            return Err(CarryCtxError::database_error("Invalid migration column"));
        }
        conn.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {definition}"
        ))
        .map_err(db_err)?;
    }
    Ok(())
}
