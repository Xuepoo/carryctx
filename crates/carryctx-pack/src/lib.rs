//! carryctx-pack — ctxpack interchange crate (P4).
//!
//! Owns `manifest`, `format_version`, JSONL encoding, validation, migration,
//! checksum, and reader/writer per `recording/research/002.md` §ctxpack and
//! `design/002-workspace-crates.md` §2.5.
//!
//! Depends only on `carryctx-core` (pure). No `rusqlite`, no `git2`, no
//! `clap`, no network. See `design/2026-09-09-ctxpack-export-import.md` for
//! the v1 `carryctx-pack-dir` interchange contract.

pub mod checksum;
pub mod io;
pub mod manifest;
pub mod migration;

pub use checksum::{checksum_reader, checksum_writer, sha256_hex};
pub use io::{PackBundle, read_bundle, read_table_file, write_bundle, write_table_file};
pub use manifest::{
    PACK_FORMAT, PACK_FORMAT_VERSION, PACK_MANIFEST_FILE, PACK_PROJECT_FILE, PACK_TABLE_FILES,
    PackManifest, PackSource, check_counts, prune_worktrees, reanchor_project,
    validate_manifest_value,
};
pub use migration::{MediaType, migrate_manifest_value};
