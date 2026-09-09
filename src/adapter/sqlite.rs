// P2 thin bridge: the sqlite persistence owner moved to `carryctx-sqlite`.
// This file intentionally stays as a re-export shim so every existing
// `crate::adapter::sqlite::*` import keeps compiling.
pub use carryctx_sqlite::database::{
    Migration, MigrationSource, ProjectDatabase, bundled_schema_version, checksum_sql,
};
