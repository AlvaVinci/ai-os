use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::time::Duration;

use aios_core::{
    CapabilityPolicy, CapabilityRequest, DenialReason, FileAccess, PolicyDecision,
    StateTransitionError, TaskSpec, TaskState, ValidationErrors,
};

use crate::{
    ApprovalAuthority, ApprovalError, ApprovalGrant, ApprovalId, ApprovalReceipt, ApprovalRequest,
    EventStore, EventStoreError, InMemoryEventStore, OperationId, TaskEvent, TaskEventKind, TaskId,
};

const DEFAULT_MAX_TASKS: usize = 10_000;

struct TaskRecord {
    spec: TaskSpec,
    state: TaskState,
}

#[derive(Eq, PartialEq)]
enum OwnedCapabilityRequest {
    File { path: String, access: FileAccess },
    Network { host: String },
    Tool { tool: String, action: String },
}

impl OwnedCapabilityRequest {
    fn from_borrowed(request: CapabilityRequest<'_>) -> Self {
        match request {
            CapabilityRequest::File { path, access } => Self::File {
                path: path.to_owned(),
                access,
            },
            CapabilityRequest::Network { host } => Self::Network {
                host: host.to_owned(),
            },
            CapabilityRequest::Tool { tool, action } => Self::Tool {
                tool: tool.to_owned(),
                action: action.to_owned(),
            },
        }
    }

    fn as_borrowed(&self) -> CapabilityRequest<'_> {
        match self {
            Self::File { path, access } => CapabilityRequest::File {
                path,
                access: *access,
            },
            Self::Network { host } => CapabilityRequest::Network { host },
            Self::Tool { tool, action } => CapabilityRequest::Tool { tool, action },
        }
    }

    fn action(&self) -> &str {
        match self {
            Self::File {
                access: FileAccess::Read,
                ..
            } => "filesystem.read",
            Self::File {
                access: FileAccess::Write,
                ..
            } => "filesystem.write",
            Self::Network { .. } => "network.egress",
            Self::Tool { action, .. } => action,
        }
    }
}

struct PendingOperation {
    task_id: TaskId,
    operation_id: OperationId,
    request: OwnedCapabilityRequest,
}

struct ApprovedOperation {
    task_id: TaskId,
    request: OwnedCapabilityRequest,
    grant: ApprovalGrant,
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

/// Resource-free result of evaluating an operation for one running task.
#[derive(Debug)]
pub enum OperationAuthorization {
    Allowed,
    Denied { reason: DenialReason },
    ApprovalRequired(ApprovalRequest),
}

/// Supervisor operation failure.
#[derive(Debug)]
pub enum SupervisorError {
    TaskNotFound,
    TaskNotRunning,
    IdempotencyConflict,
    CapacityExceeded,
    InvalidStateTransition(StateTransitionError),
    EventStore(EventStoreError),
    Approval(ApprovalError),
}

impl Display for SupervisorError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::TaskNotFound => formatter.write_str("task not found"),
            Self::TaskNotRunning => formatter.write_str("task is not running"),
            Self::IdempotencyConflict => formatter.write_str("idempotency key conflict"),
            Self::CapacityExceeded => formatter.write_str("task capacity exceeded"),
            Self::InvalidStateTransition(error) => Display::fmt(error, formatter),
            Self::EventStore(error) => Display::fmt(error, formatter),
            Self::Approval(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for SupervisorError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidStateTransition(error) => Some(error),
            Self::EventStore(error) => Some(error),
            Self::Approval(error) => Some(error),
            Self::TaskNotFound
            | Self::TaskNotRunning
            | Self::IdempotencyConflict
            | Self::CapacityExceeded => None,
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

impl From<ApprovalError> for SupervisorError {
    fn from(error: ApprovalError) -> Self {
        Self::Approval(error)
    }
}

/// Coordinates validated tasks and records every accepted state change.
pub struct TaskSupervisor<S = InMemoryEventStore> {
    tasks: BTreeMap<TaskId, TaskRecord>,
    idempotency_index: BTreeMap<String, TaskId>,
    event_store: S,
    approval_authority: ApprovalAuthority,
    pending_operations: BTreeMap<ApprovalId, PendingOperation>,
    approved_operations: BTreeMap<OperationId, ApprovedOperation>,
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
            approval_authority: ApprovalAuthority::default(),
            pending_operations: BTreeMap::new(),
            approved_operations: BTreeMap::new(),
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
            approval_authority: ApprovalAuthority::default(),
            pending_operations: BTreeMap::new(),
            approved_operations: BTreeMap::new(),
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

    /// Evaluates one exact operation and records the decision before returning it.
    pub fn request_operation(
        &mut self,
        task_id: TaskId,
        request: CapabilityRequest<'_>,
        approval_ttl: Duration,
    ) -> Result<OperationAuthorization, SupervisorError> {
        self.expire_approvals()?;
        let current = self
            .tasks
            .get(&task_id)
            .ok_or(SupervisorError::TaskNotFound)?
            .state;
        let mut proposed = current;
        proposed.transition_to(TaskState::WaitingApproval)?;

        let owned_request = OwnedCapabilityRequest::from_borrowed(request);
        let decision = {
            let record = self
                .tasks
                .get(&task_id)
                .ok_or(SupervisorError::TaskNotFound)?;
            CapabilityPolicy::from_task(&record.spec)
                .expect("stored task specifications must remain valid")
                .evaluate(owned_request.as_borrowed())
        };

        match decision {
            PolicyDecision::Allow => {
                self.event_store
                    .append_batch(task_id, &[TaskEventKind::OperationAllowed])?;
                Ok(OperationAuthorization::Allowed)
            }
            PolicyDecision::Deny { reason } => {
                self.event_store
                    .append_batch(task_id, &[TaskEventKind::OperationDenied { reason }])?;
                Ok(OperationAuthorization::Denied { reason })
            }
            PolicyDecision::ApprovalRequired => {
                if self
                    .pending_operations
                    .values()
                    .any(|pending| pending.task_id == task_id)
                {
                    return Err(ApprovalError::DuplicateOperation.into());
                }

                let operation_id = OperationId::new();
                let approval = self.approval_authority.request(
                    task_id,
                    operation_id,
                    owned_request.action(),
                    approval_ttl,
                )?;
                let event_kinds = [
                    TaskEventKind::ApprovalRequested {
                        approval_id: approval.approval_id,
                        operation_id,
                    },
                    TaskEventKind::StateTransitioned {
                        from: current,
                        to: TaskState::WaitingApproval,
                    },
                ];
                if let Err(error) = self.event_store.append_batch(task_id, &event_kinds) {
                    let _ = self.approval_authority.revoke(approval.approval_id);
                    return Err(error.into());
                }

                self.pending_operations.insert(
                    approval.approval_id,
                    PendingOperation {
                        task_id,
                        operation_id,
                        request: owned_request,
                    },
                );
                self.tasks
                    .get_mut(&task_id)
                    .ok_or(SupervisorError::TaskNotFound)?
                    .state = TaskState::WaitingApproval;
                Ok(OperationAuthorization::ApprovalRequired(approval))
            }
        }
    }

    /// Grants one pending operation and resumes its task only after audit persistence succeeds.
    pub fn approve_operation(
        &mut self,
        approval_id: ApprovalId,
    ) -> Result<TaskSnapshot, SupervisorError> {
        self.expire_approvals()?;
        let pending = self
            .pending_operations
            .get(&approval_id)
            .ok_or(ApprovalError::NotFound)?;
        let task_id = pending.task_id;
        let operation_id = pending.operation_id;
        let current = self
            .tasks
            .get(&task_id)
            .ok_or(SupervisorError::TaskNotFound)?
            .state;
        let mut proposed = current;
        proposed.transition_to(TaskState::Running)?;

        let grant = self.approval_authority.approve(approval_id)?;
        let event_kinds = [
            TaskEventKind::ApprovalGranted {
                approval_id,
                operation_id,
            },
            TaskEventKind::StateTransitioned {
                from: current,
                to: TaskState::Running,
            },
        ];
        if let Err(error) = self.event_store.append_batch(task_id, &event_kinds) {
            grant.restore(&mut self.approval_authority);
            return Err(error.into());
        }

        let pending = self
            .pending_operations
            .remove(&approval_id)
            .ok_or(ApprovalError::NotFound)?;
        self.approved_operations.insert(
            operation_id,
            ApprovedOperation {
                task_id,
                request: pending.request,
                grant,
            },
        );
        self.tasks
            .get_mut(&task_id)
            .ok_or(SupervisorError::TaskNotFound)?
            .state = TaskState::Running;
        Ok(TaskSnapshot {
            task_id,
            state: TaskState::Running,
        })
    }

    /// Denies one pending operation and fails its task.
    pub fn deny_operation(
        &mut self,
        approval_id: ApprovalId,
    ) -> Result<TaskSnapshot, SupervisorError> {
        self.expire_approvals()?;
        let pending = self
            .pending_operations
            .get(&approval_id)
            .ok_or(ApprovalError::NotFound)?;
        let task_id = pending.task_id;
        let operation_id = pending.operation_id;
        if !self.approval_authority.contains(approval_id) {
            return Err(ApprovalError::NotFound.into());
        }
        let current = self
            .tasks
            .get(&task_id)
            .ok_or(SupervisorError::TaskNotFound)?
            .state;
        let mut proposed = current;
        proposed.transition_to(TaskState::Failed)?;

        self.event_store.append_batch(
            task_id,
            &[
                TaskEventKind::ApprovalDenied {
                    approval_id,
                    operation_id,
                },
                TaskEventKind::StateTransitioned {
                    from: current,
                    to: TaskState::Failed,
                },
            ],
        )?;
        let _ = self.approval_authority.revoke(approval_id);
        self.pending_operations.remove(&approval_id);
        self.tasks
            .get_mut(&task_id)
            .ok_or(SupervisorError::TaskNotFound)?
            .state = TaskState::Failed;
        Ok(TaskSnapshot {
            task_id,
            state: TaskState::Failed,
        })
    }

    /// Consumes the approved operation only when the full resource request still matches.
    pub fn authorize_operation(
        &mut self,
        task_id: TaskId,
        operation_id: OperationId,
        request: CapabilityRequest<'_>,
    ) -> Result<ApprovalReceipt, SupervisorError> {
        let current = self
            .tasks
            .get(&task_id)
            .ok_or(SupervisorError::TaskNotFound)?
            .state;
        if current != TaskState::Running {
            return Err(SupervisorError::TaskNotRunning);
        }

        let supplied = OwnedCapabilityRequest::from_borrowed(request);
        let approved = self
            .approved_operations
            .remove(&operation_id)
            .ok_or(ApprovalError::NotFound)?;
        if approved.task_id != task_id || approved.request != supplied {
            return Err(ApprovalError::ScopeMismatch.into());
        }
        let decision = {
            let record = self
                .tasks
                .get(&task_id)
                .ok_or(SupervisorError::TaskNotFound)?;
            CapabilityPolicy::from_task(&record.spec)
                .expect("stored task specifications must remain valid")
                .evaluate(supplied.as_borrowed())
        };
        if decision != PolicyDecision::ApprovalRequired {
            return Err(ApprovalError::ScopeMismatch.into());
        }

        let approval_id = approved.grant.approval_id();
        let receipt = approved
            .grant
            .authorize(task_id, operation_id, supplied.action())?;
        self.event_store.append_batch(
            task_id,
            &[TaskEventKind::ApprovalConsumed {
                approval_id,
                operation_id,
            }],
        )?;
        Ok(receipt)
    }

    /// Expires pending requests and fails their waiting tasks after audit persistence succeeds.
    pub fn expire_approvals(&mut self) -> Result<usize, SupervisorError> {
        let expired = self.approval_authority.expired_ids();
        let mut expired_count = 0;
        for approval_id in expired {
            let Some(pending) = self.pending_operations.get(&approval_id) else {
                continue;
            };
            let task_id = pending.task_id;
            let operation_id = pending.operation_id;
            let current = self
                .tasks
                .get(&task_id)
                .ok_or(SupervisorError::TaskNotFound)?
                .state;
            let mut proposed = current;
            proposed.transition_to(TaskState::Failed)?;
            self.event_store.append_batch(
                task_id,
                &[
                    TaskEventKind::ApprovalExpired {
                        approval_id,
                        operation_id,
                    },
                    TaskEventKind::StateTransitioned {
                        from: current,
                        to: TaskState::Failed,
                    },
                ],
            )?;
            let _ = self.approval_authority.revoke(approval_id);
            self.pending_operations.remove(&approval_id);
            self.tasks
                .get_mut(&task_id)
                .ok_or(SupervisorError::TaskNotFound)?
                .state = TaskState::Failed;
            expired_count += 1;
        }
        Ok(expired_count)
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
        let mut event_kinds = Vec::new();
        if next.is_terminal() {
            event_kinds.extend(self.revocation_events(task_id));
        }
        event_kinds.push(TaskEventKind::StateTransitioned {
            from: current,
            to: next,
        });
        self.event_store.append_batch(task_id, &event_kinds)?;

        if next.is_terminal() {
            self.invalidate_approvals(task_id);
        }

        let record = self
            .tasks
            .get_mut(&task_id)
            .ok_or(SupervisorError::TaskNotFound)?;
        record.state = next;
        Ok(())
    }

    fn revocation_events(&self, task_id: TaskId) -> Vec<TaskEventKind> {
        let pending = self
            .pending_operations
            .iter()
            .filter_map(|(approval_id, operation)| {
                (operation.task_id == task_id).then_some(TaskEventKind::ApprovalRevoked {
                    approval_id: *approval_id,
                    operation_id: operation.operation_id,
                })
            });
        let approved = self
            .approved_operations
            .iter()
            .filter_map(|(operation_id, operation)| {
                (operation.task_id == task_id).then_some(TaskEventKind::ApprovalRevoked {
                    approval_id: operation.grant.approval_id(),
                    operation_id: *operation_id,
                })
            });
        pending.chain(approved).collect()
    }

    fn invalidate_approvals(&mut self, task_id: TaskId) {
        let pending_ids: Vec<ApprovalId> = self
            .pending_operations
            .iter()
            .filter_map(|(approval_id, operation)| {
                (operation.task_id == task_id).then_some(*approval_id)
            })
            .collect();
        for approval_id in pending_ids {
            let _ = self.approval_authority.revoke(approval_id);
            self.pending_operations.remove(&approval_id);
        }
        self.approved_operations
            .retain(|_, operation| operation.task_id != task_id);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use aios_core::{
        ApprovalPolicy, Budget, CapabilityRequest, CapabilitySet, FileAccess, FileCapability,
        NetworkPolicy, TaskSpec, TaskState,
    };

    use super::{OperationAuthorization, SubmitResult, SupervisorError, TaskSupervisor};
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
        let authorization = supervisor
            .request_operation(
                task_id,
                CapabilityRequest::Tool {
                    tool: "test_runner",
                    action: "git.commit",
                },
                Duration::from_secs(30),
            )
            .expect("request operation");
        let OperationAuthorization::ApprovalRequired(request) = authorization else {
            panic!("expected approval request");
        };
        supervisor
            .approve_operation(request.approval_id)
            .expect("approve operation");
        supervisor
            .authorize_operation(
                task_id,
                request.operation_id,
                CapabilityRequest::Tool {
                    tool: "test_runner",
                    action: "git.commit",
                },
            )
            .expect("consume approval");
        supervisor.succeed(task_id).expect("succeed task");

        assert_eq!(
            supervisor.get(task_id).expect("task exists").state,
            TaskState::Succeeded
        );
        assert_eq!(supervisor.events(task_id, 0).expect("events").len(), 10);
    }

    #[test]
    fn denial_fails_task_and_cannot_be_reused() {
        let mut supervisor = TaskSupervisor::default();
        let task_id = accepted_task_id(supervisor.submit(valid_task()).expect("submit task"));
        supervisor.start(task_id).expect("start task");
        let OperationAuthorization::ApprovalRequired(request) = supervisor
            .request_operation(
                task_id,
                CapabilityRequest::Tool {
                    tool: "test_runner",
                    action: "git.commit",
                },
                Duration::from_secs(30),
            )
            .expect("request operation")
        else {
            panic!("expected approval request");
        };

        let task = supervisor
            .deny_operation(request.approval_id)
            .expect("deny operation");

        assert_eq!(task.state, TaskState::Failed);
        assert!(matches!(
            supervisor.approve_operation(request.approval_id),
            Err(SupervisorError::Approval(crate::ApprovalError::NotFound))
        ));
    }

    #[test]
    fn exact_resource_mismatch_consumes_linear_grant() {
        let mut spec = valid_task();
        spec.capabilities.filesystem[0].access = FileAccess::Write;
        spec.approval.required_for = vec!["filesystem.write".to_owned()];
        let mut supervisor = TaskSupervisor::default();
        let task_id = accepted_task_id(supervisor.submit(spec).expect("submit task"));
        supervisor.start(task_id).expect("start task");
        let OperationAuthorization::ApprovalRequired(request) = supervisor
            .request_operation(
                task_id,
                CapabilityRequest::File {
                    path: "/workspace/project/a.txt",
                    access: FileAccess::Write,
                },
                Duration::from_secs(30),
            )
            .expect("request operation")
        else {
            panic!("expected approval request");
        };
        supervisor
            .approve_operation(request.approval_id)
            .expect("approve operation");

        assert!(matches!(
            supervisor.authorize_operation(
                task_id,
                request.operation_id,
                CapabilityRequest::File {
                    path: "/workspace/project/b.txt",
                    access: FileAccess::Write,
                },
            ),
            Err(SupervisorError::Approval(
                crate::ApprovalError::ScopeMismatch
            ))
        ));
        assert!(matches!(
            supervisor.authorize_operation(
                task_id,
                request.operation_id,
                CapabilityRequest::File {
                    path: "/workspace/project/a.txt",
                    access: FileAccess::Write,
                },
            ),
            Err(SupervisorError::Approval(crate::ApprovalError::NotFound))
        ));
    }

    #[test]
    fn cancellation_revokes_pending_approval() {
        let mut supervisor = TaskSupervisor::default();
        let task_id = accepted_task_id(supervisor.submit(valid_task()).expect("submit task"));
        supervisor.start(task_id).expect("start task");
        let OperationAuthorization::ApprovalRequired(request) = supervisor
            .request_operation(
                task_id,
                CapabilityRequest::Tool {
                    tool: "test_runner",
                    action: "git.commit",
                },
                Duration::from_secs(30),
            )
            .expect("request operation")
        else {
            panic!("expected approval request");
        };

        assert!(supervisor.cancel(task_id).expect("cancel task"));
        assert!(matches!(
            supervisor.approve_operation(request.approval_id),
            Err(SupervisorError::Approval(crate::ApprovalError::NotFound))
        ));
        assert!(
            supervisor
                .events(task_id, 0)
                .expect("events")
                .iter()
                .any(|event| matches!(event.kind, TaskEventKind::ApprovalRevoked { .. }))
        );
    }

    #[test]
    fn approval_request_rolls_back_when_audit_batch_fails() {
        let store = InMemoryEventStore::new(5).expect("positive capacity");
        let mut supervisor = TaskSupervisor::new(store);
        let task_id = accepted_task_id(supervisor.submit(valid_task()).expect("submit task"));
        supervisor.start(task_id).expect("start task");

        for _ in 0..2 {
            assert!(matches!(
                supervisor.request_operation(
                    task_id,
                    CapabilityRequest::Tool {
                        tool: "test_runner",
                        action: "git.commit",
                    },
                    Duration::from_secs(30),
                ),
                Err(SupervisorError::EventStore(_))
            ));
        }
        assert_eq!(
            supervisor.get(task_id).expect("task exists").state,
            TaskState::Running
        );
    }

    #[test]
    fn approval_grant_rolls_back_when_audit_batch_fails() {
        let store = InMemoryEventStore::new(7).expect("positive capacity");
        let mut supervisor = TaskSupervisor::new(store);
        let task_id = accepted_task_id(supervisor.submit(valid_task()).expect("submit task"));
        supervisor.start(task_id).expect("start task");
        let OperationAuthorization::ApprovalRequired(request) = supervisor
            .request_operation(
                task_id,
                CapabilityRequest::Tool {
                    tool: "test_runner",
                    action: "git.commit",
                },
                Duration::from_secs(30),
            )
            .expect("request operation")
        else {
            panic!("expected approval request");
        };

        for _ in 0..2 {
            assert!(matches!(
                supervisor.approve_operation(request.approval_id),
                Err(SupervisorError::EventStore(_))
            ));
        }
        assert_eq!(
            supervisor.get(task_id).expect("task exists").state,
            TaskState::WaitingApproval
        );
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
