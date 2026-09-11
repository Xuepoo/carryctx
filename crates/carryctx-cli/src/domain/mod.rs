// P2/P4 thin bridges: domain owned by `carryctx-core` (P2) and `carryctx-pack` (P4).
pub use carryctx_core::domain::agent;
pub use carryctx_core::domain::checkpoint;
pub use carryctx_core::domain::cleanup;
pub use carryctx_core::domain::collaboration;
pub use carryctx_core::domain::config;
pub use carryctx_core::domain::context;
pub use carryctx_core::domain::dependency;
pub use carryctx_core::domain::duration;
pub use carryctx_core::domain::git_snapshot;
pub use carryctx_core::domain::graph;
pub use carryctx_core::domain::ids;
// P4: pack manifest/format_version/validation owned by `carryctx-pack`; keep legacy path for zero CLI change.
pub use carryctx_core::domain::preset;
pub use carryctx_core::domain::progress;
pub use carryctx_core::domain::search;
pub use carryctx_core::domain::session;
pub use carryctx_core::domain::task;
pub use carryctx_core::domain::team;
pub use carryctx_core::domain::trust;
pub use carryctx_pack::manifest as pack;
