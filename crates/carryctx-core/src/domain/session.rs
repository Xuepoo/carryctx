use crate::error::CarryCtxError;

/// Session state (5-state model)
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Active,
    Paused,
    Ended,
    Stale,
    Abandoned,
}

impl SessionState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Ended | Self::Abandoned)
    }

    pub fn can_transition_to(self, target: Self) -> bool {
        matches!(
            (self, target),
            (Self::Active, Self::Paused)
                | (Self::Active, Self::Ended)
                | (Self::Active, Self::Stale)
                | (Self::Active, Self::Abandoned)
                | (Self::Paused, Self::Active)
                | (Self::Paused, Self::Ended)
                | (Self::Paused, Self::Abandoned)
                | (Self::Stale, Self::Active)
                | (Self::Stale, Self::Ended)
                | (Self::Stale, Self::Abandoned)
        )
    }
}

/// Evaluate whether a session state transition is allowed
pub fn evaluate_session_transition(
    current: SessionState,
    target: SessionState,
) -> Result<(), CarryCtxError> {
    if current.can_transition_to(target) {
        Ok(())
    } else {
        Err(CarryCtxError::invalid_task_transition(
            &format!("{:?}", current),
            &format!("{:?}", target),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_transitions() {
        assert!(evaluate_session_transition(SessionState::Active, SessionState::Paused).is_ok());
        assert!(evaluate_session_transition(SessionState::Active, SessionState::Ended).is_ok());
        assert!(evaluate_session_transition(SessionState::Active, SessionState::Stale).is_ok());
        assert!(evaluate_session_transition(SessionState::Paused, SessionState::Active).is_ok());
        assert!(evaluate_session_transition(SessionState::Stale, SessionState::Active).is_ok());
    }

    #[test]
    fn test_terminal_rejects_transitions() {
        assert!(evaluate_session_transition(SessionState::Ended, SessionState::Active).is_err());
        assert!(
            evaluate_session_transition(SessionState::Abandoned, SessionState::Active).is_err()
        );
    }

    #[test]
    fn test_terminal_states() {
        assert!(SessionState::Ended.is_terminal());
        assert!(SessionState::Abandoned.is_terminal());
        assert!(!SessionState::Active.is_terminal());
    }
}
