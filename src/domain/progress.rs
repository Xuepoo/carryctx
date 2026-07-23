use crate::error::CarryCtxError;

/// Progress item type
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressType {
    Todo,
    Blocker,
    Risk,
    Note,
}

impl ProgressType {
    pub fn name(self) -> &'static str {
        match self {
            Self::Todo => "todo",
            Self::Blocker => "blocker",
            Self::Risk => "risk",
            Self::Note => "note",
        }
    }
}

impl TryFrom<&str> for ProgressType {
    type Error = CarryCtxError;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "todo" => Ok(Self::Todo),
            "blocker" | "block" => Ok(Self::Blocker),
            "risk" => Ok(Self::Risk),
            "note" => Ok(Self::Note),
            _ => Err(CarryCtxError::invalid_arguments(format!(
                "Unknown progress type: {}",
                s
            ))),
        }
    }
}

/// Progress item lifecycle status
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressStatus {
    Open,
    Completed,
    Removed,
}

/// Progress transition action
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressAction {
    Complete,
    Reopen,
    Remove,
}

/// Result of evaluating a progress transition
pub fn evaluate_progress_transition(
    current: ProgressStatus,
    action: ProgressAction,
) -> Result<ProgressStatus, CarryCtxError> {
    match (current, action) {
        (ProgressStatus::Open, ProgressAction::Complete) => Ok(ProgressStatus::Completed),
        (ProgressStatus::Open, ProgressAction::Remove) => Ok(ProgressStatus::Removed),
        (ProgressStatus::Completed, ProgressAction::Reopen) => Ok(ProgressStatus::Open),
        (ProgressStatus::Completed, ProgressAction::Remove) => Ok(ProgressStatus::Removed),
        _ => Err(CarryCtxError::invalid_task_transition(
            &format!("{:?}", current),
            &format!("{:?}", action),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_progress_complete() {
        let result = evaluate_progress_transition(ProgressStatus::Open, ProgressAction::Complete);
        assert_eq!(result.unwrap(), ProgressStatus::Completed);
    }

    #[test]
    fn test_progress_reopen() {
        let result =
            evaluate_progress_transition(ProgressStatus::Completed, ProgressAction::Reopen);
        assert_eq!(result.unwrap(), ProgressStatus::Open);
    }

    #[test]
    fn test_progress_remove_from_any_state() {
        assert!(evaluate_progress_transition(ProgressStatus::Open, ProgressAction::Remove).is_ok());
        assert!(
            evaluate_progress_transition(ProgressStatus::Completed, ProgressAction::Remove).is_ok()
        );
    }

    #[test]
    fn test_removed_cannot_transition() {
        assert!(
            evaluate_progress_transition(ProgressStatus::Removed, ProgressAction::Complete)
                .is_err()
        );
        assert!(
            evaluate_progress_transition(ProgressStatus::Removed, ProgressAction::Reopen).is_err()
        );
    }
}
