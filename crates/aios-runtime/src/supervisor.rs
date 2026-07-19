use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};

use aios_core::{StateTransitionError, TaskSpec, TaskState, ValidationErrors};

use crate::{EventStore, EventStoreError, InMemoryEventStore, TaskEvent, TaskEventKind, TaskId};

const DEFAULT_MAX_TASKS: usize = 10_000;

struct TaskRecord {
    spec: TaskSpec,
    state: TaskState,
}

/// Public, non-sensitive task state returned by the supervisor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TaskSnapshot {
    pub task_id: TaskId,
    pub state: TaskState,
}

/// Result of submitting a task specification.
#[derive(Debug)]
pub enum SubmitResult {
    Accepted(TaskSnapshot),
    Existing(TaskSnapshot),
    Rejected {
        task: TaskSnapshot,
        errors: ValidationErrors,
    },
}

/// Supervisor operation failure.
#[derive(Debug)]
pub enum SupervisorError {
    TaskNotFound,
    IdempotencyConflict,
    CapacityExceeded,
    InvalidStateTransition(StateTransitionError),
    EventStore(EventStoreError),
}

impl Display for SupervisorError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::TaskNotFound => formatter.write_str("task not found"),
            Self::IdempotencyConflict => formatter.write_str("idempotency key conflict"),
            Self::CapacityExceeded => formatter.write_str("task capacity exceeded"),
            Self::InvalidStateTransition(error) => Display::fmt(error, formatter),
            Self::EventStore(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for SupervisorError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidStateTransition(error) => Some(error),
            Self::EventStore(error) => Some(error),
            Self::TaskNotFound | Self::IdempotencyConflict | Self::CapacityExceeded => None,
        }
    }
}

impl From<StateTransitionError> for SupervisorError {
    fn from(error: StateTransitionError) -> Self {
        Self::InvalidStateTransition(error)
    }
}

impl From<EventStoreError> for SupervisorError {
    fn from(error: EventStoreError) -> Self {
        Self::EventStore(error)
    }
}

/// Coordinates validated tasks and records every accepted state change.
pub struct TaskSupervisor<S = InMemoryEventStore> {
    tasks: BTreeMap<TaskId, TaskRecord>,
    idempotency_index: BTreeMap<String, TaskId>,
    event_store: S,
    max_tasks: usize,
}

impl Default for TaskSupervisor<InMemoryEventStore> {
    fn default() -> Self {
        Self::new(InMemoryEventStore::default())
    }
}

impl<S: EventStore> TaskSupervisor<S> {
    #[must_use]
    pub fn new(event_store: S) -> Self {
        Self {
            tasks: BTreeMap::new(),
            idempotency_index: BTreeMap::new(),
            event_store,
            max_tasks: DEFAULT_MAX_TASKS,
        }
    }

    pub fn with_max_tasks(event_store: S, max_tasks: usize) -> Result<Self, SupervisorError> {
        if max_tasks == 0 {
            return Err(SupervisorError::CapacityExceeded);
        }

        Ok(Self {
            tasks: BTreeMap::new(),
            idempotency_index: BTreeMap::new(),
            event_store,
            max_tasks,
        })
    }

    /// Submits a task, validates it, and leaves it queued or rejected.
    pub fn submit(&mut self, spec: TaskSpec) -> Result<SubmitResult, SupervisorError> {
        if let Some(task_id) = self.idempotency_index.get(&spec.idempotency_key).copied() {
            let existing = self
                .tasks
                .get(&task_id)
                .ok_or(SupervisorError::TaskNotFound)?;
            if existing.spec != spec {
                return Err(SupervisorError::IdempotencyConflict);
            }
            return Ok(SubmitResult::Existing(TaskSnapshot {
                task_id,
                state: existing.state,
            }));
        }
        if self.tasks.len() >= self.max_tasks {
            return Err(SupervisorError::CapacityExceeded);
        }

        let task_id = TaskId::new();
        let validation = spec.validate();
        let (state, event_kinds) = match &validation {
            Ok(()) => (
                TaskState::Queued,
                vec![
                    TaskEventKind::Submitted,
                    TaskEventKind::StateTransitioned {
                        from: TaskState::Submitted,
                        to: TaskState::Validating,
                    },
                    TaskEventKind::StateTransitioned {
                        from: TaskState::Validating,
                        to: TaskState::Queued,
                    },
                ],
            ),
            Err(errors) => (
                TaskState::Rejected,
                vec![
                    TaskEventKind::Submitted,
                    TaskEventKind::StateTransitioned {
                        from: TaskState::Submitted,
                        to: TaskState::Validating,
                    },
                    TaskEventKind::ValidationFailed {
                        error_count: errors.errors().len(),
                    },
                    TaskEventKind::StateTransitioned {
                        from: TaskState::Validating,
                        to: TaskState::Rejected,
                    },
                ],
            ),
        };

        self.event_store.append_batch(task_id, &event_kinds)?;
        let idempotency_key = spec.idempotency_key.clone();
        self.tasks.insert(task_id, TaskRecord { spec, state });
        self.idempotency_index.insert(idempotency_key, task_id);

        let task = TaskSnapshot { task_id, state };
        match validation {
            Ok(()) => Ok(SubmitResult::Accepted(task)),
            Err(errors) => Ok(SubmitResult::Rejected { task, errors }),
        }
    }

    #[must_use]
    pub fn get(&self, task_id: TaskId) -> Option<TaskSnapshot> {
        self.tasks.get(&task_id).map(|record| TaskSnapshot {
            task_id,
            state: record.state,
        })
    }

    pub fn start(&mut self, task_id: TaskId) -> Result<(), SupervisorError> {
        self.transition(task_id, TaskState::Running)
    }

    pub fn wait_for_approval(&mut self, task_id: TaskId) -> Result<(), SupervisorError> {
        self.transition(task_id, TaskState::WaitingApproval)
    }

    pub fn resume_after_approval(&mut self, task_id: TaskId) -> Result<(), SupervisorError> {
        self.transition(task_id, TaskState::Running)
    }

    pub fn succeed(&mut self, task_id: TaskId) -> Result<(), SupervisorError> {
        self.transition(task_id, TaskState::Succeeded)
    }

    pub fn fail(&mut self, task_id: TaskId) -> Result<(), SupervisorError> {
        self.transition(task_id, TaskState::Failed)
    }

    /// Cancels a non-terminal task and records one event. Repeated cancellation is a no-op.
    pub fn cancel(&mut self, task_id: TaskId) -> Result<bool, SupervisorError> {
        let current = self
            .tasks
            .get(&task_id)
            .ok_or(SupervisorError::TaskNotFound)?
            .state;
        if current.is_terminal() {
            return Ok(false);
        }

        self.apply_transition(task_id, current, TaskState::Cancelled)?;
        Ok(true)
    }

    pub fn events(
        &self,
        task_id: TaskId,
        after_sequence: u64,
    ) -> Result<Vec<TaskEvent>, SupervisorError> {
        if !self.tasks.contains_key(&task_id) {
            return Err(SupervisorError::TaskNotFound);
        }
        Ok(self.event_store.list(task_id, after_sequence)?)
    }

    fn transition(&mut self, task_id: TaskId, next: TaskState) -> Result<(), SupervisorError> {
        let current = self
            .tasks
            .get(&task_id)
            .ok_or(SupervisorError::TaskNotFound)?
            .state;
        let mut proposed = current;
        proposed.transition_to(next)?;

        self.apply_transition(task_id, current, next)
    }

    fn apply_transition(
        &mut self,
        task_id: TaskId,
        current: TaskState,
        next: TaskState,
    ) -> Result<(), SupervisorError> {
        self.event_store.append_batch(
            task_id,
            &[TaskEventKind::StateTransitioned {
                from: current,
                to: next,
            }],
        )?;

        let record = self
            .tasks
            .get_mut(&task_id)
            .ok_or(SupervisorError::TaskNotFound)?;
        record.state = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use aios_core::{
        ApprovalPolicy, Budget, CapabilitySet, FileAccess, FileCapability, NetworkPolicy, TaskSpec,
        TaskState,
    };

    use super::{SubmitResult, SupervisorError, TaskSupervisor};
    use crate::{InMemoryEventStore, TaskEventKind};

    fn valid_task() -> TaskSpec {
        TaskSpec {
            idempotency_key: "runtime-test-001".to_owned(),
            goal: "Inspect the repository".to_owned(),
            capabilities: CapabilitySet {
                filesystem: vec![FileCapability {
                    path: "/workspace/project".to_owned(),
                    access: FileAccess::Read,
                }],
                network: NetworkPolicy::Deny,
                tools: vec!["test_runner".to_owned()],
            },
            budget: Budget {
                wall_time_seconds: 300,
                memory_bytes: 1_073_741_824,
                max_parallel_agents: 1,
            },
            approval: ApprovalPolicy {
                required_for: vec!["git.commit".to_owned()],
            },
        }
    }

    fn accepted_task_id(result: SubmitResult) -> crate::TaskId {
        match result {
            SubmitResult::Accepted(task) => task.task_id,
            SubmitResult::Existing(_) | SubmitResult::Rejected { .. } => {
                panic!("expected an accepted task")
            }
        }
    }

    #[test]
    fn accepts_valid_task_and_records_validation_lifecycle() {
        let mut supervisor = TaskSupervisor::default();
        let task_id = accepted_task_id(supervisor.submit(valid_task()).expect("submit task"));

        assert_eq!(
            supervisor.get(task_id).expect("task exists").state,
            TaskState::Queued
        );
        let events = supervisor.events(task_id, 0).expect("list events");
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].kind, TaskEventKind::Submitted);
        assert_eq!(
            events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn rejects_invalid_task_without_recording_input_values() {
        let mut supervisor = TaskSupervisor::default();
        let mut spec = valid_task();
        spec.goal = "".to_owned();

        let result = supervisor.submit(spec).expect("submit task");
        let SubmitResult::Rejected { task, errors } = result else {
            panic!("expected rejected task");
        };

        assert_eq!(task.state, TaskState::Rejected);
        assert_eq!(errors.errors().len(), 1);
        let serialized =
            serde_json::to_string(&supervisor.events(task.task_id, 0).expect("events"))
                .expect("serialize events");
        assert!(!serialized.contains("goal"));
        assert!(!serialized.contains("capabilities"));
    }

    #[test]
    fn returns_existing_task_for_identical_idempotent_submission() {
        let mut supervisor = TaskSupervisor::default();
        let first = accepted_task_id(supervisor.submit(valid_task()).expect("submit task"));

        let second = supervisor.submit(valid_task()).expect("repeat task");
        let SubmitResult::Existing(task) = second else {
            panic!("expected existing task");
        };

        assert_eq!(task.task_id, first);
        assert_eq!(supervisor.events(first, 0).expect("events").len(), 3);
    }

    #[test]
    fn rejects_idempotency_key_reuse_with_different_input() {
        let mut supervisor = TaskSupervisor::default();
        supervisor.submit(valid_task()).expect("submit task");
        let mut conflicting = valid_task();
        conflicting.goal = "Perform a different task".to_owned();

        let error = supervisor
            .submit(conflicting)
            .expect_err("conflicting task must fail");
        assert!(matches!(error, SupervisorError::IdempotencyConflict));
    }

    #[test]
    fn records_complete_approval_and_success_lifecycle() {
        let mut supervisor = TaskSupervisor::default();
        let task_id = accepted_task_id(supervisor.submit(valid_task()).expect("submit task"));

        supervisor.start(task_id).expect("start task");
        supervisor
            .wait_for_approval(task_id)
            .expect("wait for approval");
        supervisor
            .resume_after_approval(task_id)
            .expect("resume task");
        supervisor.succeed(task_id).expect("succeed task");

        assert_eq!(
            supervisor.get(task_id).expect("task exists").state,
            TaskState::Succeeded
        );
        assert_eq!(supervisor.events(task_id, 0).expect("events").len(), 7);
    }

    #[test]
    fn invalid_transition_does_not_change_state_or_append_event() {
        let mut supervisor = TaskSupervisor::default();
        let task_id = accepted_task_id(supervisor.submit(valid_task()).expect("submit task"));

        let error = supervisor
            .succeed(task_id)
            .expect_err("queued task cannot succeed directly");

        assert!(matches!(error, SupervisorError::InvalidStateTransition(_)));
        assert_eq!(
            supervisor.get(task_id).expect("task exists").state,
            TaskState::Queued
        );
        assert_eq!(supervisor.events(task_id, 0).expect("events").len(), 3);
    }

    #[test]
    fn event_store_failure_does_not_register_task() {
        let store = InMemoryEventStore::new(2).expect("positive capacity");
        let mut supervisor = TaskSupervisor::new(store);

        let error = supervisor
            .submit(valid_task())
            .expect_err("submission needs three events");

        assert!(matches!(error, SupervisorError::EventStore(_)));
        assert!(matches!(
            supervisor.submit(valid_task()),
            Err(SupervisorError::EventStore(_))
        ));
    }

    #[test]
    fn cancellation_is_idempotent_and_records_one_event() {
        let mut supervisor = TaskSupervisor::default();
        let task_id = accepted_task_id(supervisor.submit(valid_task()).expect("submit task"));

        assert!(supervisor.cancel(task_id).expect("cancel task"));
        assert!(!supervisor.cancel(task_id).expect("repeat cancellation"));
        assert_eq!(supervisor.events(task_id, 0).expect("events").len(), 4);
    }

    #[test]
    fn capacity_rejects_new_tasks_but_preserves_idempotent_lookup() {
        let mut supervisor = TaskSupervisor::with_max_tasks(InMemoryEventStore::default(), 1)
            .expect("positive capacity");
        let first_id = accepted_task_id(supervisor.submit(valid_task()).expect("submit task"));

        let repeated = supervisor.submit(valid_task()).expect("repeat task");
        assert!(matches!(
            repeated,
            SubmitResult::Existing(task) if task.task_id == first_id
        ));

        let mut second = valid_task();
        second.idempotency_key = "runtime-test-002".to_owned();
        assert!(matches!(
            supervisor.submit(second),
            Err(SupervisorError::CapacityExceeded)
        ));
    }
}
