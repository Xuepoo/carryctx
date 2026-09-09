// P2 thin bridge: repository contracts owned by `carryctx-core`; persistence impls live in `carryctx-sqlite`.
pub use carryctx_core::repository::agent;
pub use carryctx_core::repository::checkpoint;
pub use carryctx_core::repository::cleanup;
pub use carryctx_core::repository::collaboration;
pub use carryctx_core::repository::dependency;
pub use carryctx_core::repository::event;
pub use carryctx_core::repository::progress;
pub use carryctx_core::repository::session;
pub use carryctx_core::repository::task;
pub use carryctx_core::repository::team;
pub use carryctx_core::repository::worktree;

pub use carryctx_core::repository::CheckpointRepository;
pub use carryctx_core::repository::DependencyRepository;
pub use carryctx_core::repository::{AgentFilter, AgentRepository, NewAgent};
pub use carryctx_core::repository::{CleanupRecord, CleanupRepository, NewCleanupRequest};
pub use carryctx_core::repository::{
    DecisionRepository, HandoffFilter, HandoffRepository, ScopeRepository,
};
pub use carryctx_core::repository::{EventFilter, EventRecord, EventRepository, NewEvent};
pub use carryctx_core::repository::{
    NewProgressItem, ProgressFilter, ProgressItemRecord, ProgressRepository,
};
pub use carryctx_core::repository::{NewSession, SessionRecord, SessionRepository};
pub use carryctx_core::repository::{NewTask, TaskFilter, TaskRecord, TaskRepository};
pub use carryctx_core::repository::{NewTeam, NewTeamMember, TeamRepository};
pub use carryctx_core::repository::{NewWorktree, WorktreeRecord, WorktreeRepository};

// graph/search have SQLite-backed impls in `carryctx-sqlite` but their contracts are still implicit;
// keep the bridge modules for backward-compat.
pub mod graph;
pub mod search;
pub use graph::GraphRepository;
pub use search::{SearchOptions, SearchRepository};
