// Root facade: re-export the workspace crates so `carryctx::adapter::*`,
// `carryctx::application::*`, `carryctx::output`, etc. keep compiling.
// The real implementations live in `crates/carryctx-cli/src/` (CLI shell),
// `crates/carryctx-core/src/` (pure domain/ports), `crates/carryctx-sqlite`,
// `crates/carryctx-vcs`, and `crates/carryctx-pack`. Only the binary entry
// (`main.rs` thin wrapper) still lives under root `src/`; `commands/` moved
// to `crates/carryctx-cli` in Step2 (CTX-0134). Step3 deletes this facade.

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
