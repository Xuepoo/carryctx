use std::path::{Path, PathBuf};
use std::process::Command;

use crate::adapter::filesystem::{self, JournalEntry};
use crate::adapter::git::GitCli;
use crate::error::CarryCtxError;
use crate::repository::{
    EventRepository, NewEvent, NewWorktree, TaskRepository, WorktreeRecord, WorktreeRepository,
};

pub struct BindWorktreeInput {
    pub project_id: String,
    pub path: String,
    pub task_id: Option<String>,
}

pub fn bind_worktree(
    worktree_repo: &dyn WorktreeRepository,
    task_repo: &dyn TaskRepository,
    event_repo: &dyn EventRepository,
    git_cli: &GitCli,
    input: &BindWorktreeInput,
    now: &str,
) -> Result<WorktreeRecord, CarryCtxError> {
    let path = Path::new(&input.path);
    let discovery = git_cli.discover(path)?;

    let mut task_id: Option<String> = None;
    if let Some(ref t) = input.task_id {
        let task = task_repo
            .find_by_display_id(&input.project_id, t)?
            .or_else(|| task_repo.find_by_id(&input.project_id, t).ok().flatten())
            .ok_or_else(|| CarryCtxError::resource_not_found(format!("Task '{}' not found", t)))?;

        let existing_bound = worktree_repo.find_by_task_id(&input.project_id, &task.id)?;
        if let Some(ref wt) = existing_bound {
            if wt.path != discovery.repository_root.to_string_lossy() {
                return Err(CarryCtxError::state_conflict(format!(
                    "Task '{}' is already bound to worktree '{}'",
                    task.display_id, wt.path
                )));
            }
        }

        task_id = Some(task.id);
    }

    let existing = worktree_repo.find_by_path(
        &input.project_id,
        &discovery.repository_root.to_string_lossy(),
    )?;
    let worktree_id = existing
        .as_ref()
        .map(|w| w.id.clone())
        .unwrap_or_else(|| ulid::Ulid::generate().to_string());

    let record = worktree_repo.upsert(
        &NewWorktree {
            id: worktree_id,
            project_id: input.project_id.clone(),
            path: discovery.repository_root.to_string_lossy().to_string(),
            branch: discovery.branch.clone(),
            head: discovery.head.clone(),
            task_id,
        },
        now,
    )?;

    event_repo.append(&NewEvent {
        id: ulid::Ulid::generate().to_string(),
        project_id: input.project_id.clone(),
        event_type: "worktree.bound".into(),
        actor_agent_id: None,
        session_id: None,
        task_id: record.task_id.clone(),
        payload: serde_json::json!({
            "worktree_id": record.id,
            "path": record.path,
            "task_id": record.task_id,
            "branch": record.branch,
            "head": record.head,
        }),
        occurred_at: now.to_string(),
    })?;

    Ok(record)
}

pub fn unbind_worktree(
    worktree_repo: &dyn WorktreeRepository,
    event_repo: &dyn EventRepository,
    project_id: &str,
    path_or_id: &str,
    now: &str,
) -> Result<WorktreeRecord, CarryCtxError> {
    let worktree = worktree_repo
        .find_by_id(project_id, path_or_id)?
        .or_else(|| {
            worktree_repo
                .find_by_path(project_id, path_or_id)
                .ok()
                .flatten()
        })
        .ok_or_else(|| {
            CarryCtxError::resource_not_found(format!("Worktree '{}' not found", path_or_id))
        })?;

    let updated = worktree_repo.unbind_task(&worktree.id, project_id, now)?;

    event_repo.append(&NewEvent {
        id: ulid::Ulid::generate().to_string(),
        project_id: project_id.to_string(),
        event_type: "worktree.unbound".into(),
        actor_agent_id: None,
        session_id: None,
        task_id: None,
        payload: serde_json::json!({
            "worktree_id": updated.id,
            "path": updated.path,
        }),
        occurred_at: now.to_string(),
    })?;

    Ok(updated)
}

/// Input for [`remove_worktree`] (CTX-0083).
pub struct RemoveWorktreeInput {
    pub project_id: String,
    /// Repository root the removal's git operations run from.
    pub repository_root: String,
    /// Worktree reference: registration ULID, bound task display id
    /// (CTX-XXXX), or directory path (absolute or repository-relative).
    pub worktree_ref: String,
    /// Remove even when the worktree is dirty or has uncommitted changes.
    pub force: bool,
}

/// Result of a successful removal.
#[derive(serde::Serialize)]
pub struct RemovedWorktree {
    pub worktree: WorktreeRecord,
    /// True when a live Git worktree directory was removed; false when only
    /// an orphaned registration row was cleaned up.
    pub git_removed: bool,
}

/// Resolve a remove/bind-style reference to a registered worktree.
///
/// Accepts, in order: the registration ULID, exact stored paths (absolute or
/// repository-relative), and the bound task's display id (CTX-XXXX).
fn resolve_worktree_ref(
    worktree_repo: &dyn WorktreeRepository,
    task_repo: &dyn TaskRepository,
    project_id: &str,
    repository_root: &str,
    worktree_ref: &str,
) -> Result<WorktreeRecord, CarryCtxError> {
    let not_found =
        || CarryCtxError::resource_not_found(format!("Worktree '{worktree_ref}' not found"));

    // 1. Registration ULID.
    if let Some(record) = worktree_repo.find_by_id(project_id, worktree_ref)? {
        return Ok(record);
    }

    // 2. Paths: as-given (absolute), then joined with the repository root for
    // relative refs. Registered rows store canonical absolute paths.
    if let Some(record) = worktree_repo
        .find_by_path(project_id, worktree_ref)
        .ok()
        .flatten()
    {
        return Ok(record);
    }
    let joined = Path::new(repository_root).join(worktree_ref);
    if let Some(record) = worktree_repo
        .find_by_path(project_id, &joined.to_string_lossy())
        .ok()
        .flatten()
    {
        return Ok(record);
    }
    if let Ok(canon) = joined.canonicalize() {
        if let Some(record) = worktree_repo
            .find_by_path(project_id, &canon.to_string_lossy())
            .ok()
            .flatten()
        {
            return Ok(record);
        }
    }

    // 3. Bound task display id (CTX-XXXX) → task ULID → registration.
    if let Ok(Some(task)) = task_repo.find_by_display_id(project_id, worktree_ref) {
        if let Some(record) = worktree_repo.find_by_task_id(project_id, &task.id)? {
            return Ok(record);
        }
    }

    Err(not_found())
}

/// Remove a worktree by reference (CTX-0083).
///
/// - Live Git worktree: runs `git worktree remove` semantics — refuses when
///   the tree is dirty/uncommitted unless `force`, mirroring git's own guard.
/// - Directory already gone: orphan-cleanup path, deletes just the
///   registration row.
///
/// The registration row is always deleted on success (unlike
/// [`unbind_worktree`], which detaches without deleting), and a
/// `worktree.removed` audit event is appended in the same unit of work.
pub fn remove_worktree(
    worktree_repo: &dyn WorktreeRepository,
    task_repo: &dyn TaskRepository,
    event_repo: &dyn EventRepository,
    git_cli: &GitCli,
    input: &RemoveWorktreeInput,
    now: &str,
) -> Result<RemovedWorktree, CarryCtxError> {
    let record = resolve_worktree_ref(
        worktree_repo,
        task_repo,
        &input.project_id,
        &input.repository_root,
        &input.worktree_ref,
    )?;

    let absolute_path = Path::new(&record.path);
    let mut git_removed = false;
    if absolute_path.exists() {
        // Only run git removal for directories that are live worktrees of
        // this repository; foreign directories are just unregistered.
        let live = git_cli
            .list_worktrees(Path::new(&input.repository_root))
            .map(|entries| {
                entries
                    .iter()
                    .any(|entry| Path::new(&entry.path) == absolute_path)
            })
            .unwrap_or(false);
        if live {
            git_cli.remove_worktree(
                Path::new(&input.repository_root),
                absolute_path,
                input.force,
            )?;
            git_removed = true;
        }
    }

    worktree_repo.delete(&record.id, &input.project_id)?;

    event_repo.append(&NewEvent {
        id: ulid::Ulid::generate().to_string(),
        project_id: input.project_id.clone(),
        event_type: "worktree.removed".into(),
        actor_agent_id: None,
        session_id: None,
        task_id: record.task_id.clone(),
        payload: serde_json::json!({
            "worktree_id": record.id,
            "path": record.path,
            "git_removed": git_removed,
            "forced": input.force,
        }),
        occurred_at: now.to_string(),
    })?;

    Ok(RemovedWorktree {
        worktree: record,
        git_removed,
    })
}

pub struct CreateWorktreeInput {
    pub project_id: String,
    pub repository_root: String,
    pub path: String,
    pub branch: String,
    pub base: Option<String>,
    pub task_id: Option<String>,
}

pub fn create_worktree(
    worktree_repo: &dyn WorktreeRepository,
    task_repo: &dyn TaskRepository,
    event_repo: &dyn EventRepository,
    git_cli: &GitCli,
    xdg_paths: &crate::adapter::xdg::XdgPaths,
    input: &CreateWorktreeInput,
    now: &str,
) -> Result<WorktreeRecord, CarryCtxError> {
    let worktree_path = Path::new(&input.path);
    if worktree_path.exists() {
        return Err(CarryCtxError::invalid_arguments(format!(
            "Worktree path '{}' already exists",
            input.path
        )));
    }

    if crate::adapter::git::detect_jj_colocation(
        &git_cli
            .discover(Path::new(&input.repository_root))?
            .git_common_dir,
    ) {
        return Err(CarryCtxError::validation_error(
            "This repository is jj-colocated (.jj/ alongside .git/). `carryctx worktree create` \
             uses `git worktree add`, which jj does not recognize as a workspace, and jj's own \
             secondary workspaces (from `jj workspace add`) have no `.git/` directory for carryctx \
             to read state from. Create the workspace directly with `jj workspace add <path>`, cd \
             into it, then run `carryctx worktree bind <path>` once inside the *primary* colocated \
             checkout — carryctx state commands are not usable from inside a pure jj secondary \
             workspace. See carryctx-docs/plans/2026-07-25-jujutsu-compatibility.md.",
        ));
    }

    let branch_exists = git_cli.has_branch(Path::new(&input.repository_root), &input.branch)?;
    if branch_exists {
        return Err(CarryCtxError::state_conflict(format!(
            "Branch '{}' already exists",
            input.branch
        )));
    }

    let operation_id = ulid::Ulid::generate().to_string();
    let git_project = git_cli.discover(Path::new(&input.repository_root))?;
    let journal_dir = xdg_paths.journal_dir(&git_project.git_common_dir);

    let journal_entry = JournalEntry {
        operation_id: operation_id.clone(),
        kind: "worktree.create".into(),
        status: "running".into(),
        created_at: now.to_string(),
        metadata: serde_json::json!({
            "repositoryRoot": input.repository_root,
            "path": input.path,
            "branch": input.branch,
            "base": input.base,
        }),
    };
    filesystem::write_journal(&journal_dir, &journal_entry)?;

    let create_result = git_cli.create_worktree(
        Path::new(&input.repository_root),
        worktree_path,
        &input.branch,
        input.base.as_deref(),
    );

    if let Err(ref e) = create_result {
        let failed_entry = JournalEntry {
            operation_id,
            kind: "worktree.create".into(),
            status: "failed".into(),
            created_at: now.to_string(),
            metadata: serde_json::json!({
                "error": e.to_string(),
            }),
        };
        let _ = filesystem::write_journal(&journal_dir, &failed_entry);
        return Err(CarryCtxError::git_error(format!(
            "Failed to create worktree: {}",
            e
        )));
    }

    match bind_worktree(
        worktree_repo,
        task_repo,
        event_repo,
        git_cli,
        &BindWorktreeInput {
            project_id: input.project_id.clone(),
            path: input.path.clone(),
            task_id: input.task_id.clone(),
        },
        now,
    ) {
        Ok(record) => {
            // Success leaves nothing to reconcile.
            let _ = filesystem::remove_journal(&journal_dir, &operation_id);
            Ok(record)
        }
        Err(bind_error) => {
            // Roll back the `git worktree add` so a bind failure does not
            // strand an orphaned worktree directory and branch.
            match cleanup_worktree_and_branch(
                Path::new(&input.repository_root),
                worktree_path,
                Some(&input.branch),
                input.base.as_deref(),
            ) {
                Ok(()) => {
                    // Nothing dangling: drop the journal entirely.
                    let _ = filesystem::remove_journal(&journal_dir, &operation_id);
                }
                Err(rollback_error) => {
                    // Keep a failed journal so startup reconciliation can
                    // retry the removal on the next mutating command.
                    let failed_entry = JournalEntry {
                        operation_id: operation_id.clone(),
                        kind: "worktree.create".into(),
                        status: "failed".into(),
                        created_at: now.to_string(),
                        metadata: serde_json::json!({
                            "repositoryRoot": input.repository_root,
                            "path": input.path,
                            "branch": input.branch,
                            "base": input.base,
                            "bindError": bind_error.to_string(),
                            "rollbackError": rollback_error.to_string(),
                        }),
                    };
                    let _ = filesystem::write_journal(&journal_dir, &failed_entry);
                    eprintln!(
                        "carryctx: failed to roll back orphaned worktree '{}': {}; \
                         it will be retried on the next command",
                        input.path, rollback_error
                    );
                }
            }
            Err(bind_error)
        }
    }
}

/// Reconcile interrupted `worktree.create` journals on startup.
///
/// A crash between `git worktree add` and the bind step (or a bind failure
/// whose rollback also failed) used to strand an orphaned worktree
/// directory plus branch behind a permanent running/failed journal nobody
/// read. This consumer removes the orphaned worktree and — only when the
/// branch still points exactly at its creation commit — deletes the
/// branch, then retires the journal.
pub fn recover_worktree_create_journals(
    xdg_paths: &crate::adapter::xdg::XdgPaths,
    git_common_dir: &Path,
) -> Result<(), CarryCtxError> {
    let journal_dir = xdg_paths.journal_dir(git_common_dir);
    for entry in filesystem::list_journals(&journal_dir)? {
        if entry.kind != "worktree.create" {
            continue;
        }
        match entry.status.as_str() {
            "completed" => {
                // Leftover from an older version; nothing to reconcile.
                filesystem::remove_journal(&journal_dir, &entry.operation_id)?;
            }
            "running" | "failed" => {
                reconcile_worktree_create_entry(git_common_dir, &entry);
                filesystem::remove_journal(&journal_dir, &entry.operation_id)?;
            }
            _ => {
                // Unknown status: leave for manual inspection.
            }
        }
    }
    Ok(())
}

fn reconcile_worktree_create_entry(git_common_dir: &Path, entry: &JournalEntry) {
    let cwd = entry.metadata["repositoryRoot"]
        .as_str()
        .map(PathBuf::from)
        .unwrap_or_else(|| git_common_dir.to_path_buf());
    let Some(path) = entry.metadata["path"].as_str() else {
        return;
    };
    let branch = entry.metadata["branch"].as_str();
    let base = entry.metadata["base"].as_str();
    if let Err(error) = cleanup_worktree_and_branch(&cwd, Path::new(path), branch, base) {
        eprintln!(
            "carryctx: could not fully reconcile orphaned worktree '{}': {}",
            path, error
        );
    }
}

/// Remove `worktree_path` and its branch after an aborted create.
///
/// The removal is deliberately non-forced: a dirty worktree survives for
/// manual inspection instead of destroying uncommitted agent work. The
/// branch is deleted only when it still points at its creation anchor
/// (`base`, or `HEAD` when no explicit base was given), so any commits an
/// agent managed to make are never destroyed by cleanup.
fn cleanup_worktree_and_branch(
    repo_root: &Path,
    worktree_path: &Path,
    branch: Option<&str>,
    base: Option<&str>,
) -> Result<(), CarryCtxError> {
    let path_str = worktree_path.to_string_lossy().into_owned();
    // Ignore remove failures: prune below still cleans registrations whose
    // directory is already gone.
    let _ = git_run(repo_root, &["worktree", "remove", &path_str]);
    git_run(repo_root, &["worktree", "prune"])?;

    if let Some(branch) = branch {
        match (
            rev_parse(repo_root, &format!("refs/heads/{branch}")),
            resolve_anchor(repo_root, base),
        ) {
            (Some(tip), Some(anchor)) if tip == anchor => {
                git_run(repo_root, &["branch", "-D", branch])?;
            }
            _ => eprintln!(
                "carryctx: preserving branch '{branch}': it no longer points at its creation commit"
            ),
        }
    }
    Ok(())
}

fn resolve_anchor(repo_root: &Path, base: Option<&str>) -> Option<String> {
    rev_parse(repo_root, base.unwrap_or("HEAD"))
}

fn rev_parse(repo_root: &Path, revision: &str) -> Option<String> {
    git_capture(repo_root, &["rev-parse", "--verify", "--quiet", revision])
        .ok()
        .map(|out| out.trim().to_string())
        .filter(|out| !out.is_empty())
}

fn git_capture(repo_root: &Path, args: &[&str]) -> Result<String, CarryCtxError> {
    let mut command = Command::new("git");
    crate::adapter::git::isolate_git_env(&mut command);
    let output = command
        .args(args)
        .current_dir(repo_root)
        .output()
        .map_err(|e| CarryCtxError::git_error(format!("Failed to run git: {e}")))?;
    if !output.status.success() {
        return Err(CarryCtxError::git_error(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn git_run(repo_root: &Path, args: &[&str]) -> Result<(), CarryCtxError> {
    git_capture(repo_root, args).map(|_| ())
}

pub fn list_worktrees(
    worktree_repo: &dyn WorktreeRepository,
    git_cli: &GitCli,
    project_id: &str,
    repository_root: Option<&str>,
) -> Result<Vec<WorktreeRecord>, CarryCtxError> {
    let mut records = worktree_repo.list(project_id)?;

    if let Some(root) = repository_root {
        if let Ok(git_trees) = git_cli.list_worktrees(Path::new(root)) {
            let db_paths: std::collections::HashSet<String> =
                records.iter().map(|w| w.path.clone()).collect();

            for gt in &git_trees {
                if !gt.detached && !db_paths.contains(&gt.path) {
                    // Skip the main repository root — it's always reported by git
                    // but is not a worktree that needs separate registration.
                    if let Some(root) = repository_root {
                        if gt.path.trim_end_matches('/') == root.trim_end_matches('/') {
                            continue;
                        }
                    }
                    records.push(WorktreeRecord {
                        id: String::new(),
                        project_id: project_id.to_string(),
                        path: gt.path.clone(),
                        branch: gt.branch.clone(),
                        head: gt.head.clone(),
                        task_id: None,
                        created_at: String::new(),
                        updated_at: String::new(),
                    });
                }
            }
        }
    }

    Ok(records)
}

pub fn show_worktree(
    worktree_repo: &dyn WorktreeRepository,
    git_cli: &GitCli,
    project_id: &str,
    path_or_id: &str,
) -> Result<WorktreeRecord, CarryCtxError> {
    let mut record = worktree_repo
        .find_by_id(project_id, path_or_id)?
        .or_else(|| {
            worktree_repo
                .find_by_path(project_id, path_or_id)
                .ok()
                .flatten()
        })
        .ok_or_else(|| {
            CarryCtxError::resource_not_found(format!("Worktree '{}' not found", path_or_id))
        })?;

    if let Ok(snapshot) = git_cli.get_snapshot(Path::new(&record.path)) {
        record.branch = snapshot.branch;
        record.head = snapshot.head;
    }

    Ok(record)
}

pub fn stale_worktrees(
    worktree_repo: &dyn WorktreeRepository,
    project_id: &str,
    repository_root: &Path,
) -> Result<Vec<WorktreeRecord>, CarryCtxError> {
    Ok(worktree_repo
        .list(project_id)?
        .into_iter()
        .filter(|worktree| {
            let path = Path::new(&worktree.path);
            if path.is_absolute() {
                !path.exists()
            } else {
                !repository_root.join(path).exists()
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::xdg::XdgPaths;

    /// Disposable git repository with one initial commit.
    struct TestRepo {
        _dir: tempfile::TempDir,
        root: PathBuf,
    }

    /// Run a fixture git command with inherited GIT_* state stripped and a
    /// hard success assertion. Hook runners (lefthook) execute tests with
    /// GIT_DIR/GIT_INDEX_FILE pointing at the repository under test; without
    /// scrubbing, fixture commands resolve into that repo instead of the
    /// temp dir — CTX-0082: an unscrubbed fixture "init" commit once landed
    /// on the feature branch and replaced the whole tree.
    fn git_fixture(repo_root: &Path, args: &[&str]) {
        const GIT_STATE_VARS: &[&str] = &[
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_COMMON_DIR",
            "GIT_NAMESPACE",
            "GIT_CEILING_DIRECTORIES",
            "GIT_AUTHOR_NAME",
            "GIT_AUTHOR_EMAIL",
            "GIT_AUTHOR_DATE",
            "GIT_COMMITTER_NAME",
            "GIT_COMMITTER_EMAIL",
            "GIT_COMMITTER_DATE",
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_SYSTEM",
        ];
        let mut command = Command::new("git");
        command.args(args).current_dir(repo_root);
        for var in GIT_STATE_VARS {
            command.env_remove(var);
        }
        let output = command
            .output()
            .unwrap_or_else(|e| panic!("fixture git {args:?} failed to spawn: {e}"));
        assert!(
            output.status.success(),
            "fixture git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_repo() -> TestRepo {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        git_fixture(&root, &["init", "-b", "main", "."]);
        git_fixture(&root, &["config", "user.email", "test@example.com"]);
        git_fixture(&root, &["config", "user.name", "Test"]);
        std::fs::write(root.join("README.md"), "# test\n").expect("write readme");
        git_fixture(&root, &["add", "."]);
        git_fixture(&root, &["commit", "-m", "init"]);
        TestRepo { _dir: dir, root }
    }

    fn commit_file(cwd: &Path, name: &str) {
        std::fs::write(cwd.join(name), "content\n").expect("write file");
        git_fixture(cwd, &["add", "."]);
        git_fixture(cwd, &["commit", "-m", name]);
    }

    #[test]
    fn cleanup_removes_freshly_created_worktree_and_branch() {
        let repo = init_repo();
        let git_cli = GitCli::new();
        let wt = repo.root.join("wt-cleanup");
        git_cli
            .create_worktree(&repo.root, &wt, "feature/cleanup", None)
            .expect("worktree add");
        assert!(wt.exists());
        assert!(git_cli.has_branch(&repo.root, "feature/cleanup").unwrap());

        cleanup_worktree_and_branch(&repo.root, &wt, Some("feature/cleanup"), None)
            .expect("cleanup");

        assert!(!wt.exists(), "orphaned worktree directory must be removed");
        assert!(
            !git_cli.has_branch(&repo.root, "feature/cleanup").unwrap(),
            "unmoved creation branch must be deleted"
        );
    }

    #[test]
    fn cleanup_preserves_diverged_branch_but_still_removes_worktree() {
        let repo = init_repo();
        let git_cli = GitCli::new();
        let wt = repo.root.join("wt-diverged");
        git_cli
            .create_worktree(&repo.root, &wt, "feature/diverged", None)
            .expect("worktree add");
        // An agent managed to commit before the crash: the branch moved off
        // its creation anchor.
        commit_file(&wt, "work.txt");

        cleanup_worktree_and_branch(&repo.root, &wt, Some("feature/diverged"), None)
            .expect("cleanup");

        assert!(!wt.exists());
        assert!(
            git_cli.has_branch(&repo.root, "feature/diverged").unwrap(),
            "a diverged branch must be preserved, never destroyed by cleanup"
        );
    }

    #[test]
    fn recover_removes_orphaned_running_journal_state() {
        let repo = init_repo();
        let git_cli = GitCli::new();
        let xdg = XdgPaths::default();
        let common_dir = repo.root.join(".git");

        // Simulate a crash between `git worktree add` and bind: a fresh
        // worktree plus a running journal nobody has consumed yet.
        let wt = repo.root.join("wt-orphan");
        git_cli
            .create_worktree(&repo.root, &wt, "feature/orphan", None)
            .expect("worktree add");
        let journal_dir = xdg.journal_dir(&common_dir);
        filesystem::write_journal(
            &journal_dir,
            &JournalEntry {
                operation_id: ulid::Ulid::generate().to_string(),
                kind: "worktree.create".into(),
                status: "running".into(),
                created_at: "now".into(),
                metadata: serde_json::json!({
                    "repositoryRoot": repo.root.to_string_lossy(),
                    "path": wt.to_string_lossy(),
                    "branch": "feature/orphan",
                    "base": serde_json::Value::Null,
                }),
            },
        )
        .expect("write journal");

        recover_worktree_create_journals(&xdg, &common_dir).expect("recover");

        assert!(!wt.exists(), "orphaned worktree must be removed on startup");
        assert!(
            !git_cli.has_branch(&repo.root, "feature/orphan").unwrap(),
            "orphaned unmoved branch must be removed"
        );
        assert!(
            filesystem::list_journals(&journal_dir).unwrap().is_empty(),
            "reconciled journal must be retired"
        );
    }

    #[test]
    fn recover_leaves_unknown_status_journals_for_manual_inspection() {
        let repo = init_repo();
        let xdg = XdgPaths::default();
        let common_dir = repo.root.join(".git");
        let journal_dir = xdg.journal_dir(&common_dir);
        filesystem::write_journal(
            &journal_dir,
            &JournalEntry {
                operation_id: ulid::Ulid::generate().to_string(),
                kind: "worktree.create".into(),
                status: "mystery".into(),
                created_at: "now".into(),
                metadata: serde_json::json!({}),
            },
        )
        .expect("write journal");

        recover_worktree_create_journals(&xdg, &common_dir).expect("recover");

        assert_eq!(filesystem::list_journals(&journal_dir).unwrap().len(), 1);
    }
}
