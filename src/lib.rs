// Root facade: re-export the workspace crates so `carryctx::adapter::*`,
// `carryctx::application::*`, `carryctx::output`, etc. keep compiling.
// The real implementations live in `crates/carryctx-cli/src/` (CLI shell),
// `crates/carryctx-core/src/` (pure domain/ports), `crates/carryctx-sqlite`,
// `crates/carryctx-vcs`, and `crates/carryctx-pack`. Only `commands/` and
// `main.rs` still live under root `src/` (binary entry); they move to
// `crates/carryctx-cli` in the follow-on migration.

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
