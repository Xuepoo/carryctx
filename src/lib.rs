pub mod adapter;
pub mod application;
pub mod domain;
pub mod error;
pub mod output;
pub mod repository;

// Re-export pure core so external callers can migrate to carryctx_core::*
#[allow(unused_imports)]
pub use carryctx_core::domain as core_domain;
#[allow(unused_imports)]
pub use carryctx_core::error as core_error;
#[allow(unused_imports)]
pub use carryctx_core::repository as core_repository;

// VCS crate re-export (P3) — coordinator note: root Cargo.toml + src/lib.rs
// are the only shared files with teammate ref-pack (CTX-0126). Commander
// resolves if both touch them; this P3 change adds exactly the VCS surface.
#[allow(unused_imports)]
pub use carryctx_vcs as vcs;
#[allow(unused_imports)]
pub use carryctx_vcs::backend::{BackendKind as VcsBackendKind, VcsBackend};
#[allow(unused_imports)]
pub use carryctx_vcs::capabilities::VcsCapabilities as CoreVcsCapabilities;
