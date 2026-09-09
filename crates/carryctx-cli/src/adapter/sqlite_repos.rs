// P2 thin bridge: repository implementations moved to `carryctx-sqlite`.
// Keep every `crate::adapter::sqlite_repos::*` import valid while the
// persistence owner lives in the `carryctx-sqlite` crate.
pub use carryctx_sqlite::repos::*;
