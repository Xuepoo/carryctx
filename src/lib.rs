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
