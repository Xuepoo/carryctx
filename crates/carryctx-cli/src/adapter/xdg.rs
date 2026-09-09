// P3 thin bridge: xdg paths are VCS-adjacent (git_common_dir anchoring)
// but remain a lightweight filesystem helper. Re-export shim keeps
// `crate::adapter::xdg::*` stable; the canonical owner for git-anchored
// paths is now documented alongside `carryctx-vcs`.
pub use carryctx_vcs::xdg::XdgPaths;
