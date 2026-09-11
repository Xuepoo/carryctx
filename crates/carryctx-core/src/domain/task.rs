use crate::domain::agent::AgentKind;
use crate::error::CarryCtxError;

/// Hard cap for task titles, counted in characters. Titles are duplicated
/// into event payloads, so one unbounded title used to double its storage
/// footprint across the audit log. Aligned in spirit with the 64-char agent
/// name cap while leaving room for descriptive titles.
pub const MAX_TITLE_CHARS: usize = 200;

/// Hard cap for task descriptions, counted in characters. Descriptions are
/// duplicated into event payloads like titles.
pub const MAX_DESCRIPTION_CHARS: usize = 8_000;

/// Validate a task title at the use-case boundary (create/edit).
pub fn validate_title(title: &str) -> Result<(), CarryCtxError> {
    let len = title.chars().count();
    if len > MAX_TITLE_CHARS {
        return Err(CarryCtxError::validation_error(format!(
            "Task title is {len} characters; the maximum is {MAX_TITLE_CHARS}."
        )));
    }
    Ok(())
}

/// Validate a task description at the use-case boundary (create/edit).
pub fn validate_description(description: &str) -> Result<(), CarryCtxError> {
    let len = description.chars().count();
    if len > MAX_DESCRIPTION_CHARS {
        return Err(CarryCtxError::validation_error(format!(
            "Task description is {len} characters; the maximum is {MAX_DESCRIPTION_CHARS}."
        )));
    }
    Ok(())
}

/// Task status (7-state model)
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Planned,
    Ready,
    InProgress,
    Blocked,
    Review,
    Completed,
    Cancelled,
}

impl TaskStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled)
    }

    pub fn is_active(self) -> bool {
        matches!(self, Self::InProgress | Self::Review | Self::Blocked)
    }
}

/// Single source of truth for "this strong prerequisite no longer blocks
/// work": any terminal state counts as settled — a cancelled prerequisite is
/// just as settled as a completed one.
///
/// Every completeness gate must derive from this predicate (creation gating,
/// claim/transition gating via `list_incomplete_strong_dependencies`, and
/// ready-promotion). Previously creation counted cancelled prerequisites as
/// incomplete while claim/transition treated them as complete, so the same
/// task could be born Planned yet immediately claimable.
pub fn prerequisite_settled(status: TaskStatus) -> bool {
    status.is_terminal()
}

/// Task priority
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum TaskPriority {
    Low,
    #[default]
    Normal,
    High,
    Urgent,
}

/// Transition action
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionAction {
    Claim,
    Release,
    Start,
    Block,
    Unblock,
    Review,
    Complete,
    Cancel,
    Reopen,
}

impl TransitionAction {
    pub fn name(self) -> &'static str {
        match self {
            Self::Claim => "claim",
            Self::Release => "release",
            Self::Start => "start",
            Self::Block => "block",
            Self::Unblock => "unblock",
            Self::Review => "review",
            Self::Complete => "complete",
            Self::Cancel => "cancel",
            Self::Reopen => "reopen",
        }
    }

    pub fn past_tense(self) -> &'static str {
        match self {
            Self::Claim => "task.claimed",
            Self::Release => "task.released",
            Self::Start => "task.started",
            Self::Block => "task.blocked",
            Self::Unblock => "task.unblocked",
            Self::Review => "task.reviewed",
            Self::Complete => "task.completed",
            Self::Cancel => "task.cancelled",
            Self::Reopen => "task.reopened",
        }
    }
}

impl TryFrom<&str> for TransitionAction {
    type Error = CarryCtxError;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "claim" => Ok(Self::Claim),
            "release" => Ok(Self::Release),
            "start" => Ok(Self::Start),
            "block" => Ok(Self::Block),
            "unblock" => Ok(Self::Unblock),
            "review" => Ok(Self::Review),
            "complete" => Ok(Self::Complete),
            "cancel" => Ok(Self::Cancel),
            "reopen" => Ok(Self::Reopen),
            _ => Err(CarryCtxError::invalid_arguments(format!(
                "Unknown transition action: {}",
                s
            ))),
        }
    }
}

/// Facts needed to evaluate a transition
pub struct TransitionFacts {
    pub has_owner: bool,
    pub strong_dependencies_complete: bool,
    pub has_active_session: bool,
    pub has_open_progress: bool,
    pub strict_completion: bool,
    pub reason: Option<String>,
    pub task_display_id: String,
    pub owner: Option<String>,
    /// Canonical id of the acting agent, when one could be resolved.
    pub actor_agent_id: Option<String>,
    /// Execution kind of the acting agent; `None` is the unclassified/legacy
    /// case that keeps pre-team behavior (CTX-0044).
    pub actor_kind: Option<AgentKind>,
}

/// Result of evaluating a transition
pub enum TransitionOutcome {
    Allowed {
        new_status: TaskStatus,
        clears_owner: bool,
        warnings: Vec<String>,
    },
    Denied(CarryCtxError),
}

impl TransitionOutcome {
    pub fn allowed(self) -> Result<(TaskStatus, bool, Vec<String>), CarryCtxError> {
        match self {
            Self::Allowed {
                new_status,
                clears_owner,
                warnings,
            } => Ok((new_status, clears_owner, warnings)),
            Self::Denied(e) => Err(e),
        }
    }
}

/// Whether the acting agent is allowed to release the task.
///
/// Invariant I2 (design 2026-08-21 §4.4): removing another agent's ownership
/// requires being the owner, an explicit `commander` override, or an
/// unclassified legacy actor. A peer `subagent` is rejected. Releasing an
/// unowned task removes nothing and is always allowed.
fn release_authorized(facts: &TransitionFacts) -> bool {
    if !facts.has_owner {
        return true;
    }
    match facts.actor_kind {
        None => true,
        Some(AgentKind::Commander) => true,
        Some(AgentKind::Subagent) => {
            facts.actor_agent_id.is_some() && facts.actor_agent_id == facts.owner
        }
    }
}

/// Evaluate whether a transition action is allowed given current facts
pub fn evaluate_transition(
    current_status: TaskStatus,
    action: TransitionAction,
    facts: &TransitionFacts,
) -> TransitionOutcome {
    use TaskStatus as St;
    use TransitionAction as Ac;

    // Ownership authorization is checked before the state machine so a peer
    // subagent gets a stable TASK_NOT_OWNED (exit 9) rather than a status
    // error, and cannot clear ownership by racing the session guard.
    if action == Ac::Release && !release_authorized(facts) {
        return TransitionOutcome::Denied(CarryCtxError::task_not_owned(
            &facts.task_display_id,
            facts.owner.as_deref().unwrap_or("unknown"),
        ));
    }

    let allowed = match (action, current_status) {
        (Ac::Claim, St::Ready) if !facts.has_owner && facts.strong_dependencies_complete => true,
        (Ac::Claim, _) if facts.has_owner => {
            return TransitionOutcome::Denied(CarryCtxError::task_already_claimed(
                &facts.task_display_id,
                facts.owner.as_deref().unwrap_or("unknown"),
            ));
        }
        (Ac::Claim, _) if !facts.strong_dependencies_complete => {
            return TransitionOutcome::Denied(CarryCtxError::dependency_incomplete(
                &facts.task_display_id,
            ));
        }

        (Ac::Release, St::InProgress | St::Blocked | St::Review) if !facts.has_active_session => {
            true
        }
        (Ac::Release, _) if facts.has_active_session => {
            return TransitionOutcome::Denied(CarryCtxError::state_conflict(
                "Cannot release task while an active session exists.",
            ));
        }

        (Ac::Start, St::Ready | St::Planned) if facts.strong_dependencies_complete => true,
        // Idempotent no-op: `task claim` already moves Ready -> InProgress, and
        // the documented workflow is claim-then-start. Starting an in-progress
        // task succeeds without changing anything.
        (Ac::Start, St::InProgress) => true,
        (Ac::Start, _) if !facts.strong_dependencies_complete => {
            return TransitionOutcome::Denied(CarryCtxError::dependency_incomplete(
                &facts.task_display_id,
            ));
        }

        (Ac::Block, St::InProgress | St::Ready | St::Planned | St::Review)
            if facts.reason.is_some() =>
        {
            true
        }
        (Ac::Block, _) if facts.reason.is_none() => {
            return TransitionOutcome::Denied(CarryCtxError::validation_error(
                "Block reason is required.",
            ));
        }

        (Ac::Unblock, St::Blocked | St::Planned) if !facts.strong_dependencies_complete => {
            return TransitionOutcome::Denied(CarryCtxError::dependency_incomplete(
                &facts.task_display_id,
            ));
        }
        (Ac::Unblock, St::Blocked | St::Planned) => true,

        (Ac::Review, St::InProgress) => true,

        // Completing while a strong blocker is still open would break the
        // dependency invariant at the finish line, so Complete is gated like
        // Claim and Start.
        (Ac::Complete, St::Review | St::InProgress) if !facts.strong_dependencies_complete => {
            return TransitionOutcome::Denied(CarryCtxError::dependency_incomplete(
                &facts.task_display_id,
            ));
        }
        (Ac::Complete, St::Review | St::InProgress)
            if facts.has_open_progress && facts.strict_completion =>
        {
            return TransitionOutcome::Denied(CarryCtxError::state_conflict(
                "Task has open progress items. Complete or remove them first.",
            ));
        }
        (Ac::Complete, St::Review | St::InProgress) => true,

        (Ac::Cancel, s) if !s.is_terminal() && facts.reason.is_some() => true,
        (Ac::Cancel, s) if !s.is_terminal() && facts.reason.is_none() => {
            return TransitionOutcome::Denied(CarryCtxError::validation_error(
                "Cancel reason is required for active tasks.",
            ));
        }

        // Reopen deliberately ignores dependency state: a terminal task can
        // always be reopened, and the resulting status (ready vs planned)
        // reflects the current dependency facts below.
        (Ac::Reopen, St::Completed | St::Cancelled) => true,

        _ => false,
    };

    if !allowed {
        return TransitionOutcome::Denied(CarryCtxError::invalid_task_transition(
            &format!("{:?}", current_status),
            action.name(),
        ));
    }

    let (new_status, clears_owner) = match action {
        Ac::Claim => (St::InProgress, false),
        Ac::Release => {
            let status = if facts.strong_dependencies_complete {
                St::Ready
            } else {
                St::Planned
            };
            return TransitionOutcome::Allowed {
                new_status: status,
                clears_owner: true,
                warnings: vec![],
            };
        }
        Ac::Start => (St::InProgress, false),
        Ac::Block => (St::Blocked, false),
        Ac::Unblock => {
            let status = if facts.has_owner {
                St::InProgress
            } else {
                St::Ready
            };
            return TransitionOutcome::Allowed {
                new_status: status,
                clears_owner: false,
                warnings: vec![],
            };
        }
        Ac::Review => (St::Review, false),
        Ac::Complete => {
            let mut warnings = vec![];
            if facts.has_open_progress && !facts.strict_completion {
                warnings.push("Task has open progress items.".into());
            }
            return TransitionOutcome::Allowed {
                new_status: St::Completed,
                clears_owner: false,
                warnings,
            };
        }
        Ac::Cancel => (St::Cancelled, true),
        Ac::Reopen => {
            let status = if facts.strong_dependencies_complete {
                St::Ready
            } else {
                St::Planned
            };
            return TransitionOutcome::Allowed {
                new_status: status,
                clears_owner: true,
                warnings: vec![],
            };
        }
    };

    TransitionOutcome::Allowed {
        new_status,
        clears_owner,
        warnings: vec![],
    }
}

/// Determine initial status on creation
pub fn initial_status(dependencies_complete: bool, explicit_planned: bool) -> TaskStatus {
    if explicit_planned {
        TaskStatus::Planned
    } else if dependencies_complete {
        TaskStatus::Ready
    } else {
        TaskStatus::Planned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn basic_facts(_status: TaskStatus, has_owner: bool) -> TransitionFacts {
        TransitionFacts {
            has_owner,
            strong_dependencies_complete: true,
            has_active_session: false,
            has_open_progress: false,
            strict_completion: false,
            reason: Some("reason".into()),
            task_display_id: "CTX-0001".into(),
            owner: if has_owner {
                Some("agent".into())
            } else {
                None
            },
            actor_agent_id: Some(if has_owner {
                "agent".into()
            } else {
                "actor".into()
            }),
            actor_kind: None,
        }
    }

    #[test]
    fn test_claim_ready_unowned() {
        let facts = basic_facts(TaskStatus::Ready, false);
        let result = evaluate_transition(TaskStatus::Ready, TransitionAction::Claim, &facts);
        let (status, clears, _) = result.allowed().unwrap();
        assert_eq!(status, TaskStatus::InProgress);
        assert!(!clears);
    }

    #[test]
    fn test_complete_with_open_strong_dependency_denied() {
        // CTX-0071: Complete is dependency-gated like Claim and Start.
        let mut facts = basic_facts(TaskStatus::InProgress, true);
        facts.strong_dependencies_complete = false;
        for status in [TaskStatus::InProgress, TaskStatus::Review] {
            let outcome = evaluate_transition(status, TransitionAction::Complete, &facts);
            assert!(
                matches!(outcome, TransitionOutcome::Denied(_)),
                "complete from {status:?} with open strong deps must be denied"
            );
        }
    }

    #[test]
    fn test_complete_with_dependencies_complete_allowed() {
        let facts = basic_facts(TaskStatus::Review, true);
        let outcome = evaluate_transition(TaskStatus::Review, TransitionAction::Complete, &facts);
        let (status, _, warnings) = outcome.allowed().unwrap();
        assert_eq!(status, TaskStatus::Completed);
        assert!(warnings.is_empty());
    }

    #[test]
    fn test_claim_already_owned() {
        let facts = basic_facts(TaskStatus::Ready, true);
        let result = evaluate_transition(TaskStatus::Ready, TransitionAction::Claim, &facts);
        assert!(result.allowed().is_err());
    }

    #[test]
    fn test_complete_review() {
        let facts = basic_facts(TaskStatus::Review, true);
        let result = evaluate_transition(TaskStatus::Review, TransitionAction::Complete, &facts);
        let (status, _, _) = result.allowed().unwrap();
        assert_eq!(status, TaskStatus::Completed);
    }

    #[test]
    fn test_cancel_ready_requires_reason() {
        let mut facts = basic_facts(TaskStatus::Ready, false);
        facts.reason = None;
        let result = evaluate_transition(TaskStatus::Ready, TransitionAction::Cancel, &facts);
        assert!(result.allowed().is_err());
    }

    #[test]
    fn test_terminal_is_terminal() {
        assert!(TaskStatus::Completed.is_terminal());
        assert!(TaskStatus::Cancelled.is_terminal());
        assert!(!TaskStatus::InProgress.is_terminal());
    }

    #[test]
    fn test_initial_status_ready() {
        assert_eq!(initial_status(true, false), TaskStatus::Ready);
    }

    #[test]
    fn test_initial_status_planned() {
        assert_eq!(initial_status(false, false), TaskStatus::Planned);
        assert_eq!(initial_status(true, true), TaskStatus::Planned);
    }

    #[test]
    fn test_block_requires_reason() {
        let mut facts = basic_facts(TaskStatus::InProgress, true);
        facts.reason = None;
        let result = evaluate_transition(TaskStatus::InProgress, TransitionAction::Block, &facts);
        assert!(result.allowed().is_err());
    }

    #[test]
    fn test_start_in_progress_is_idempotent() {
        // `task claim` moves Ready -> InProgress; the documented workflow is
        // claim-then-start, so starting an in-progress task must succeed as a
        // no-op rather than erroring "Cannot transition from InProgress".
        let facts = basic_facts(TaskStatus::InProgress, true);
        let result = evaluate_transition(TaskStatus::InProgress, TransitionAction::Start, &facts);
        let (status, clears, _) = result.allowed().unwrap();
        assert_eq!(status, TaskStatus::InProgress);
        assert!(!clears);
    }

    #[test]
    fn test_prerequisite_settled_matches_terminal() {
        // CTX-0072: one shared definition — both terminal states settle a
        // strong prerequisite, every non-terminal state blocks.
        for status in [TaskStatus::Completed, TaskStatus::Cancelled] {
            assert!(prerequisite_settled(status), "{status:?} settles");
        }
        for status in [
            TaskStatus::Planned,
            TaskStatus::Ready,
            TaskStatus::InProgress,
            TaskStatus::Blocked,
            TaskStatus::Review,
        ] {
            assert!(!prerequisite_settled(status), "{status:?} blocks");
        }
    }

    #[test]
    fn test_reopen_ignores_dependency_state() {
        // CTX-0072: reopen from a terminal state is allowed regardless of
        // dependency completeness; only the resulting status differs.
        let mut facts = basic_facts(TaskStatus::Completed, false);
        facts.strong_dependencies_complete = false;
        let outcome = evaluate_transition(TaskStatus::Completed, TransitionAction::Reopen, &facts);
        let (status, clears, _) = outcome.allowed().unwrap();
        assert_eq!(status, TaskStatus::Planned);
        assert!(clears);

        facts.strong_dependencies_complete = true;
        let outcome = evaluate_transition(TaskStatus::Completed, TransitionAction::Reopen, &facts);
        let (status, clears, _) = outcome.allowed().unwrap();
        assert_eq!(status, TaskStatus::Ready);
        assert!(clears);
    }

    // ── CTX-0044: release ownership authorization (design §4.4) ──────────────

    #[test]
    fn test_release_owner_allowed() {
        let facts = basic_facts(TaskStatus::InProgress, true);
        let outcome =
            evaluate_transition(TaskStatus::InProgress, TransitionAction::Release, &facts);
        let (status, clears, _) = outcome.allowed().unwrap();
        assert_eq!(status, TaskStatus::Ready);
        assert!(clears);
    }

    #[test]
    fn test_release_peer_subagent_denied_with_task_not_owned() {
        let mut facts = basic_facts(TaskStatus::InProgress, true);
        facts.actor_agent_id = Some("peer".into());
        facts.actor_kind = Some(AgentKind::Subagent);
        let outcome =
            evaluate_transition(TaskStatus::InProgress, TransitionAction::Release, &facts);
        match outcome {
            TransitionOutcome::Denied(err) => {
                assert_eq!(err.code, "TASK_NOT_OWNED");
                assert_eq!(err.exit_code, crate::error::ExitCode::PermissionScope);
            }
            TransitionOutcome::Allowed { .. } => {
                panic!("a peer subagent must not release another agent's task")
            }
        }
    }

    #[test]
    fn test_release_subagent_owner_allowed() {
        let mut facts = basic_facts(TaskStatus::InProgress, true);
        facts.actor_kind = Some(AgentKind::Subagent);
        assert!(
            evaluate_transition(TaskStatus::InProgress, TransitionAction::Release, &facts)
                .allowed()
                .is_ok(),
            "the owning subagent may release its own task"
        );
    }

    #[test]
    fn test_release_commander_override_allowed() {
        let mut facts = basic_facts(TaskStatus::InProgress, true);
        facts.actor_agent_id = Some("cmd".into());
        facts.actor_kind = Some(AgentKind::Commander);
        assert!(
            evaluate_transition(TaskStatus::InProgress, TransitionAction::Release, &facts)
                .allowed()
                .is_ok(),
            "a commander may override ownership"
        );
    }

    #[test]
    fn test_release_unclassified_legacy_actor_allowed() {
        let mut facts = basic_facts(TaskStatus::InProgress, true);
        facts.actor_agent_id = Some("legacy".into());
        facts.actor_kind = None;
        assert!(
            evaluate_transition(TaskStatus::InProgress, TransitionAction::Release, &facts)
                .allowed()
                .is_ok(),
            "unclassified agents keep the legacy unchecked behavior"
        );
    }

    #[test]
    fn test_release_unowned_task_allowed_for_subagent() {
        let mut facts = basic_facts(TaskStatus::InProgress, false);
        facts.actor_agent_id = Some("peer".into());
        facts.actor_kind = Some(AgentKind::Subagent);
        assert!(
            evaluate_transition(TaskStatus::InProgress, TransitionAction::Release, &facts)
                .allowed()
                .is_ok(),
            "releasing an unowned task removes nothing and stays allowed"
        );
    }
}
