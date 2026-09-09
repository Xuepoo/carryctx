//! jj backend — optional runtime backend (Tier 2 / experimental).
//!
//! Thin `Command::new("jj")` passthrough with no heavy `libjj` dependency.
//! Keep jj fail-closed: where jj cannot safely emulate Git behavior, return
//! a typed error rather than diverging silently. See `recording/research/002.md` §3.

use std::path::{Path, PathBuf};
use std::process::Command;

use carryctx_core::domain::git_snapshot::GitSnapshot;
use carryctx_core::error::CarryCtxError;

use crate::backend::{BackendKind, VcsBackend, Workspace, WorkspaceRequest};
use crate::capabilities::VcsCapabilities;

/// Optional jj backend — reached only when `auto` detects `.jj/` or when
/// `jj` is explicitly selected. Thin CLI wrapper; single binary ships both
/// backends with no Cargo feature matrix (see `recording/research/002.md` §3).
pub struct JjBackend {
    jj_path: String,
    git: crate::git::GitBackend,
}

impl JjBackend {
    pub fn new() -> Self {
        Self {
            jj_path: "jj".into(),
            git: crate::git::GitBackend::new(),
        }
    }

    pub fn with_path(jj_path: impl Into<String>) -> Self {
        Self {
            jj_path: jj_path.into(),
            git: crate::git::GitBackend::new(),
        }
    }

    fn jj_available(&self) -> bool {
        Command::new(&self.jj_path)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn run_jj(&self, cwd: &Path, args: &[&str]) -> Result<std::process::Output, CarryCtxError> {
        let mut cmd = Command::new(&self.jj_path);
        cmd.current_dir(cwd);
        cmd.args(args);
        // jj inherits the same env-clear discipline used for git; it also
        // resolves by `-C`/`cwd`, not by ambient GIT_* state.
        cmd.env_clear();
        if let Ok(path) = std::env::var("PATH") {
            cmd.env("PATH", path);
        }
        cmd.output()
            .map_err(|e| CarryCtxError::git_error(format!("Failed to run jj: {e}")))
    }
}

impl Default for JjBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl VcsBackend for JjBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Jj
    }

    fn repository_root(&self, start_path: &Path) -> Result<PathBuf, CarryCtxError> {
        // jj colocated repos still have a real Git root; reuse Git discovery
        // so `<git-common-dir>/carryctx/state.sqlite` stays correct.
        self.git.repository_root(start_path)
    }

    fn git_common_dir(&self, start_path: &Path) -> Result<PathBuf, CarryCtxError> {
        self.git.git_common_dir(start_path)
    }

    fn head(&self, cwd: &Path) -> Result<Option<String>, CarryCtxError> {
        if self.jj_available() {
            // jj colocated: Git HEAD is exported by jj, still authoritative.
            // Prefer jj's view when available, fall back to git.
            let out = self.run_jj(
                cwd,
                &[
                    "log",
                    "-r",
                    "@",
                    "--no-graph",
                    "-T",
                    "commit_id",
                    "--limit",
                    "1",
                ],
            );
            if let Ok(output) = out {
                if output.status.success() {
                    let id = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if !id.is_empty() {
                        return Ok(Some(id));
                    }
                }
            }
        }
        self.git.head(cwd)
    }

    fn status(&self, cwd: &Path) -> Result<GitSnapshot, CarryCtxError> {
        // jj colocated: the staged/unstaged split is unreliable (see git.rs
        // comment). Reuse Git snapshot which already collapses under jj.
        self.git.get_snapshot(cwd)
    }

    fn create_workspace(
        &self,
        repo_root: &Path,
        request: WorkspaceRequest,
    ) -> Result<Workspace, CarryCtxError> {
        if self.jj_available() {
            let path_str = request.path.to_string_lossy().to_string();
            let out = self.run_jj(repo_root, &["workspace", "add", &path_str])?;
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                return Err(CarryCtxError::git_error(format!(
                    "jj workspace add failed: {}",
                    stderr.trim()
                )));
            }
            let head = self.head(&request.path).ok().flatten();
            return Ok(Workspace {
                path: request.path,
                branch: Some(request.branch),
                head,
            });
        }
        // jj CLI not present — fail-closed rather than silently emulating
        // a jj workspace via git worktree add.
        Err(CarryCtxError::validation_error(
            "jj workspace creation requested but `jj` CLI is not available on PATH. Install jj or use the Git backend.",
        ))
    }

    fn remove_workspace(
        &self,
        _repo_root: &Path,
        _path: &Path,
        _force: bool,
    ) -> Result<(), CarryCtxError> {
        // jj workspace removal is distinct from `git worktree remove` and has
        // no stable `carryctx worktree remove` analogue yet; keep it fail-closed
        // so callers must use `jj workspace forget` directly.
        Err(CarryCtxError::validation_error(
            "Refusing to remove a live workspace from a jj-colocated repository: `git worktree remove` can leave jj workspace state inconsistent. Use `jj workspace forget` for jj-managed workspaces, or remove only the CarryCtx registration after the directory is gone.",
        ))
    }

    fn capabilities(&self) -> VcsCapabilities {
        VcsCapabilities::jj()
    }
}
