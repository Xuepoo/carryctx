use std::path::Path;

use crate::adapter::git::GitCli;
use crate::domain::checkpoint::{Checkpoint, CheckpointCorrection};
use crate::domain::git_snapshot::GitSnapshot;
use crate::error::CarryCtxError;
use crate::repository::{CheckpointRepository, EventRepository, NewEvent};

pub struct CreateCheckpointInput {
    pub project_id: String,
    pub task_id: String,
    pub session_id: Option<String>,
    pub agent_id: Option<String>,
    pub worktree_id: Option<String>,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub done: Vec<String>,
    pub remaining: Vec<String>,
    pub blockers: Vec<String>,
    pub risks: Vec<String>,
    pub next_actions: Vec<String>,
    pub notes: Vec<String>,
    pub repo_path: Option<String>,
}

fn build_git_snapshot(
    git_cli: &GitCli,
    repo_path: Option<&str>,
) -> Result<Option<GitSnapshot>, CarryCtxError> {
    match repo_path {
        Some(path) => {
            let snapshot = git_cli.get_snapshot(Path::new(path))?;
            Ok(Some(snapshot))
        }
        None => Ok(None),
    }
}

use crate::domain::graph::{GraphEdge, GraphNode};

pub fn create_checkpoint(
    checkpoint_repo: &dyn CheckpointRepository,
    event_repo: &dyn EventRepository,
    graph_repo: Option<&crate::repository::graph::GraphRepository>,
    git_cli: &GitCli,
    input: &CreateCheckpointInput,
    now: &str,
) -> Result<Checkpoint, CarryCtxError> {
    let git_snapshot = build_git_snapshot(git_cli, input.repo_path.as_deref())?;

    let (
        head,
        branch,
        dirty,
        staged,
        modified,
        deleted,
        renamed,
        untracked,
        diff_files,
        diff_insertions,
        diff_deletions,
    ) = match &git_snapshot {
        Some(s) => (
            Some(s.head.clone()),
            s.branch.clone(),
            s.dirty,
            s.staged.clone(),
            s.modified.clone(),
            s.deleted.clone(),
            s.renamed
                .iter()
                .map(|r| crate::domain::checkpoint::RenamedFile {
                    from: r.from.clone(),
                    to: r.to.clone(),
                })
                .collect(),
            s.untracked.clone(),
            s.diff_stats.as_ref().map(|d| d.files),
            s.diff_stats.as_ref().map(|d| d.insertions),
            s.diff_stats.as_ref().map(|d| d.deletions),
        ),
        None => (
            input.head.clone(),
            input.branch.clone(),
            false,
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            None,
            None,
            None,
        ),
    };

    let cp = Checkpoint {
        id: ulid::Ulid::generate().to_string(),
        project_id: input.project_id.clone(),
        task_id: input.task_id.clone(),
        session_id: input.session_id.clone(),
        agent_id: input.agent_id.clone(),
        worktree_id: input.worktree_id.clone(),
        branch,
        head,
        dirty,
        staged_files: staged,
        modified_files: modified.clone(),
        deleted_files: deleted,
        renamed_files: renamed,
        untracked_files: untracked,
        diff_files,
        diff_insertions,
        diff_deletions,
        done: input.done.clone(),
        remaining: input.remaining.clone(),
        blockers: input.blockers.clone(),
        risks: input.risks.clone(),
        next_actions: input.next_actions.clone(),
        notes: input.notes.clone(),
        created_at: now.to_string(),
    };

    let saved = checkpoint_repo.create(&cp)?;

    event_repo.append(&NewEvent {
        id: ulid::Ulid::generate().to_string(),
        project_id: input.project_id.clone(),
        event_type: "checkpoint.created".into(),
        actor_agent_id: input.agent_id.clone(),
        session_id: input.session_id.clone(),
        task_id: Some(input.task_id.clone()),
        payload: serde_json::json!({
            "checkpoint_id": saved.id,
            "task_id": saved.task_id,
            "dirty": saved.dirty,
            "done_count": saved.done.len(),
            "remaining_count": saved.remaining.len(),
        }),
        occurred_at: now.to_string(),
    })?;

    if let Some(g_repo) = graph_repo {
        let t_id = &input.task_id;
        for file in modified {
            let file_id = match g_repo.get_node_by_name_and_type(&file, "file")? {
                Some(node) => node.id,
                None => {
                    let new_id = ulid::Ulid::generate().to_string();
                    let node = GraphNode {
                        id: new_id.clone(),
                        node_type: "file".into(),
                        name: file.clone(),
                        description: None,
                        metadata: serde_json::json!({"auto_generated_by": "checkpoint"}),
                        created_at: now.to_string(),
                        updated_at: now.to_string(),
                    };
                    g_repo.insert_node(&node)?;
                    new_id
                }
            };

            if g_repo.get_edge(t_id, &file_id, "changed")?.is_none() {
                g_repo.insert_edge(&GraphEdge {
                    source_id: t_id.clone(),
                    target_id: file_id,
                    relation_type: "changed".into(),
                    created_at: now.to_string(),
                    created_by: input.agent_id.clone(),
                    metadata: serde_json::json!({"checkpoint_id": saved.id}),
                })?;
            }
        }
    }

    Ok(saved)
}

pub fn list_checkpoints(
    checkpoint_repo: &dyn CheckpointRepository,
    project_id: &str,
    task_id: Option<&str>,
) -> Result<Vec<Checkpoint>, CarryCtxError> {
    checkpoint_repo.list(project_id, task_id)
}

pub fn show_checkpoint(
    checkpoint_repo: &dyn CheckpointRepository,
    project_id: &str,
    checkpoint_id: &str,
) -> Result<Checkpoint, CarryCtxError> {
    checkpoint_repo
        .find_by_id(project_id, checkpoint_id)?
        .ok_or_else(|| {
            CarryCtxError::resource_not_found(format!("Checkpoint '{}' not found", checkpoint_id))
        })
}

pub struct CorrectCheckpointInput {
    pub project_id: String,
    pub checkpoint_id: String,
    pub done: Option<Vec<String>>,
    pub remaining: Option<Vec<String>>,
    pub blockers: Option<Vec<String>>,
    pub risks: Option<Vec<String>>,
    pub next_actions: Option<Vec<String>>,
    pub notes: Option<Vec<String>>,
}

pub fn correct_checkpoint(
    checkpoint_repo: &dyn CheckpointRepository,
    event_repo: &dyn EventRepository,
    input: &CorrectCheckpointInput,
    now: &str,
) -> Result<(), CarryCtxError> {
    let existing = checkpoint_repo
        .find_by_id(&input.project_id, &input.checkpoint_id)?
        .ok_or_else(|| {
            CarryCtxError::resource_not_found(format!(
                "Checkpoint '{}' not found",
                input.checkpoint_id
            ))
        })?;

    let correction = CheckpointCorrection {
        id: ulid::Ulid::generate().to_string(),
        checkpoint_id: input.checkpoint_id.clone(),
        done: input.done.clone(),
        remaining: input.remaining.clone(),
        blockers: input.blockers.clone(),
        risks: input.risks.clone(),
        next_actions: input.next_actions.clone(),
        notes: input.notes.clone(),
        created_at: now.to_string(),
    };

    checkpoint_repo.correct(&correction)?;

    event_repo.append(&NewEvent {
        id: ulid::Ulid::generate().to_string(),
        project_id: input.project_id.clone(),
        event_type: "checkpoint.corrected".into(),
        actor_agent_id: None,
        session_id: None,
        task_id: Some(existing.task_id.clone()),
        payload: serde_json::json!({
            "checkpoint_id": input.checkpoint_id,
            "correction_id": correction.id,
            "fields": {
                "done": input.done.is_some(),
                "remaining": input.remaining.is_some(),
                "blockers": input.blockers.is_some(),
                "risks": input.risks.is_some(),
                "next_actions": input.next_actions.is_some(),
                "notes": input.notes.is_some(),
            },
        }),
        occurred_at: now.to_string(),
    })?;

    Ok(())
}
