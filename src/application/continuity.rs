use std::collections::HashMap;
use std::path::Path;

use crate::domain::agent::{Agent, AgentStatus};
use crate::domain::dependency::DependencyKind;
use crate::domain::git_snapshot::GitSnapshot;
use crate::domain::progress::ProgressType;
use crate::domain::session::SessionState;
use crate::domain::task::TaskStatus;
use crate::error::CarryCtxError;
use crate::repository::agent::{AgentFilter, AgentRepository};
use crate::repository::checkpoint::CheckpointRepository;
use crate::repository::collaboration::DecisionRepository;
use crate::repository::dependency::DependencyRepository;
use crate::repository::event::{EventFilter, EventRepository};
use crate::repository::progress::{ProgressFilter, ProgressRepository};
use crate::repository::session::SessionRepository;
use crate::repository::task::{TaskFilter, TaskRecord, TaskRepository};
use crate::repository::worktree::WorktreeRepository;

// ---------------------------------------------------------------------------
// Resume
// ---------------------------------------------------------------------------

pub struct ResumeInput {
    pub project_id: String,
    pub task_id: Option<String>,
    pub session_id: Option<String>,
    pub resolve_agent: bool,
    pub start_session: bool,
    pub include_diff: bool,
    pub max_events: u64,
}

pub struct ResumeResult {
    pub project_id: String,
    pub agent: Option<String>,
    pub session: Option<String>,
    pub task: String,
    pub git: GitSnapshot,
    pub checkpoint: Option<String>,
    pub progress: ProgressSummary,
    pub warnings: Vec<String>,
    pub next_actions: Vec<String>,
}

pub struct ProgressSummary {
    pub completed: Vec<String>,
    pub remaining: Vec<String>,
    pub blockers: Vec<String>,
    pub risks: Vec<String>,
}

// ---------------------------------------------------------------------------
// Context
// ---------------------------------------------------------------------------

pub struct ContextInput {
    pub project_id: String,
    pub task_id: Option<String>,
    pub compact: bool,
    pub include_decisions: bool,
    pub include_events: bool,
    pub include_related_tasks: bool,
    pub max_events: u64,
    pub since: Option<String>,
}

pub struct ContextSection {
    pub kind: String,
    pub rank: u32,
    pub title: String,
    pub content: serde_json::Value,
}

pub struct ContextResult {
    pub sections: Vec<ContextSection>,
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

pub struct StatusInput {
    pub project_id: String,
    pub mine: bool,
    pub all: bool,
    pub compact: bool,
    pub show_sessions: bool,
    pub show_tasks: bool,
    pub show_worktrees: bool,
    pub since: Option<String>,
}

pub struct StatusResult {
    pub project_id: String,
    pub current_agent: Option<String>,
    pub counts: HashMap<String, u64>,
    pub sessions: Vec<String>,
    pub tasks: Vec<String>,
    pub worktrees: Vec<String>,
    pub activity: Vec<String>,
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn resolve_task(
    project_id: &str,
    ref_: &str,
    task_repo: &dyn TaskRepository,
) -> Result<TaskRecord, CarryCtxError> {
    if let Some(task) = task_repo.find_by_display_id(project_id, ref_)? {
        return Ok(task);
    }
    if let Some(task) = task_repo.find_by_id(project_id, ref_)? {
        return Ok(task);
    }
    Err(CarryCtxError::resource_not_found(format!(
        "Task '{ref_}' not found."
    )))
}

fn load_progress_summary(
    project_id: &str,
    task_id: &str,
    progress_repo: &dyn ProgressRepository,
) -> Result<ProgressSummary, CarryCtxError> {
    let filter = ProgressFilter {
        project_id: project_id.to_string(),
        task_id: task_id.to_string(),
        include_removed: false,
    };
    let items = progress_repo.list(&filter)?;

    let mut done = Vec::new();
    let mut remaining = Vec::new();
    let mut blockers = Vec::new();
    let mut risks = Vec::new();

    for item in &items {
        use crate::domain::progress::ProgressStatus;
        match item.status {
            ProgressStatus::Completed => done.push(item.content.clone()),
            ProgressStatus::Open => match item.item_type {
                ProgressType::Todo => remaining.push(item.content.clone()),
                ProgressType::Blocker => blockers.push(item.content.clone()),
                ProgressType::Risk => risks.push(item.content.clone()),
                ProgressType::Note => {}
            },
            ProgressStatus::Removed => {}
        }
    }

    Ok(ProgressSummary {
        completed: done,
        remaining,
        blockers,
        risks,
    })
}

fn detect_stale_checkpoint(
    checkpoint: &crate::domain::checkpoint::Checkpoint,
    current_head: &str,
) -> Option<String> {
    if let Some(ref cp_head) = checkpoint.head {
        if cp_head != current_head {
            return Some(format!(
                "Checkpoint HEAD {} differs from current HEAD {}",
                cp_head, current_head
            ));
        }
    }
    None
}

fn try_resolve_single_agent(
    project_id: &str,
    agent_repo: &dyn AgentRepository,
) -> Result<Option<Agent>, CarryCtxError> {
    let active = agent_repo.list(&AgentFilter {
        project_id: project_id.to_string(),
        status: Some(AgentStatus::Active),
    })?;
    if active.len() == 1 {
        Ok(Some(active.into_iter().next().unwrap()))
    } else {
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// resume_work
// ---------------------------------------------------------------------------

pub fn resume_work(
    input: &ResumeInput,
    task_repo: &dyn TaskRepository,
    agent_repo: &dyn AgentRepository,
    session_repo: &dyn SessionRepository,
    checkpoint_repo: &dyn CheckpointRepository,
    progress_repo: &dyn ProgressRepository,
    dependency_repo: &dyn DependencyRepository,
    _worktree_repo: &dyn WorktreeRepository,
    _event_repo: &dyn EventRepository,
    git_cli: &crate::adapter::git::GitCli,
    repo_path: Option<&Path>,
) -> Result<ResumeResult, CarryCtxError> {
    let mut warnings: Vec<String> = Vec::new();

    // Resolve task
    let task_ref = input.task_id.as_deref().unwrap_or("current");
    let task = resolve_task(&input.project_id, task_ref, task_repo)?;

    // Resolve agent
    let resolved_agent: Option<String> = if input.resolve_agent {
        match try_resolve_single_agent(&input.project_id, agent_repo)? {
            Some(a) => Some(a.name),
            None => {
                warnings.push("Could not resolve a single active agent; use --agent.".into());
                None
            }
        }
    } else {
        None
    };

    // Resolve session
    let _resolved_session: Option<String> = if let Some(ref sid) = input.session_id {
        let session = session_repo
            .find_by_id(&input.project_id, sid)?
            .ok_or_else(|| {
                CarryCtxError::resource_not_found(format!("Session '{sid}' not found."))
            })?;
        Some(session.id)
    } else {
        None
    };

    // Git snapshot
    let git = match repo_path {
        Some(path) => git_cli.get_snapshot(path)?,
        None => GitSnapshot {
            branch: None,
            head: String::new(),
            dirty: false,
            staged: Vec::new(),
            modified: Vec::new(),
            deleted: Vec::new(),
            renamed: Vec::new(),
            untracked: Vec::new(),
            diff_stats: None,
        },
    };

    // Latest checkpoint
    let latest_cp = checkpoint_repo.find_latest_for_task(&input.project_id, &task.id)?;
    let checkpoint_id = latest_cp.as_ref().map(|cp| cp.id.clone());

    // Detect stale checkpoint
    if let Some(ref cp) = latest_cp {
        if !git.head.is_empty() {
            if let Some(msg) = detect_stale_checkpoint(cp, &git.head) {
                warnings.push(msg);
            }
        }
    }

    // Progress summary
    let progress = load_progress_summary(&input.project_id, &task.id, progress_repo)?;

    // Dependencies
    let deps = dependency_repo.list_for_task(&input.project_id, &task.id)?;
    let incomplete_strong: Vec<_> = deps
        .iter()
        .filter(|d| d.kind == DependencyKind::Strong)
        .collect();

    // ---- Next actions ----
    let mut next_actions: Vec<String> = Vec::new();

    if !progress.blockers.is_empty() {
        next_actions.push(format!("Resolve {} blocker(s)", progress.blockers.len()));
    }

    if !incomplete_strong.is_empty() {
        next_actions.push(format!(
            "Wait for {} incomplete strong dependency/dependencies",
            incomplete_strong.len()
        ));
    }

    if !progress.remaining.is_empty() {
        next_actions.push(format!(
            "Continue working on {} remaining item(s)",
            progress.remaining.len()
        ));
    }

    if git.dirty {
        next_actions.push("Create a checkpoint to capture current progress".into());
    }

    if checkpoint_id.is_none() {
        next_actions.push("Create initial checkpoint".into());
    }

    if progress.completed.is_empty() && progress.remaining.is_empty() {
        next_actions.push("Define initial progress items (todos)".into());
    }

    if progress.remaining.is_empty()
        && !progress.blockers.is_empty()
        && task.status != TaskStatus::Completed
        && task.status != TaskStatus::Review
    {
        next_actions.push("All work appears complete — mark task for review".into());
    }

    next_actions.push("Review project context and plan next steps".into());

    Ok(ResumeResult {
        project_id: input.project_id.clone(),
        agent: resolved_agent,
        session: _resolved_session,
        task: task.display_id,
        git,
        checkpoint: checkpoint_id,
        progress,
        warnings,
        next_actions,
    })
}

// ---------------------------------------------------------------------------
// build_context
// ---------------------------------------------------------------------------

pub fn build_context(
    input: &ContextInput,
    task_repo: &dyn TaskRepository,
    agent_repo: &dyn AgentRepository,
    checkpoint_repo: &dyn CheckpointRepository,
    progress_repo: &dyn ProgressRepository,
    dependency_repo: &dyn DependencyRepository,
    decision_repo: &dyn DecisionRepository,
    event_repo: &dyn EventRepository,
    git_cli: &crate::adapter::git::GitCli,
    repo_path: Option<&Path>,
) -> Result<ContextResult, CarryCtxError> {
    let warnings: Vec<String> = Vec::new();
    let mut sections: Vec<ContextSection> = Vec::new();

    let task_ref = input.task_id.as_deref().unwrap_or("current");
    let task = resolve_task(&input.project_id, task_ref, task_repo)?;

    // 1 – Task
    sections.push(ContextSection {
        kind: "task".into(),
        rank: 1,
        title: "Task".into(),
        content: serde_json::json!({
            "id": task.id,
            "displayId": task.display_id,
            "title": task.title,
            "status": task.status,
            "priority": task.priority,
            "ownerAgentId": task.owner_agent_id,
            "description": task.description,
        }),
    });

    // 2 – Checkpoint
    if let Ok(Some(cp)) = checkpoint_repo.find_latest_for_task(&input.project_id, &task.id) {
        sections.push(ContextSection {
            kind: "checkpoint".into(),
            rank: 2,
            title: "Latest Checkpoint".into(),
            content: serde_json::json!({
                "id": cp.id,
                "head": cp.head,
                "dirty": cp.dirty,
                "stagedFiles": cp.staged_files,
                "modifiedFiles": cp.modified_files,
                "done": cp.done,
                "remaining": cp.remaining,
                "blockers": cp.blockers,
                "risks": cp.risks,
                "nextActions": cp.next_actions,
                "createdAt": cp.created_at,
            }),
        });
    }

    // 3 – Git
    if let Some(path) = repo_path {
        if let Ok(snapshot) = git_cli.get_snapshot(path) {
            sections.push(ContextSection {
                kind: "git".into(),
                rank: 3,
                title: "Git Status".into(),
                content: serde_json::json!({
                    "branch": snapshot.branch,
                    "head": snapshot.head,
                    "dirty": snapshot.dirty,
                    "untracked": snapshot.untracked,
                    "modified": snapshot.modified,
                    "staged": snapshot.staged,
                    "diffStats": snapshot.diff_stats,
                }),
            });
        }
    }

    // 4 – Blockers
    let progress = load_progress_summary(&input.project_id, &task.id, progress_repo)?;
    if !progress.blockers.is_empty() {
        sections.push(ContextSection {
            kind: "blockers".into(),
            rank: 4,
            title: "Blockers".into(),
            content: serde_json::json!(progress.blockers),
        });
    }

    // 5 – Todos
    if !progress.remaining.is_empty() {
        sections.push(ContextSection {
            kind: "todos".into(),
            rank: 5,
            title: "Remaining Work".into(),
            content: serde_json::json!(progress.remaining),
        });
    }

    // 6 – Dependencies
    if let Ok(deps) = dependency_repo.list_for_task(&input.project_id, &task.id) {
        if !deps.is_empty() {
            let deps_json: Vec<serde_json::Value> = deps
                .iter()
                .map(|d| {
                    serde_json::json!({
                        "taskId": d.task_id,
                        "prerequisiteId": d.prerequisite_id,
                        "kind": d.kind,
                    })
                })
                .collect();
            sections.push(ContextSection {
                kind: "dependencies".into(),
                rank: 6,
                title: "Dependencies".into(),
                content: serde_json::json!(deps_json),
            });
        }
    }

    // 7 – Blocked tasks (tasks that depend on this one)
    if input.include_related_tasks {
        if let Ok(all_deps) = dependency_repo.list_all_for_project(&input.project_id) {
            let blocked: Vec<String> = all_deps
                .iter()
                .filter(|d| d.prerequisite_id == task.id)
                .map(|d| d.task_id.clone())
                .collect();
            if !blocked.is_empty() {
                sections.push(ContextSection {
                    kind: "blocked_tasks".into(),
                    rank: 7,
                    title: "Blocked Tasks".into(),
                    content: serde_json::json!(blocked),
                });
            }
        }
    }

    // 9 – Decisions (skip group 8 which is scope overlaps, not yet implemented)
    if input.include_decisions {
        if let Ok(decisions) = decision_repo.list(&input.project_id) {
            let relevant: Vec<serde_json::Value> = decisions
                .iter()
                .filter(|d| d.related_tasks.contains(&task.id))
                .map(|d| {
                    serde_json::json!({
                        "id": d.id,
                        "displayId": d.display_id,
                        "title": d.title,
                        "decision": d.decision,
                        "context": d.context,
                    })
                })
                .collect();
            if !relevant.is_empty() {
                sections.push(ContextSection {
                    kind: "decisions".into(),
                    rank: 9,
                    title: "Related Decisions".into(),
                    content: serde_json::json!(relevant),
                });
            }
        }
    }

    // 10 – Agent tasks
    if let Ok(agents) = agent_repo.list(&AgentFilter {
        project_id: input.project_id.clone(),
        status: Some(AgentStatus::Active),
    }) {
        let mut agent_task_list = Vec::new();
        for a in &agents {
            if let Ok(tasks) = task_repo.list(&TaskFilter {
                project_id: input.project_id.clone(),
                status: Some(TaskStatus::InProgress),
                owner_agent_id: Some(a.id.clone()),
                ready: false,
                blocked: false,
                mine: None,
            }) {
                if !tasks.is_empty() {
                    let tlist: Vec<serde_json::Value> = tasks
                        .iter()
                        .map(|t| {
                            serde_json::json!({
                                "id": t.id,
                                "displayId": t.display_id,
                                "title": t.title,
                                "status": t.status,
                            })
                        })
                        .collect();
                    agent_task_list.push(serde_json::json!({
                        "agent": a.name,
                        "agentId": a.id,
                        "tasks": tlist,
                    }));
                }
            }
        }
        if !agent_task_list.is_empty() {
            sections.push(ContextSection {
                kind: "agent_tasks".into(),
                rank: 10,
                title: "Tasks by Agent".into(),
                content: serde_json::json!(agent_task_list),
            });
        }
    }

    // 11 – Task events
    if input.include_events {
        if let Ok(events) = event_repo.list(&EventFilter {
            project_id: input.project_id.clone(),
            task_id: Some(task.id.clone()),
            agent_id: None,
            session_id: None,
            event_type: None,
            since: input.since.clone(),
            until: None,
            limit: Some(input.max_events),
        }) {
            let ev_json: Vec<serde_json::Value> = events
                .iter()
                .map(|e| {
                    serde_json::json!({
                        "id": e.id,
                        "type": e.event_type,
                        "actorAgentId": e.actor_agent_id,
                        "sessionId": e.session_id,
                        "payload": e.payload,
                        "occurredAt": e.occurred_at,
                    })
                })
                .collect();
            if !ev_json.is_empty() {
                sections.push(ContextSection {
                    kind: "task_events".into(),
                    rank: 11,
                    title: "Task Events".into(),
                    content: serde_json::json!(ev_json),
                });
            }
        }
    }

    // 12 – Project activity
    if let Ok(events) = event_repo.list(&EventFilter {
        project_id: input.project_id.clone(),
        task_id: None,
        agent_id: None,
        session_id: None,
        event_type: None,
        since: input.since.clone(),
        until: None,
        limit: Some(10),
    }) {
        let activity: Vec<serde_json::Value> = events
            .iter()
            .map(|e| {
                serde_json::json!({
                    "id": e.id,
                    "type": e.event_type,
                    "occurredAt": e.occurred_at,
                })
            })
            .collect();
        if !activity.is_empty() {
            sections.push(ContextSection {
                kind: "project_activity".into(),
                rank: 12,
                title: "Recent Activity".into(),
                content: serde_json::json!(activity),
            });
        }
    }

    if input.compact {
        sections.retain(|s| s.rank <= 6);
    }

    sections.sort_by_key(|s| s.rank);

    Ok(ContextResult { sections, warnings })
}

// ---------------------------------------------------------------------------
// get_status
// ---------------------------------------------------------------------------

pub fn get_status(
    input: &StatusInput,
    task_repo: &dyn TaskRepository,
    agent_repo: &dyn AgentRepository,
    session_repo: &dyn SessionRepository,
    worktree_repo: &dyn WorktreeRepository,
    event_repo: &dyn EventRepository,
    _progress_repo: &dyn ProgressRepository,
    _dependency_repo: &dyn DependencyRepository,
) -> Result<StatusResult, CarryCtxError> {
    let mut warnings: Vec<String> = Vec::new();
    let mut counts: HashMap<String, u64> = HashMap::new();

    // Agents
    let agents = agent_repo.list(&AgentFilter {
        project_id: input.project_id.clone(),
        status: None,
    })?;

    // Current agent (auto-detect if exactly one active)
    let current_agent = {
        let active_agents: Vec<_> = agents
            .iter()
            .filter(|a| a.status == AgentStatus::Active)
            .collect();
        if active_agents.len() == 1 {
            Some(active_agents[0].name.clone())
        } else {
            None
        }
    };

    let active_agent_id = current_agent.as_ref().and_then(|name| {
        agents
            .iter()
            .find(|a| a.name == *name && a.status == AgentStatus::Active)
            .map(|a| a.id.clone())
    });

    counts.insert("agents_total".into(), agents.len() as u64);
    counts.insert(
        "agents_active".into(),
        agents
            .iter()
            .filter(|a| a.status == AgentStatus::Active)
            .count() as u64,
    );

    // Tasks
    let task_filter = if input.mine {
        if let Some(ref aid) = active_agent_id {
            TaskFilter {
                project_id: input.project_id.clone(),
                status: None,
                owner_agent_id: Some(aid.clone()),
                ready: false,
                blocked: false,
                mine: None,
            }
        } else {
            warnings.push("--mine requires a single active agent.".into());
            TaskFilter {
                project_id: input.project_id.clone(),
                status: None,
                owner_agent_id: None,
                ready: false,
                blocked: false,
                mine: None,
            }
        }
    } else {
        TaskFilter {
            project_id: input.project_id.clone(),
            status: None,
            owner_agent_id: None,
            ready: false,
            blocked: false,
            mine: None,
        }
    };

    let all_tasks = task_repo.list(&task_filter)?;

    let count_status = |status: TaskStatus| -> u64 {
        all_tasks.iter().filter(|t| t.status == status).count() as u64
    };

    counts.insert("tasks_total".into(), all_tasks.len() as u64);
    counts.insert("tasks_planned".into(), count_status(TaskStatus::Planned));
    counts.insert("tasks_ready".into(), count_status(TaskStatus::Ready));
    counts.insert(
        "tasks_in_progress".into(),
        count_status(TaskStatus::InProgress),
    );
    counts.insert("tasks_blocked".into(), count_status(TaskStatus::Blocked));
    counts.insert("tasks_review".into(), count_status(TaskStatus::Review));
    counts.insert(
        "tasks_completed".into(),
        count_status(TaskStatus::Completed),
    );
    counts.insert(
        "tasks_cancelled".into(),
        count_status(TaskStatus::Cancelled),
    );

    // Sessions
    let all_sessions = session_repo.list(&input.project_id)?;
    counts.insert("sessions_total".into(), all_sessions.len() as u64);
    counts.insert(
        "sessions_active".into(),
        all_sessions
            .iter()
            .filter(|s| s.state == SessionState::Active)
            .count() as u64,
    );

    // Worktrees
    let all_worktrees = worktree_repo.list(&input.project_id)?;
    counts.insert("worktrees_total".into(), all_worktrees.len() as u64);

    // Detail sections
    let sessions = if input.show_sessions {
        all_sessions
            .iter()
            .map(|s| format!("{} via {} [{:?}]", s.id, s.agent_id, s.state))
            .collect()
    } else {
        Vec::new()
    };

    let tasks = if input.show_tasks {
        all_tasks
            .iter()
            .map(|t| format!("{}: {} [{:?}]", t.display_id, t.title, t.status))
            .collect()
    } else {
        Vec::new()
    };

    let worktrees = if input.show_worktrees {
        all_worktrees
            .iter()
            .map(|w| {
                format!(
                    "{} (branch: {})",
                    w.path,
                    w.branch.as_deref().unwrap_or("detached")
                )
            })
            .collect()
    } else {
        Vec::new()
    };

    let activity = if let Ok(events) = event_repo.list(&EventFilter {
        project_id: input.project_id.clone(),
        task_id: None,
        agent_id: None,
        session_id: None,
        event_type: None,
        since: input.since.clone(),
        until: None,
        limit: Some(20),
    }) {
        events
            .iter()
            .map(|e| format!("[{}] {} ({})", e.occurred_at, e.event_type, e.id))
            .collect()
    } else {
        Vec::new()
    };

    if input.compact {
        return Ok(StatusResult {
            project_id: input.project_id.clone(),
            current_agent,
            counts,
            sessions: Vec::new(),
            tasks: Vec::new(),
            worktrees: Vec::new(),
            activity: Vec::new(),
            warnings,
        });
    }

    Ok(StatusResult {
        project_id: input.project_id.clone(),
        current_agent,
        counts,
        sessions,
        tasks,
        worktrees,
        activity,
        warnings,
    })
}
