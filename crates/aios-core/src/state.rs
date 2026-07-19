use serde::{Deserialize, Serialize};

use crate::StateTransitionError;

/// Runtime lifecycle of a task.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Submitted,
    Validating,
    Rejected,
    Queued,
    Running,
    WaitingApproval,
    Succeeded,
    Failed,
    Cancelled,
}

impl TaskState {
    /// Returns whether no more work may be performed for this task instance.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Rejected | Self::Succeeded | Self::Failed | Self::Cancelled
        )
    }

    /// Returns whether the requested lifecycle transition is allowed.
    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Submitted, Self::Validating)
                | (Self::Validating, Self::Rejected | Self::Queued)
                | (Self::Queued, Self::Running)
                | (
                    Self::Running,
                    Self::WaitingApproval | Self::Succeeded | Self::Failed
                )
                | (Self::WaitingApproval, Self::Running | Self::Failed)
        ) || (!self.is_terminal() && matches!(next, Self::Cancelled))
    }

    /// Applies a lifecycle transition, rejecting invalid and repeated transitions.
    pub fn transition_to(&mut self, next: Self) -> Result<(), StateTransitionError> {
        if !self.can_transition_to(next) {
            return Err(StateTransitionError::new(*self, next));
        }

        *self = next;
        Ok(())
    }

    /// Cancels a non-terminal task. Repeated cancellation is a no-op.
    ///
    /// Returns `true` only when the state changed.
    pub fn cancel(&mut self) -> bool {
        if self.is_terminal() {
            return false;
        }

        *self = Self::Cancelled;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::TaskState;

    #[test]
    fn accepts_complete_success_path() {
        let mut state = TaskState::Submitted;

        for next in [
            TaskState::Validating,
            TaskState::Queued,
            TaskState::Running,
            TaskState::WaitingApproval,
            TaskState::Running,
            TaskState::Succeeded,
        ] {
            assert_eq!(state.transition_to(next), Ok(()));
        }

        assert!(state.is_terminal());
    }

    #[test]
    fn rejects_skipping_validation() {
        let mut state = TaskState::Submitted;

        let error = state
            .transition_to(TaskState::Running)
            .expect_err("submitted tasks must be validated before execution");

        assert_eq!(error.from(), TaskState::Submitted);
        assert_eq!(error.to(), TaskState::Running);
        assert_eq!(state, TaskState::Submitted);
    }

    #[test]
    fn terminal_states_cannot_transition() {
        for terminal in [
            TaskState::Rejected,
            TaskState::Succeeded,
            TaskState::Failed,
            TaskState::Cancelled,
        ] {
            let mut state = terminal;
            assert!(state.transition_to(TaskState::Running).is_err());
            assert_eq!(state, terminal);
        }
    }

    #[test]
    fn cancellation_is_idempotent() {
        let mut state = TaskState::Running;

        assert!(state.cancel());
        assert!(!state.cancel());
        assert_eq!(state, TaskState::Cancelled);
    }
}
