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
        db.apply_pragmas()?;
        Ok(db)
    }

    /// Apply standard PRAGMAs to the connection.
    fn apply_pragmas(&mut self) -> Result<(), CarryCtxError> {
        self.conn
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA foreign_keys=ON;
                 PRAGMA busy_timeout=5000;
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
        let applied = self.applied_version()?;
        Ok(migration_sources()
            .into_iter()
            .filter(|m| m.version > applied)
            .collect())
    }

    // ── Migration execution ─────────────────────────────────────────────

    /// Run all pending migrations inside a single immediate transaction.
    /// Returns the list of migrations that were applied.
    pub fn migrate(&mut self) -> Result<Vec<MigrationSource>, CarryCtxError> {
        let pending = self.pending_migrations()?;
        if pending.is_empty() {
            return Ok(Vec::new());
        }
        self.apply_migrations(&pending)?;
        Ok(pending)
    }

    /// Apply an explicit list of migrations in order.
    pub fn apply_migrations(&mut self, sources: &[MigrationSource]) -> Result<(), CarryCtxError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_err)?;

        for source in sources {
            let cksum = checksum_sql(source.sql);
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
        }

        tx.commit().map_err(db_err)?;
        Ok(())
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
        let pending = self.pending_migrations()?.len();
        Ok(pending == 0)
    }

    /// Verify all applied migration checksums match the bundled sources.
    pub fn verify_checksums(&self) -> Result<Vec<String>, CarryCtxError> {
        let applied = self.list_applied_migrations()?;
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

    /// Create a verified backup using VACUUM INTO.
    pub fn create_backup(&self, path: impl AsRef<Path>) -> Result<(), CarryCtxError> {
        let dest = path.as_ref().to_string_lossy().replace('\'', "''");
        self.conn
            .execute_batch(&format!("VACUUM INTO '{dest}'"))
            .map_err(|e| {
                CarryCtxError::new(
                    "BACKUP_FAILED",
                    format!("VACUUM INTO failed: {e}"),
                    ExitCode::Database,
                )
                .with_source(e)
            })?;

        let mut stmt = self
            .conn
            .prepare("PRAGMA integrity_check")
            .map_err(db_err)?;
        let integrity: String = stmt.query_row([], |row| row.get(0)).map_err(db_err)?;
        if integrity != "ok" {
            return Err(CarryCtxError::new(
                "BACKUP_INTEGRITY_FAILED",
                format!("Integrity check failed on backup: {integrity}"),
                ExitCode::Database,
            ));
        }
        Ok(())
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
