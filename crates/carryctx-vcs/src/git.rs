//! Git backend — Tier 1, stable.
//!
//! Extracted from `src/adapter/git.rs` (P3). Depends only on `carryctx-core`
//! and std `Command`. No `rusqlite`, no `clap`, no network.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use carryctx_core::domain::git_snapshot::{
    DiffStats, GitSnapshot, RenamedFile, VcsBackend as SnapshotBackend,
};
use carryctx_core::error::CarryCtxError;

use crate::backend::{BackendKind, VcsBackend, Workspace, WorkspaceRequest};
use crate::capabilities::VcsCapabilities;
use crate::snapshot::{
    SNAPSHOT_MANIFEST_FILE, SnapshotCommit, SnapshotRefCommit, SnapshotTrailers,
    render_snapshot_message,
};

/// Information about a discovered Git repository
#[derive(Debug, Clone)]
pub struct GitProject {
    pub repository_root: PathBuf,
    pub git_common_dir: PathBuf,
    pub worktree_root: PathBuf,
    pub branch: Option<String>,
    pub head: Option<String>,
}

/// Strip inherited GIT_* state from a spawned git command so it resolves
/// strictly against its explicit working directory / `-C` target.
///
/// CarryCtx always targets repositories by path; honoring an ambient
/// GIT_DIR/GIT_INDEX_FILE from an arbitrary ancestor process makes internal
/// git calls operate on an unrelated repository — corrupting scans, worktree
/// maintenance, and any fixture running under a hook runner (lefthook sets
/// exactly these variables). `GitCli` already isolates via `env_clear`;
/// this applies the same contract to raw spawns.
pub fn isolate_git_env(command: &mut Command) -> &mut Command {
    const GIT_STATE_VARS: &[&str] = &[
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_COMMON_DIR",
        "GIT_NAMESPACE",
        "GIT_CEILING_DIRECTORIES",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_SYSTEM",
    ];
    for var in GIT_STATE_VARS {
        command.env_remove(var);
    }
    command
}

/// Detect whether a Git repository is colocated with a Jujutsu (jj) repository.
///
/// jj's colocated mode (`jj git init --colocate`) keeps a real `.git/` directory
/// alongside `.jj/` as siblings. Checking for that sibling directory is a reliable,
/// dependency-free signal: it requires neither the `jj` binary on `PATH` nor any
/// jj-specific parsing, and never changes behavior for plain Git repositories.
pub fn detect_jj_colocation(git_common_dir: &Path) -> bool {
    git_common_dir
        .parent()
        .map(|repo_root| repo_root.join(".jj").is_dir())
        .unwrap_or(false)
}

/// Git CLI wrapper — also the Tier 1 `VcsBackend` implementation.
pub struct GitBackend {
    git_path: String,
}

// Back-compat alias: the root crate historically exported `GitCli`.
// Keep the name as a type alias so `carryctx_vcs::GitCli` and
// `carryctx_vcs::GitBackend` are interchangeable at the type level.
pub type GitCli = GitBackend;

impl GitBackend {
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
                SnapshotBackend::Jj
            }
            _ => SnapshotBackend::Git,
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

        if vcs_backend == SnapshotBackend::Jj {
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
                    locked: None,
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
            } else if line.starts_with("locked") {
                if let Some(ref mut entry) = current {
                    let reason = line.strip_prefix("locked").unwrap_or("").trim().to_string();
                    entry.locked = Some(reason);
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

    /// Remove a Git worktree.
    ///
    /// Mirrors `git worktree remove`'s own guard: without `force`, git
    /// refuses when the worktree contains modified or untracked files; that
    /// specific refusal is surfaced as `STATE_CONFLICT` (with a `--force`
    /// hint) instead of a generic git error so callers can document a stable
    /// exit code.
    pub fn remove_worktree(
        &self,
        repo_root: &Path,
        path: &Path,
        force: bool,
    ) -> Result<(), CarryCtxError> {
        let mut args: Vec<String> = vec!["worktree".into(), "remove".into()];
        if force {
            args.push("--force".into());
        }
        args.push(path.to_string_lossy().into_owned());

        let mut cmd = self.run_git_args(repo_root, &args)?;
        let output = cmd
            .output()
            .map_err(|e| CarryCtxError::git_error(format!("Failed to run git: {e}")))?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("modified or untracked files")
            || stderr.contains("contains modified or untracked")
            || stderr.contains("use --force to delete it")
        {
            return Err(CarryCtxError::state_conflict(format!(
                "Worktree '{}' contains modified or untracked files; \
                 re-run with --force to remove anyway.",
                path.display()
            )));
        }
        Err(CarryCtxError::git_error(format!(
            "git worktree remove failed: {}",
            stderr.trim()
        )))
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

    // ── Local snapshot-ref plumbing (CTX-0144) ─────────────────────────────
    //
    // Every helper below works on local Git objects only. No index, worktree,
    // or network access: objects are written with `hash-object -w`, trees with
    // `mktree`, commits with `commit-tree`, and the ref with a compare-and-swap
    // `update-ref`. The environment is inherited (so HOME/global Git identity
    // resolve for `commit-tree`) but ambient GIT_* state is stripped via
    // `isolate_git_env`, matching the contract documented on that function.

    /// Build a plumbing command targeting `cwd` by `-C`, with ambient GIT_*
    /// state stripped but the rest of the environment intact.
    fn plumbing_command<I, S>(&self, cwd: &Path, args: I) -> Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let mut cmd = Command::new(&self.git_path);
        cmd.arg("-C");
        cmd.arg(cwd);
        cmd.args(args);
        isolate_git_env(&mut cmd);
        cmd
    }

    /// Run a plumbing command, returning its raw [`std::process::Output`]
    /// without treating a non-zero exit as an error (callers that probe for
    /// absence need the status).
    fn run_plumbing_output<I, S>(
        &self,
        cwd: &Path,
        args: I,
    ) -> Result<std::process::Output, CarryCtxError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        self.plumbing_command(cwd, args)
            .output()
            .map_err(|e| CarryCtxError::git_error(format!("Failed to run git plumbing: {e}")))
    }

    /// Run a plumbing command and fail with `GIT_ERROR` on a non-zero exit.
    fn run_plumbing<I, S>(&self, cwd: &Path, args: I) -> Result<Vec<u8>, CarryCtxError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let output = self.run_plumbing_output(cwd, args)?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(CarryCtxError::git_error(format!(
                "Git plumbing command failed: {}",
                stderr.trim()
            )));
        }
        Ok(output.stdout)
    }

    /// Run a plumbing command feeding `input` on stdin (hash-object,
    /// mktree, commit-tree, update-ref --stdin).
    fn run_plumbing_stdin<I, S>(
        &self,
        cwd: &Path,
        args: I,
        input: &[u8],
    ) -> Result<Vec<u8>, CarryCtxError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let mut cmd = self.plumbing_command(cwd, args);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| CarryCtxError::git_error(format!("Failed to run git plumbing: {e}")))?;
        {
            use std::io::Write as _;
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| CarryCtxError::git_error("Failed to open git stdin."))?;
            stdin
                .write_all(input)
                .map_err(|e| CarryCtxError::git_error(format!("Failed to write git stdin: {e}")))?;
        }
        let output = child.wait_with_output().map_err(|e| {
            CarryCtxError::git_error(format!("Failed to read git plumbing output: {e}"))
        })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(CarryCtxError::git_error(format!(
                "Git plumbing command failed: {}",
                stderr.trim()
            )));
        }
        Ok(output.stdout)
    }

    /// Whether `ref_name` is a syntactically valid full ref name
    /// (`git check-ref-format`, local-only). Callers must pass a `refs/...`
    /// value: `check-ref-format` rejects one-level names such as `main`, so
    /// requiring the prefix first also keeps a leading `-` from being read as
    /// an option.
    pub fn check_ref_format(
        &self,
        repo_root: &Path,
        ref_name: &str,
    ) -> Result<bool, CarryCtxError> {
        if !ref_name.starts_with("refs/") {
            return Ok(false);
        }
        let output = self.run_plumbing_output(repo_root, ["check-ref-format", ref_name])?;
        Ok(output.status.success())
    }

    /// Resolve a ref to its tip commit sha, or `None` when it does not exist.
    pub fn resolve_ref(
        &self,
        repo_root: &Path,
        ref_name: &str,
    ) -> Result<Option<String>, CarryCtxError> {
        let output = self.run_plumbing_output(
            repo_root,
            [
                "rev-parse",
                "--verify",
                "--quiet",
                "--end-of-options",
                ref_name,
            ],
        )?;
        if !output.status.success() {
            return Ok(None);
        }
        let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok((!sha.is_empty()).then_some(sha))
    }

    /// Whether `revision` resolves to a commit in this repository.
    pub fn commit_exists(&self, repo_root: &Path, revision: &str) -> Result<bool, CarryCtxError> {
        let spec = format!("{revision}^{{commit}}");
        let output = self.run_plumbing_output(
            repo_root,
            [
                "rev-parse",
                "--verify",
                "--quiet",
                "--end-of-options",
                spec.as_str(),
            ],
        )?;
        Ok(output.status.success())
    }

    /// Read `file` from `revision`'s tree, or `None` when the path is absent.
    pub fn read_file_at(
        &self,
        repo_root: &Path,
        revision: &str,
        file: &str,
    ) -> Result<Option<Vec<u8>>, CarryCtxError> {
        let spec = format!("{revision}:{file}");
        let probe = self.run_plumbing_output(
            repo_root,
            ["cat-file", "-e", "--end-of-options", spec.as_str()],
        )?;
        if !probe.status.success() {
            return Ok(None);
        }
        let bytes = self.run_plumbing(
            repo_root,
            ["cat-file", "--end-of-options", "blob", spec.as_str()],
        )?;
        Ok(Some(bytes))
    }

    /// The raw commit message (`%B`) of `sha`.
    ///
    /// `git log` (not `git show`, which has no `--end-of-options`) keeps the
    /// revision argument after an explicit end-of-options guard so a
    /// leading-`-` revision can never be read as a flag.
    pub fn commit_message(&self, repo_root: &Path, sha: &str) -> Result<String, CarryCtxError> {
        let bytes = self.run_plumbing(
            repo_root,
            ["log", "-1", "--format=%B", "--end-of-options", sha],
        )?;
        Ok(String::from_utf8_lossy(&bytes).to_string())
    }

    /// Commit shas reachable from `ref_name`, newest first (empty when the ref
    /// is absent).
    pub fn snapshot_history_commits(
        &self,
        repo_root: &Path,
        ref_name: &str,
    ) -> Result<Vec<String>, CarryCtxError> {
        let output =
            self.run_plumbing_output(repo_root, ["rev-list", "--end-of-options", ref_name])?;
        if !output.status.success() {
            return Ok(Vec::new());
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// Write one blob and return its sha.
    pub fn hash_object(&self, repo_root: &Path, bytes: &[u8]) -> Result<String, CarryCtxError> {
        let out = self.run_plumbing_stdin(repo_root, ["hash-object", "-w", "--stdin"], bytes)?;
        Ok(String::from_utf8_lossy(&out).trim().to_string())
    }

    /// Build a tree holding `files` at the root and return its sha. Entries are
    /// sorted by git tree order (plain byte order for blobs) before `mktree`.
    pub fn write_tree(
        &self,
        repo_root: &Path,
        files: &[(String, Vec<u8>)],
    ) -> Result<String, CarryCtxError> {
        let mut entries: Vec<(String, String)> = Vec::with_capacity(files.len());
        for (name, bytes) in files {
            if name.is_empty() || name.contains('\n') || name.contains('\t') || name.contains('/') {
                return Err(CarryCtxError::git_error(format!(
                    "Invalid snapshot file name '{name}'; names must be flat and non-empty."
                )));
            }
            entries.push((name.clone(), self.hash_object(repo_root, bytes)?));
        }
        entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        let mut input = String::new();
        for (name, blob) in &entries {
            input.push_str(&format!("100644 blob {blob}\t{name}\n"));
        }
        let out = self.run_plumbing_stdin(repo_root, ["mktree"], input.as_bytes())?;
        Ok(String::from_utf8_lossy(&out).trim().to_string())
    }

    /// Create a commit object with `parents` and `message`, returning its sha.
    pub fn commit_tree(
        &self,
        repo_root: &Path,
        tree_sha: &str,
        parents: &[String],
        message: &str,
    ) -> Result<String, CarryCtxError> {
        let mut args: Vec<String> = vec!["commit-tree".to_string(), tree_sha.to_string()];
        for parent in parents {
            args.push("-p".to_string());
            args.push(parent.clone());
        }
        let out = self.run_plumbing_stdin(repo_root, &args, message.as_bytes())?;
        Ok(String::from_utf8_lossy(&out).trim().to_string())
    }

    /// Compare-and-swap `ref_name` to `new_commit`, or create it when `old` is
    /// `None`. Uses `update-ref --stdin` so the create/update guard is
    /// independent of the repository object format.
    pub fn update_ref_cas(
        &self,
        repo_root: &Path,
        ref_name: &str,
        new_commit: &str,
        old: Option<&str>,
    ) -> Result<(), CarryCtxError> {
        let command = match old {
            Some(old) => format!("update {ref_name} {new_commit} {old}\n"),
            None => format!("create {ref_name} {new_commit}\n"),
        };
        match self.run_plumbing_stdin(repo_root, ["update-ref", "--stdin"], command.as_bytes()) {
            Ok(_) => Ok(()),
            Err(error) => Err(CarryCtxError::git_error(format!(
                "Snapshot ref compare-and-swap failed for '{ref_name}': {}",
                error.message
            ))
            .with_suggestions([
                "Another worktree updated the ref concurrently; re-read the ref and retry."
                    .to_string(),
            ])),
        }
    }
}

impl Default for GitBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl VcsBackend for GitBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Git
    }

    fn repository_root(&self, start_path: &Path) -> Result<PathBuf, CarryCtxError> {
        Ok(self.discover(start_path)?.repository_root)
    }

    fn git_common_dir(&self, start_path: &Path) -> Result<PathBuf, CarryCtxError> {
        Ok(self.discover(start_path)?.git_common_dir)
    }

    fn head(&self, cwd: &Path) -> Result<Option<String>, CarryCtxError> {
        Ok(self
            .capture_stdout(cwd, ["rev-parse", "HEAD"])
            .ok()
            .map(|h| h.trim().to_string()))
    }

    fn status(&self, cwd: &Path) -> Result<GitSnapshot, CarryCtxError> {
        self.get_snapshot(cwd)
    }

    fn create_workspace(
        &self,
        repo_root: &Path,
        request: WorkspaceRequest,
    ) -> Result<Workspace, CarryCtxError> {
        self.create_worktree(
            repo_root,
            &request.path,
            &request.branch,
            request.base.as_deref(),
        )?;
        let head = self.head(&request.path).ok().flatten();
        Ok(Workspace {
            path: request.path,
            branch: Some(request.branch),
            head,
        })
    }

    fn remove_workspace(
        &self,
        repo_root: &Path,
        path: &Path,
        force: bool,
    ) -> Result<(), CarryCtxError> {
        self.remove_worktree(repo_root, path, force)
    }

    fn capabilities(&self) -> VcsCapabilities {
        VcsCapabilities::git()
    }

    fn create_snapshot_commit(
        &self,
        repo_root: &Path,
        ref_name: &str,
        files: &[(String, Vec<u8>)],
        export_id: &str,
        parents: &[String],
        source_label: &str,
        subject_label: &str,
    ) -> Result<SnapshotCommit, CarryCtxError> {
        // Read the current tip once, then hold it as the compare-and-swap
        // guard. The caller-supplied first parent must be that tip; otherwise
        // another worktree moved the ref between the caller's read and now.
        let current = self.resolve_ref(repo_root, ref_name)?;
        match parents.first() {
            Some(expected) if current.as_deref() != Some(expected.as_str()) => {
                return Err(CarryCtxError::git_error(format!(
                    "Snapshot ref '{ref_name}' moved concurrently: expected tip {expected}, found {}.",
                    current.as_deref().unwrap_or("<none>")
                ))
                .with_suggestions([
                    "Another worktree updated the snapshot ref; re-read it and retry.".to_string(),
                ]));
            }
            None if current.is_some() => {
                return Err(CarryCtxError::git_error(format!(
                    "Snapshot ref '{ref_name}' already exists at {}; refusing to create a second root snapshot.",
                    current.as_deref().unwrap_or("<none>")
                ))
                .with_suggestions([
                    "Re-run the export with the existing ref tip as its parent.".to_string(),
                ]));
            }
            _ => {}
        }

        let mut parent_export_ids = Vec::with_capacity(parents.len());
        for parent in parents {
            let message = self.commit_message(repo_root, parent)?;
            if let Some(id) = SnapshotTrailers::parse(&message).export_id {
                parent_export_ids.push(id);
            }
        }

        let tree = self.write_tree(repo_root, files)?;
        // Design §3.1 subject: `chore(ctxpack): snapshot <id> (<branch> @ <sha>)`.
        // The repo/branch source goes in the `CarryCtx-Source` trailer only.
        let subject = format!("chore(ctxpack): snapshot {export_id} ({subject_label})");
        let trailers = SnapshotTrailers {
            export_id: Some(export_id.to_string()),
            parents: parent_export_ids.clone(),
            source: Some(source_label.to_string()),
        };
        let message = render_snapshot_message(&subject, &trailers);
        let commit = self.commit_tree(repo_root, &tree, parents, &message)?;
        self.update_ref_cas(repo_root, ref_name, &commit, current.as_deref())?;
        Ok(SnapshotCommit {
            commit,
            previous: current,
            parent_export_ids,
        })
    }

    fn read_snapshot_manifest(
        &self,
        repo_root: &Path,
        ref_name: &str,
    ) -> Result<(Vec<u8>, Option<String>), CarryCtxError> {
        let Some(commit) = self.resolve_ref(repo_root, ref_name)? else {
            return Ok((Vec::new(), None));
        };
        match self.read_file_at(repo_root, &commit, SNAPSHOT_MANIFEST_FILE)? {
            Some(bytes) => Ok((bytes, Some(commit))),
            None => Err(CarryCtxError::git_error(format!(
                "Snapshot ref '{ref_name}' has no {SNAPSHOT_MANIFEST_FILE} at its tip."
            ))),
        }
    }

    fn read_snapshot_file(
        &self,
        repo_root: &Path,
        revision: &str,
        file: &str,
    ) -> Result<Option<Vec<u8>>, CarryCtxError> {
        self.read_file_at(repo_root, revision, file)
    }

    fn snapshot_history(
        &self,
        repo_root: &Path,
        ref_name: &str,
    ) -> Result<Vec<SnapshotRefCommit>, CarryCtxError> {
        let mut history = Vec::new();
        for commit in self.snapshot_history_commits(repo_root, ref_name)? {
            let message = self.commit_message(repo_root, &commit)?;
            let trailers = SnapshotTrailers::parse(&message);
            if let Some(export_id) = trailers.export_id {
                history.push(SnapshotRefCommit {
                    commit,
                    export_id,
                    parents: trailers.parents,
                    source: trailers.source,
                });
            }
        }
        Ok(history)
    }

    fn revision_exists(&self, repo_root: &Path, revision: &str) -> Result<bool, CarryCtxError> {
        self.commit_exists(repo_root, revision)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct WorktreeEntry {
    pub path: String,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub detached: bool,
    /// Present when `git worktree list --porcelain` emits a `locked` line.
    /// `Some("")` means locked without a reason, `Some("...")` carries the
    /// reason passed to `git worktree lock --reason`.
    pub locked: Option<String>,
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
