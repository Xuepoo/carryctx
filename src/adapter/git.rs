//! Git CLI adapter - discovers repositories, captures snapshots, manages worktrees

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::domain::git_snapshot::{DiffStats, GitSnapshot, RenamedFile};
use crate::error::CarryCtxError;

/// Information about a discovered Git repository
pub struct GitProject {
    pub repository_root: PathBuf,
    pub git_common_dir: PathBuf,
    pub worktree_root: PathBuf,
    pub branch: Option<String>,
    pub head: Option<String>,
}

/// Detect whether a Git repository is colocated with a Jujutsu (jj) repository.
///
/// jj's colocated mode (`jj git init --colocate`) keeps a real `.git/` directory
/// alongside `.jj/` as siblings. Checking for that sibling directory is a reliable,
/// dependency-free signal: it requires neither the `jj` binary on `PATH` nor any
/// jj-specific parsing, and never changes behavior for plain Git repositories.
pub fn detect_jj_colocation(git_common_dir: &Path) -> bool {
    // `git_common_dir` is typically `<repo>/.git` (or `<repo>/.git/worktrees/<name>`
    // is never returned here since we always resolve `--git-common-dir`). The `.jj`
    // marker sits as a sibling of `.git` at the repository root, i.e. one level up
    // from a common dir literally named `.git`, and two levels up from a
    // `.git/worktrees/<name>` style path is not applicable since git-common-dir
    // already collapses worktrees to the main `.git` directory.
    git_common_dir
        .parent()
        .map(|repo_root| repo_root.join(".jj").is_dir())
        .unwrap_or(false)
}

/// Git CLI wrapper
pub struct GitCli {
    git_path: String,
}

impl GitCli {
    pub fn new() -> Self {
        Self {
            git_path: "git".into(),
        }
    }

    pub fn with_path(git_path: impl Into<String>) -> Self {
        Self {
            git_path: git_path.into(),
        }
    }

    /// Discover Git repository from a starting path
    pub fn discover(&self, start_path: &Path) -> Result<GitProject, CarryCtxError> {
        let root_raw = self.capture_stdout(start_path, ["rev-parse", "--show-toplevel"])?;
        let root_trimmed = root_raw.trim();
        let root_path = Path::new(root_trimmed);

        let common_dir = self.capture_stdout(
            start_path,
            ["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?;
        let head = self
            .capture_stdout(start_path, ["rev-parse", "HEAD"])
            .ok()
            .map(|h| h.trim().to_string());
        let worktree_root = self.worktree_root(root_path)?;
        let branch = self.get_branch(root_path)?;

        Ok(GitProject {
            repository_root: root_path.to_path_buf(),
            git_common_dir: PathBuf::from(common_dir.trim()),
            worktree_root: PathBuf::from(worktree_root.trim()),
            branch,
            head,
        })
    }

    fn run_git_args<I, S>(&self, cwd: &Path, args: I) -> Result<Command, CarryCtxError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let mut cmd = Command::new(&self.git_path);
        cmd.arg("-C");
        cmd.arg(cwd);
        cmd.args(args);
        cmd.env_clear();
        if let Ok(path) = std::env::var("PATH") {
            cmd.env("PATH", path);
        }
        Ok(cmd)
    }

    fn capture_stdout<I, S>(&self, cwd: &Path, args: I) -> Result<String, CarryCtxError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let mut cmd = self.run_git_args(cwd, args)?;
        let output = cmd
            .output()
            .map_err(|e| CarryCtxError::git_error(format!("Failed to run git: {e}")))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(CarryCtxError::git_error(format!(
                "Git command failed: {stderr}"
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    fn get_branch(&self, cwd: &Path) -> Result<Option<String>, CarryCtxError> {
        let mut cmd = self.run_git_args(cwd, ["symbolic-ref", "--quiet", "--short", "HEAD"])?;
        let output = cmd
            .output()
            .map_err(|e| CarryCtxError::git_error(format!("Failed to get branch: {e}")))?;
        if output.status.success() {
            Ok(Some(
                String::from_utf8_lossy(&output.stdout).trim().to_string(),
            ))
        } else {
            Ok(None)
        }
    }

    fn worktree_root(&self, cwd: &Path) -> Result<String, CarryCtxError> {
        let mut cmd = self.run_git_args(cwd, ["rev-parse", "--show-cdup"])?;
        let output = cmd
            .output()
            .map_err(|e| CarryCtxError::git_error(format!("Failed to get cdup: {e}")))?;
        let cdup = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if cdup.is_empty() {
            return Ok(cwd.to_string_lossy().to_string());
        }
        let root = cwd.join(Path::new(&cdup));
        Ok(root.to_string_lossy().to_string())
    }

    /// Get the current Git worktree snapshot
    pub fn get_snapshot(&self, cwd: &Path) -> Result<GitSnapshot, CarryCtxError> {
        let branch = self.get_branch(cwd)?;
        let head = self
            .capture_stdout(cwd, ["rev-parse", "HEAD"])
            .ok()
            .map(|h| h.trim().to_string());
        let vcs_backend = match self
            .capture_stdout(
                cwd,
                ["rev-parse", "--path-format=absolute", "--git-common-dir"],
            )
            .ok()
        {
            Some(common_dir) if detect_jj_colocation(Path::new(common_dir.trim())) => {
                crate::domain::git_snapshot::VcsBackend::Jj
            }
            _ => crate::domain::git_snapshot::VcsBackend::Git,
        };
        let status = self.capture_stdout(cwd, ["status", "--porcelain"])?;
        let dirty = !status.is_empty();
        let mut staged = Vec::new();
        let mut modified = Vec::new();
        let mut deleted = Vec::new();
        let mut renamed = Vec::new();
        let mut untracked_vec = Vec::new();

        for line in status.lines() {
            if line.is_empty() {
                continue;
            }
            let (xy, path) = line.split_at(2);
            let path = path.trim();
            match xy.trim() {
                "M" => modified.push(path.to_string()),
                "A" => staged.push(path.to_string()),
                "D" => deleted.push(path.to_string()),
                "R" | "RM" | "RD" => {}
                "??" => untracked_vec.push(path.to_string()),
                _ => {}
            }
            if xy.trim().starts_with('R') {
                if let Some((from, to)) = path.split_once(" -> ") {
                    renamed.push(RenamedFile {
                        from: from.to_string(),
                        to: to.to_string(),
                    });
                }
            }
        }

        // The staged/unstaged/untracked three-way split answers "did the agent
        // deliberately stage this", which stops meaning anything once jj's
        // automatic working-copy snapshotting starts writing to the Git index
        // as a side effect of read-only commands (see
        // carryctx-docs/plans/2026-07-25-jujutsu-compatibility.md, §2.3).
        // `changed_files` stays accurate under both backends; collapse the
        // unreliable split into it and clear the three lists under jj so
        // consumers don't read a false signal.
        let mut changed_files: Vec<String> = staged
            .iter()
            .chain(modified.iter())
            .chain(untracked_vec.iter())
            .cloned()
            .collect();
        changed_files.sort();
        changed_files.dedup();

        if vcs_backend == crate::domain::git_snapshot::VcsBackend::Jj {
            staged.clear();
            modified.clear();
            untracked_vec.clear();
        }

        let diff_stats = self.get_diff_stats(cwd)?;

        Ok(GitSnapshot {
            branch,
            head,
            dirty,
            vcs_backend,
            staged,
            modified,
            deleted,
            renamed,
            untracked: untracked_vec,
            changed_files,
            diff_stats,
        })
    }

    fn get_diff_stats(&self, cwd: &Path) -> Result<Option<DiffStats>, CarryCtxError> {
        let mut cmd = self.run_git_args(cwd, ["diff", "--numstat"])?;
        let output = cmd
            .output()
            .map_err(|e| CarryCtxError::git_error(format!("Failed to get diff stats: {e}")))?;
        if !output.status.success() {
            return Ok(None);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut files = 0i64;
        let mut insertions = 0i64;
        let mut deletions = 0i64;
        for line in stdout.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 {
                insertions += parts[0].parse::<i64>().unwrap_or(0);
                deletions += parts[1].parse::<i64>().unwrap_or(0);
                files += 1;
            }
        }
        if files == 0 {
            return Ok(None);
        }
        Ok(Some(DiffStats {
            files,
            insertions,
            deletions,
        }))
    }

    /// List Git worktrees
    pub fn list_worktrees(&self, cwd: &Path) -> Result<Vec<WorktreeEntry>, CarryCtxError> {
        let output = self.capture_stdout(cwd, ["worktree", "list", "--porcelain"])?;
        let mut entries = Vec::new();
        let mut current: Option<WorktreeEntry> = None;
        for line in output.lines() {
            if line.starts_with("worktree ") {
                if let Some(entry) = current.take() {
                    entries.push(entry);
                }
                let path = line
                    .strip_prefix("worktree ")
                    .unwrap_or("")
                    .trim()
                    .to_string();
                current = Some(WorktreeEntry {
                    path,
                    branch: None,
                    head: None,
                    detached: false,
                });
            } else if line.starts_with("HEAD ") {
                if let Some(ref mut entry) = current {
                    entry.head = Some(line.strip_prefix("HEAD ").unwrap_or("").trim().to_string());
                }
            } else if line.starts_with("branch ") {
                if let Some(ref mut entry) = current {
                    entry.branch = Some(
                        line.strip_prefix("branch ")
                            .unwrap_or("")
                            .trim()
                            .to_string(),
                    );
                }
            } else if line == "detached" {
                if let Some(ref mut entry) = current {
                    entry.detached = true;
                }
            }
        }
        if let Some(entry) = current {
            entries.push(entry);
        }
        Ok(entries)
    }

    /// Create a Git worktree
    pub fn create_worktree(
        &self,
        repo_root: &Path,
        path: &Path,
        branch: &str,
        base: Option<&str>,
    ) -> Result<(), CarryCtxError> {
        let branch_exists = self.has_branch(repo_root, branch)?;
        let mut args = vec!["worktree", "add"];

        if !branch_exists {
            args.push("-b");
            args.push(branch);
        }

        args.push(path.to_str().unwrap_or_default());

        if !branch_exists {
            if let Some(b) = base {
                args.push(b);
            }
        } else {
            args.push(branch);
        }

        self.capture_stdout(repo_root, args)?;
        Ok(())
    }

    /// Check if a branch exists
    pub fn has_branch(&self, cwd: &Path, branch: &str) -> Result<bool, CarryCtxError> {
        let mut cmd = self.run_git_args(
            cwd,
            [
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
        )?;
        let output = cmd
            .output()
            .map_err(|e| CarryCtxError::git_error(format!("Failed to check branch: {e}")))?;
        Ok(output.status.success())
    }
}

impl Default for GitCli {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct WorktreeEntry {
    pub path: String,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub detached: bool,
}

#[cfg(test)]
mod jj_colocation_tests {
    use super::detect_jj_colocation;
    use std::path::Path;

    #[test]
    fn detects_sibling_jj_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo_root = tmp.path();
        let git_common_dir = repo_root.join(".git");
        std::fs::create_dir_all(&git_common_dir).expect("create .git");
        std::fs::create_dir_all(repo_root.join(".jj")).expect("create .jj");

        assert!(detect_jj_colocation(&git_common_dir));
    }

    #[test]
    fn plain_git_repo_has_no_jj_sibling() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo_root = tmp.path();
        let git_common_dir = repo_root.join(".git");
        std::fs::create_dir_all(&git_common_dir).expect("create .git");

        assert!(!detect_jj_colocation(&git_common_dir));
    }

    #[test]
    fn missing_parent_is_not_colocated() {
        assert!(!detect_jj_colocation(Path::new("/")));
    }
}
