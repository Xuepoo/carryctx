use crate::error::CarryCtxError;

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

/// Evaluate whether a transition action is allowed given current facts
pub fn evaluate_transition(
    current_status: TaskStatus,
    action: TransitionAction,
    facts: &TransitionFacts,
) -> TransitionOutcome {
    use TaskStatus as St;
    use TransitionAction as Ac;

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

        (Ac::Complete, St::Review | St::InProgress) => true,
        (Ac::Complete, _) if facts.has_open_progress && facts.strict_completion => {
            return TransitionOutcome::Denied(CarryCtxError::state_conflict(
                "Task has open progress items. Complete or remove them first.",
            ));
        }

        (Ac::Cancel, s) if !s.is_terminal() && facts.reason.is_some() => true,
        (Ac::Cancel, s) if !s.is_terminal() && facts.reason.is_none() => {
            return TransitionOutcome::Denied(CarryCtxError::validation_error(
                "Cancel reason is required for active tasks.",
            ));
        }

        (Ac::Reopen, St::Completed | St::Cancelled) if facts.strong_dependencies_complete => true,
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

    fn basic_facts(status: TaskStatus, has_owner: bool) -> TransitionFacts {
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
}
