use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::time::Duration;

use aios_core::{CapabilityRequest, DenialReason};

use crate::{
    ApprovalId, ApprovalReceipt, ApprovalRequest, EventStore, OperationAuthorization, OperationId,
    SupervisorError, TaskId, TaskSnapshot, TaskSupervisor,
};

/// One adapter-owned operation whose capability request is constructed by trusted code.
///
/// Implementations intentionally should not expose model-selected identifiers as trusted
/// capability fields. The complete operation value is retained by [`ExecutionGate`] while human
/// approval is pending.
pub trait GuardedOperation {
    fn capability_request(&self) -> CapabilityRequest<'_>;
}

/// Executes one complete operation value without shell or prompt interpretation.
pub trait ExecutionAdapter<O: GuardedOperation> {
    type Output;
    type Error;

    fn execute(&mut self, operation: O) -> Result<Self::Output, Self::Error>;
}

/// Successful execution metadata returned after the adapter completes.
pub struct Executed<T> {
    pub output: T,
    pub approval: Option<ApprovalReceipt>,
}

/// Result of submitting one operation to the guarded adapter boundary.
pub enum ExecutionOutcome<T> {
    Executed(Executed<T>),
    Denied { reason: DenialReason },
    ApprovalRequired(ApprovalRequest),
}

/// Failure before or during guarded adapter execution.
pub enum ExecutionError<E> {
    Supervisor(SupervisorError),
    Adapter(E),
    OperationNotFound,
}

impl<E> fmt::Debug for ExecutionError<E> {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(self, formatter)
    }
}

impl<E> Display for ExecutionError<E> {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Supervisor(_) => "operation authorization failed",
            Self::Adapter(_) => "adapter execution failed",
            Self::OperationNotFound => "pending operation not found",
        };
        formatter.write_str(message)
    }
}

impl<E> Error for ExecutionError<E> {}

impl<E> From<SupervisorError> for ExecutionError<E> {
    fn from(error: SupervisorError) -> Self {
        Self::Supervisor(error)
    }
}

struct PendingExecution<O> {
    task_id: TaskId,
    approval_id: ApprovalId,
    operation: O,
}

/// Keeps complete operations private and invokes an adapter only after runtime authorization.
///
/// The gate deliberately exposes no reference to its adapter. Pending operation count is bounded
/// by the supervisor's approval authority.
pub struct ExecutionGate<A, O> {
    adapter: A,
    pending: BTreeMap<OperationId, PendingExecution<O>>,
    approval_index: BTreeMap<ApprovalId, OperationId>,
}

impl<A, O> ExecutionGate<A, O>
where
    O: GuardedOperation,
    A: ExecutionAdapter<O>,
{
    #[must_use]
    pub fn new(adapter: A) -> Self {
        Self {
            adapter,
            pending: BTreeMap::new(),
            approval_index: BTreeMap::new(),
        }
    }

    /// Authorizes an operation and executes it immediately only when no approval is required.
    pub fn request<S: EventStore>(
        &mut self,
        supervisor: &mut TaskSupervisor<S>,
        task_id: TaskId,
        operation: O,
        approval_ttl: Duration,
    ) -> Result<ExecutionOutcome<A::Output>, ExecutionError<A::Error>> {
        let authorization =
            supervisor.request_operation(task_id, operation.capability_request(), approval_ttl)?;

        match authorization {
            OperationAuthorization::Allowed => self
                .adapter
                .execute(operation)
                .map(|output| {
                    ExecutionOutcome::Executed(Executed {
                        output,
                        approval: None,
                    })
                })
                .map_err(ExecutionError::Adapter),
            OperationAuthorization::Denied { reason } => Ok(ExecutionOutcome::Denied { reason }),
            OperationAuthorization::ApprovalRequired(approval) => {
                let previous = self.pending.insert(
                    approval.operation_id,
                    PendingExecution {
                        task_id,
                        approval_id: approval.approval_id,
                        operation,
                    },
                );
                debug_assert!(previous.is_none(), "operation identifiers must be unique");
                self.approval_index
                    .insert(approval.approval_id, approval.operation_id);
                Ok(ExecutionOutcome::ApprovalRequired(approval))
            }
        }
    }

    /// Approves, consumes, and executes the exact operation retained by the gate.
    pub fn approve_and_execute<S: EventStore>(
        &mut self,
        supervisor: &mut TaskSupervisor<S>,
        approval_id: ApprovalId,
    ) -> Result<Executed<A::Output>, ExecutionError<A::Error>> {
        let operation_id = self
            .approval_index
            .get(&approval_id)
            .copied()
            .ok_or(ExecutionError::OperationNotFound)?;
        if !self.pending.contains_key(&operation_id) {
            return Err(ExecutionError::OperationNotFound);
        }

        supervisor.approve_operation(approval_id)?;
        let pending = self
            .remove_pending(approval_id, operation_id)
            .ok_or(ExecutionError::OperationNotFound)?;
        let approval = supervisor.authorize_operation(
            pending.task_id,
            operation_id,
            pending.operation.capability_request(),
        )?;
        self.adapter
            .execute(pending.operation)
            .map(|output| Executed {
                output,
                approval: Some(approval),
            })
            .map_err(ExecutionError::Adapter)
    }

    /// Denies a pending operation and removes its complete value only after audit persistence.
    pub fn deny<S: EventStore>(
        &mut self,
        supervisor: &mut TaskSupervisor<S>,
        approval_id: ApprovalId,
    ) -> Result<TaskSnapshot, ExecutionError<A::Error>> {
        let operation_id = self
            .approval_index
            .get(&approval_id)
            .copied()
            .ok_or(ExecutionError::OperationNotFound)?;
        let task = supervisor.deny_operation(approval_id)?;
        let _ = self.remove_pending(approval_id, operation_id);
        Ok(task)
    }

    /// Expires runtime approvals and drops complete operations for newly terminal Tasks.
    pub fn expire<S: EventStore>(
        &mut self,
        supervisor: &mut TaskSupervisor<S>,
    ) -> Result<usize, ExecutionError<A::Error>> {
        let expired = supervisor.expire_approvals()?;
        self.purge_terminal(supervisor);
        Ok(expired)
    }

    /// Cancels a Task and drops its retained operations after the runtime invalidates approvals.
    pub fn cancel<S: EventStore>(
        &mut self,
        supervisor: &mut TaskSupervisor<S>,
        task_id: TaskId,
    ) -> Result<bool, ExecutionError<A::Error>> {
        let changed = supervisor.cancel(task_id)?;
        self.remove_task(task_id);
        Ok(changed)
    }

    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    fn remove_pending(
        &mut self,
        approval_id: ApprovalId,
        operation_id: OperationId,
    ) -> Option<PendingExecution<O>> {
        self.approval_index.remove(&approval_id);
        self.pending.remove(&operation_id)
    }

    fn remove_task(&mut self, task_id: TaskId) {
        let operation_ids: Vec<OperationId> = self
            .pending
            .iter()
            .filter_map(|(operation_id, pending)| {
                (pending.task_id == task_id).then_some(*operation_id)
            })
            .collect();
        for operation_id in operation_ids {
            if let Some(pending) = self.pending.remove(&operation_id) {
                self.approval_index.remove(&pending.approval_id);
            }
        }
    }

    fn purge_terminal<S: EventStore>(&mut self, supervisor: &TaskSupervisor<S>) {
        let task_ids: Vec<TaskId> = self
            .pending
            .values()
            .filter_map(|pending| {
                supervisor
                    .get(pending.task_id)
                    .is_some_and(|task| task.state.is_terminal())
                    .then_some(pending.task_id)
            })
            .collect();
        for task_id in task_ids {
            self.remove_task(task_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use aios_core::{
        ApprovalPolicy, Budget, CapabilityRequest, CapabilitySet, NetworkPolicy, TaskSpec,
        TaskState,
    };

    use super::{
        Executed, ExecutionAdapter, ExecutionError, ExecutionGate, ExecutionOutcome,
        GuardedOperation,
    };
    use crate::{InMemoryEventStore, SubmitResult, TaskId, TaskSupervisor};

    struct ToolOperation {
        tool: String,
        action: String,
        argument: String,
    }

    impl GuardedOperation for ToolOperation {
        fn capability_request(&self) -> CapabilityRequest<'_> {
            CapabilityRequest::Tool {
                tool: &self.tool,
                action: &self.action,
            }
        }
    }

    #[derive(Default)]
    struct RecordingAdapter {
        executed_arguments: Vec<String>,
        fail: bool,
    }

    impl ExecutionAdapter<ToolOperation> for RecordingAdapter {
        type Output = usize;
        type Error = &'static str;

        fn execute(&mut self, operation: ToolOperation) -> Result<Self::Output, Self::Error> {
            if self.fail {
                return Err("sensitive adapter detail");
            }
            self.executed_arguments.push(operation.argument);
            Ok(self.executed_arguments.len())
        }
    }

    fn task(required_for: &[&str]) -> TaskSpec {
        TaskSpec {
            idempotency_key: "execution-gate-test".to_owned(),
            goal: "Run one guarded tool operation".to_owned(),
            capabilities: CapabilitySet {
                filesystem: Vec::new(),
                network: NetworkPolicy::Deny,
                tools: vec!["test_runner".to_owned()],
            },
            budget: Budget {
                wall_time_seconds: 60,
                memory_bytes: 64 * 1024 * 1024,
                max_parallel_agents: 1,
            },
            approval: ApprovalPolicy {
                required_for: required_for
                    .iter()
                    .map(|action| (*action).to_owned())
                    .collect(),
            },
        }
    }

    fn running_supervisor(required_for: &[&str]) -> (TaskSupervisor, TaskId) {
        let mut supervisor = TaskSupervisor::default();
        let result = supervisor.submit(task(required_for)).expect("submit task");
        let SubmitResult::Accepted(task) = result else {
            panic!("expected accepted task");
        };
        supervisor.start(task.task_id).expect("start task");
        (supervisor, task.task_id)
    }

    fn operation(action: &str, argument: &str) -> ToolOperation {
        ToolOperation {
            tool: "test_runner".to_owned(),
            action: action.to_owned(),
            argument: argument.to_owned(),
        }
    }

    #[test]
    fn executes_allowed_operation_immediately() {
        let (mut supervisor, task_id) = running_supervisor(&[]);
        let mut gate = ExecutionGate::new(RecordingAdapter::default());

        let result = gate
            .request(
                &mut supervisor,
                task_id,
                operation("test.run", "original"),
                Duration::from_secs(30),
            )
            .expect("execute allowed operation");

        let ExecutionOutcome::Executed(Executed { output, approval }) = result else {
            panic!("expected execution");
        };
        assert_eq!(output, 1);
        assert!(approval.is_none());
        assert_eq!(gate.pending_count(), 0);
    }

    #[test]
    fn denied_operation_never_reaches_adapter() {
        let (mut supervisor, task_id) = running_supervisor(&[]);
        let mut gate = ExecutionGate::new(RecordingAdapter::default());

        let result = gate
            .request(
                &mut supervisor,
                task_id,
                ToolOperation {
                    tool: "untrusted_tool".to_owned(),
                    action: "test.run".to_owned(),
                    argument: "must-not-run".to_owned(),
                },
                Duration::from_secs(30),
            )
            .expect("deny operation");

        assert!(matches!(result, ExecutionOutcome::Denied { .. }));
        assert_eq!(gate.pending_count(), 0);
    }

    #[test]
    fn approval_executes_the_retained_complete_operation_once() {
        let (mut supervisor, task_id) = running_supervisor(&["tool.execute"]);
        let mut gate = ExecutionGate::new(RecordingAdapter::default());
        let result = gate
            .request(
                &mut supervisor,
                task_id,
                operation("tool.execute", "approved-argument"),
                Duration::from_secs(30),
            )
            .expect("request approval");
        let ExecutionOutcome::ApprovalRequired(request) = result else {
            panic!("expected approval request");
        };

        let executed = gate
            .approve_and_execute(&mut supervisor, request.approval_id)
            .expect("approve and execute");

        assert_eq!(executed.output, 1);
        assert_eq!(
            executed.approval.expect("approval receipt").approval_id,
            request.approval_id
        );
        assert_eq!(gate.pending_count(), 0);
        assert!(matches!(
            gate.approve_and_execute(&mut supervisor, request.approval_id),
            Err(ExecutionError::OperationNotFound)
        ));
    }

    #[test]
    fn denial_and_cancellation_drop_retained_operations() {
        let (mut supervisor, task_id) = running_supervisor(&["tool.execute"]);
        let mut gate = ExecutionGate::new(RecordingAdapter::default());
        let ExecutionOutcome::ApprovalRequired(request) = gate
            .request(
                &mut supervisor,
                task_id,
                operation("tool.execute", "denied"),
                Duration::from_secs(30),
            )
            .expect("request approval")
        else {
            panic!("expected approval request");
        };

        let task = gate
            .deny(&mut supervisor, request.approval_id)
            .expect("deny operation");

        assert_eq!(task.state, TaskState::Failed);
        assert_eq!(gate.pending_count(), 0);

        let (mut supervisor, task_id) = running_supervisor(&["tool.execute"]);
        let mut gate = ExecutionGate::new(RecordingAdapter::default());
        let _ = gate
            .request(
                &mut supervisor,
                task_id,
                operation("tool.execute", "cancelled"),
                Duration::from_secs(30),
            )
            .expect("request approval");
        assert!(gate.cancel(&mut supervisor, task_id).expect("cancel task"));
        assert_eq!(gate.pending_count(), 0);
    }

    #[test]
    fn audit_failure_does_not_retain_or_execute_operation() {
        let store = InMemoryEventStore::new(5).expect("positive capacity");
        let mut supervisor = TaskSupervisor::new(store);
        let SubmitResult::Accepted(task) = supervisor
            .submit(task(&["tool.execute"]))
            .expect("submit task")
        else {
            panic!("expected accepted task");
        };
        supervisor.start(task.task_id).expect("start task");
        let mut gate = ExecutionGate::new(RecordingAdapter::default());

        let result = gate.request(
            &mut supervisor,
            task.task_id,
            operation("tool.execute", "must-not-run"),
            Duration::from_secs(30),
        );

        assert!(matches!(result, Err(ExecutionError::Supervisor(_))));
        assert_eq!(gate.pending_count(), 0);
    }

    #[test]
    fn approval_audit_failure_preserves_pending_operation() {
        let store = InMemoryEventStore::new(7).expect("positive capacity");
        let mut supervisor = TaskSupervisor::new(store);
        let SubmitResult::Accepted(task) = supervisor
            .submit(task(&["tool.execute"]))
            .expect("submit task")
        else {
            panic!("expected accepted task");
        };
        supervisor.start(task.task_id).expect("start task");
        let mut gate = ExecutionGate::new(RecordingAdapter::default());
        let ExecutionOutcome::ApprovalRequired(request) = gate
            .request(
                &mut supervisor,
                task.task_id,
                operation("tool.execute", "retained"),
                Duration::from_secs(30),
            )
            .expect("request approval")
        else {
            panic!("expected approval request");
        };

        assert!(matches!(
            gate.approve_and_execute(&mut supervisor, request.approval_id),
            Err(ExecutionError::Supervisor(_))
        ));
        assert_eq!(gate.pending_count(), 1);
        assert_eq!(
            supervisor.get(task.task_id).expect("task exists").state,
            TaskState::WaitingApproval
        );
    }

    #[test]
    fn expiration_drops_retained_operation() {
        let (mut supervisor, task_id) = running_supervisor(&["tool.execute"]);
        let mut gate = ExecutionGate::new(RecordingAdapter::default());
        let _ = gate
            .request(
                &mut supervisor,
                task_id,
                operation("tool.execute", "expired"),
                Duration::from_millis(1),
            )
            .expect("request approval");
        std::thread::sleep(Duration::from_millis(5));

        assert_eq!(gate.expire(&mut supervisor).expect("expire approval"), 1);
        assert_eq!(gate.pending_count(), 0);
        assert_eq!(
            supervisor.get(task_id).expect("task exists").state,
            TaskState::Failed
        );
    }

    #[test]
    fn adapter_errors_are_redacted_and_cannot_be_replayed() {
        let (mut supervisor, task_id) = running_supervisor(&["tool.execute"]);
        let mut gate = ExecutionGate::new(RecordingAdapter {
            executed_arguments: Vec::new(),
            fail: true,
        });
        let ExecutionOutcome::ApprovalRequired(request) = gate
            .request(
                &mut supervisor,
                task_id,
                operation("tool.execute", "secret-value"),
                Duration::from_secs(30),
            )
            .expect("request approval")
        else {
            panic!("expected approval request");
        };

        let Err(error) = gate.approve_and_execute(&mut supervisor, request.approval_id) else {
            panic!("adapter must fail");
        };

        assert_eq!(error.to_string(), "adapter execution failed");
        assert!(!error.to_string().contains("sensitive"));
        assert_eq!(format!("{error:?}"), "adapter execution failed");
        assert!(matches!(
            gate.approve_and_execute(&mut supervisor, request.approval_id),
            Err(ExecutionError::OperationNotFound)
        ));
    }
}
