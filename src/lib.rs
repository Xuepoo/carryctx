// Root facade: re-export the workspace crates so `carryctx::adapter::*`,
// `carryctx::application::*`, `carryctx::output`, etc. keep compiling.
// The real implementations live in `crates/carryctx-cli/src/` (CLI shell),
// `crates/carryctx-core/src/` (pure domain/ports), `crates/carryctx-sqlite`,
// `crates/carryctx-vcs`, and `crates/carryctx-pack`. Only `commands/` and
// `main.rs` still live under root `src/` (binary entry); they move to
// `crates/carryctx-cli` in the follow-on migration.

// Re-export pure core so external callers can migrate to carryctx_core::*
#[allow(unused_imports)]
pub use carryctx_core::domain as core_domain;
#[allow(unused_imports)]
pub use carryctx_core::error as core_error;
#[allow(unused_imports)]
pub use carryctx_core::repository as core_repository;

// CLI shell (owns adapter/application/output/repository + error)
#[allow(unused_imports)]
pub use carryctx_cli::adapter;
#[allow(unused_imports)]
pub use carryctx_cli::application;
#[allow(unused_imports)]
pub use carryctx_cli::domain;
#[allow(unused_imports)]
pub use carryctx_cli::error;
#[allow(unused_imports)]
pub use carryctx_cli::output;
#[allow(unused_imports)]
pub use carryctx_cli::repository;
#[allow(unused_imports)]
pub use carryctx_pack as pack;
#[allow(unused_imports)]
pub use carryctx_vcs as vcs;
#[allow(unused_imports)]
pub use carryctx_vcs::backend::{BackendKind as VcsBackendKind, VcsBackend};
#[allow(unused_imports)]
pub use carryctx_vcs::capabilities::VcsCapabilities as CoreVcsCapabilities;

// P5 facade: root aggregates `carryctx-cli` as the CLI shell. The CLI crate
// owns `adapter`/`application` (imperative wiring), `commands`, `output`, and
// `clap` translation. Root remains import-compatible via `carryctx::`.
#[allow(unused_imports)]
pub use carryctx_cli as cli;
