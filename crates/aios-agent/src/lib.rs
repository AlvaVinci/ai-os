//! Bounded Agent execution and model adapter contracts for AI OS.
//!
//! Model decisions are untrusted proposals. [`AgentRuntime`] exposes only Task-granted operation
//! scopes, prepares operations through trusted catalogs, and executes them through Capability and
//! approval gates.

use std::collections::VecDeque;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::time::Duration;

use aios_adapter_network::{
    MAX_NETWORK_HOST_BYTES, MAX_TCP_REQUEST_BYTES, NetworkAdapterError, NetworkCatalog,
    NetworkExecutionGate,
};
use aios_adapter_tool::{
    MAX_ARGUMENT_BYTES, MAX_ARGUMENTS, MAX_IDENTIFIER_BYTES, MAX_TOTAL_ARGUMENT_BYTES,
    ToolAdapterError, ToolCatalog, ToolExecutionGate,
};
use aios_core::{DenialReason, ErrorCode, NetworkDestination};
use aios_runtime::{
    ApprovalId, ApprovalRequest, EventStore, ExecutionError, ExecutionOutcome, SupervisorError,
    TaskId, TaskSupervisor,
};

pub const DEFAULT_MAX_MODEL_STEPS: u16 = 16;
pub const MAX_MODEL_STEPS: u16 = 64;
pub const MAX_FINAL_OUTPUT_BYTES: usize = 1024 * 1024;
const DEFAULT_APPROVAL_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_APPROVAL_TTL: Duration = Duration::from_secs(15 * 60);

/// Trusted runtime limits that a model cannot change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AgentConfig {
    max_steps: u16,
    approval_ttl: Duration,
}

impl AgentConfig {
    pub fn new(max_steps: u16, approval_ttl: Duration) -> Result<Self, AgentError> {
        if max_steps == 0
            || max_steps > MAX_MODEL_STEPS
            || approval_ttl < Duration::from_millis(1)
            || approval_ttl > MAX_APPROVAL_TTL
        {
            return Err(AgentError::InvalidConfig);
        }
        Ok(Self {
            max_steps,
            approval_ttl,
        })
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_steps: DEFAULT_MAX_MODEL_STEPS,
            approval_ttl: DEFAULT_APPROVAL_TTL,
        }
    }
}

/// Sensitive input used to create one isolated model session.
///
/// This type intentionally does not implement `Debug` or serialization.
pub struct ModelStartRequest<'a> {
    goal: &'a str,
    tool_routes: &'a [&'a str],
    network_destinations: &'a [NetworkDestination],
}

impl<'a> ModelStartRequest<'a> {
    #[must_use]
    pub fn goal(&self) -> &str {
        self.goal
    }

    #[must_use]
    pub fn tool_routes(&self) -> &[&str] {
        self.tool_routes
    }

    #[must_use]
    pub fn network_destinations(&self) -> &[NetworkDestination] {
        self.network_destinations
    }
}

/// Sensitive input for one bounded model turn.
///
/// Only the immediately preceding operation output is exposed. This type intentionally does not
/// implement `Debug` or serialization.
pub struct ModelTurnRequest<'a> {
    step: u16,
    previous_operation_output: Option<&'a [u8]>,
}

impl<'a> ModelTurnRequest<'a> {
    #[must_use]
    pub const fn step(&self) -> u16 {
        self.step
    }

    #[must_use]
    pub const fn previous_tool_output(&self) -> Option<&[u8]> {
        self.previous_operation_output
    }

    #[must_use]
    pub const fn previous_operation_output(&self) -> Option<&[u8]> {
        self.previous_operation_output
    }
}

/// Creates isolated model sessions for individual Tasks.
pub trait ModelAdapter {
    type Error;
    type Session: ModelSession<Error = Self::Error>;

    fn start_session(
        &mut self,
        request: ModelStartRequest<'_>,
    ) -> Result<Self::Session, Self::Error>;
}

/// One Task-scoped model conversation.
pub trait ModelSession {
    type Error;

    fn decide(&mut self, request: ModelTurnRequest<'_>) -> Result<ModelDecision, Self::Error>;
}

/// Bounded final user-facing output.
///
/// It intentionally does not implement `Debug`, `Clone`, or serialization.
pub struct AgentOutput(String);

impl AgentOutput {
    pub fn from_text(text: String) -> Result<Self, ModelDecisionError> {
        if text.is_empty() || text.len() > MAX_FINAL_OUTPUT_BYTES || text.contains('\0') {
            return Err(ModelDecisionError::InvalidFinalOutput);
        }
        Ok(Self(text))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

enum ModelDecisionKind {
    Finish(AgentOutput),
    CallTool {
        route: String,
        arguments: Vec<String>,
    },
    TcpExchange {
        host: String,
        port: u16,
        request: Vec<u8>,
    },
}

/// Validated model proposal. Capability and approval identifiers are never model-controlled.
///
/// This type intentionally does not implement `Debug`, `Clone`, or serialization.
pub struct ModelDecision {
    kind: ModelDecisionKind,
}

impl ModelDecision {
    pub fn finish(text: String) -> Result<Self, ModelDecisionError> {
        Ok(Self {
            kind: ModelDecisionKind::Finish(AgentOutput::from_text(text)?),
        })
    }

    pub fn call_tool(route: String, arguments: Vec<String>) -> Result<Self, ModelDecisionError> {
        if !is_valid_identifier(&route) || !are_valid_arguments(&arguments) {
            return Err(ModelDecisionError::InvalidToolRequest);
        }
        Ok(Self {
            kind: ModelDecisionKind::CallTool { route, arguments },
        })
    }

    pub fn tcp_exchange(
        host: String,
        port: u16,
        request: Vec<u8>,
    ) -> Result<Self, ModelDecisionError> {
        if host.is_empty()
            || host.len() > MAX_NETWORK_HOST_BYTES
            || port == 0
            || request.len() > MAX_TCP_REQUEST_BYTES
        {
            return Err(ModelDecisionError::InvalidNetworkRequest);
        }
        Ok(Self {
            kind: ModelDecisionKind::TcpExchange {
                host,
                port,
                request,
            },
        })
    }
}

/// Stable validation category for one untrusted model proposal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelDecisionError {
    InvalidFinalOutput,
    InvalidToolRequest,
    InvalidNetworkRequest,
}

impl Display for ModelDecisionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidFinalOutput => "invalid final model output",
            Self::InvalidToolRequest => "invalid model Tool request",
            Self::InvalidNetworkRequest => "invalid model Network request",
        };
        formatter.write_str(message)
    }
}

impl Error for ModelDecisionError {}

/// Deterministic adapter that supplies one pre-validated Task-scoped decision sequence.
///
/// It is intended for conformance tests and does not perform inference.
pub struct ScriptedModelAdapter {
    script: Option<VecDeque<ModelDecision>>,
}

impl ScriptedModelAdapter {
    pub fn new(decisions: Vec<ModelDecision>) -> Result<Self, ScriptedModelError> {
        if decisions.is_empty() || decisions.len() > usize::from(MAX_MODEL_STEPS) {
            return Err(ScriptedModelError::InvalidScript);
        }
        Ok(Self {
            script: Some(decisions.into()),
        })
    }
}

pub struct ScriptedModelSession {
    decisions: VecDeque<ModelDecision>,
}

impl ModelAdapter for ScriptedModelAdapter {
    type Error = ScriptedModelError;
    type Session = ScriptedModelSession;

    fn start_session(
        &mut self,
        _request: ModelStartRequest<'_>,
    ) -> Result<Self::Session, Self::Error> {
        self.script
            .take()
            .map(|decisions| ScriptedModelSession { decisions })
            .ok_or(ScriptedModelError::Unavailable)
    }
}

impl ModelSession for ScriptedModelSession {
    type Error = ScriptedModelError;

    fn decide(&mut self, _request: ModelTurnRequest<'_>) -> Result<ModelDecision, Self::Error> {
        self.decisions
            .pop_front()
            .ok_or(ScriptedModelError::Exhausted)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScriptedModelError {
    InvalidScript,
    Unavailable,
    Exhausted,
}

impl Display for ScriptedModelError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str("scripted model adapter failed")
    }
}

impl Error for ScriptedModelError {}

struct ActiveSession<S> {
    task_id: TaskId,
    model: S,
    next_step: u16,
    previous_operation_output: Option<Vec<u8>>,
    pending_approval: Option<PendingApproval>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum PendingAdapter {
    Tool,
    Network,
}

#[derive(Clone, Copy)]
struct PendingApproval {
    approval_id: ApprovalId,
    adapter: PendingAdapter,
}

struct AgentNetwork {
    catalog: NetworkCatalog,
    gate: NetworkExecutionGate,
}

/// Synchronous single-Task Agent runtime with no raw Tool adapter escape hatch.
pub struct AgentRuntime<M: ModelAdapter> {
    model_adapter: M,
    tool_catalog: ToolCatalog,
    tool_gate: ToolExecutionGate,
    network: Option<AgentNetwork>,
    config: AgentConfig,
    active: Option<ActiveSession<M::Session>>,
}

impl<M: ModelAdapter> AgentRuntime<M> {
    #[must_use]
    pub fn new(
        model_adapter: M,
        tool_catalog: ToolCatalog,
        tool_gate: ToolExecutionGate,
        config: AgentConfig,
    ) -> Self {
        Self {
            model_adapter,
            tool_catalog,
            tool_gate,
            network: None,
            config,
            active: None,
        }
    }

    /// Enables bounded direct TCP proposals through the Network Capability gate.
    #[must_use]
    pub fn with_network_gate(mut self, gate: NetworkExecutionGate) -> Self {
        self.network = Some(AgentNetwork {
            catalog: NetworkCatalog::new(),
            gate,
        });
        self
    }

    /// Starts one queued Task and drives it until completion, denial, or approval wait.
    pub fn start<S: EventStore>(
        &mut self,
        supervisor: &mut TaskSupervisor<S>,
        task_id: TaskId,
    ) -> Result<AgentRunOutcome, AgentError> {
        if self.active.is_some() {
            return Err(AgentError::CapacityExceeded);
        }

        let input = supervisor.start_execution(task_id)?;
        let routes: Vec<&str> = self
            .tool_catalog
            .route_names_for_tools(input.capability_tools())
            .collect();
        let network_destinations: Vec<NetworkDestination> = self
            .network
            .as_ref()
            .map(|network| {
                input
                    .network_destinations()
                    .iter()
                    .filter(|destination| {
                        network
                            .catalog
                            .prepare_tcp_exchange(
                                destination.host.clone(),
                                destination.port,
                                Vec::new(),
                            )
                            .is_ok()
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let model = match self.model_adapter.start_session(ModelStartRequest {
            goal: input.goal(),
            tool_routes: &routes,
            network_destinations: &network_destinations,
        }) {
            Ok(model) => model,
            Err(_) => {
                supervisor.fail(task_id)?;
                return Err(AgentError::ModelFailed);
            }
        };
        self.active = Some(ActiveSession {
            task_id,
            model,
            next_step: 1,
            previous_operation_output: None,
            pending_approval: None,
        });
        self.drive(supervisor)
    }

    /// Consumes one exact approval and resumes the retained model session.
    pub fn approve_and_resume<S: EventStore>(
        &mut self,
        supervisor: &mut TaskSupervisor<S>,
        approval_id: ApprovalId,
    ) -> Result<AgentRunOutcome, AgentError> {
        let (task_id, adapter) = self.require_pending(approval_id)?;
        match adapter {
            PendingAdapter::Tool => {
                match self.tool_gate.approve_and_execute(supervisor, approval_id) {
                    Ok(executed) => {
                        self.resume_with_output(supervisor, executed.output.into_bytes())
                    }
                    Err(ExecutionError::Supervisor(error)) => Err(AgentError::Supervisor(error)),
                    Err(ExecutionError::Adapter(ToolAdapterError::BudgetExceeded)) => {
                        self.fail_budget_active(supervisor, task_id)?;
                        Err(AgentError::BudgetExceeded)
                    }
                    Err(ExecutionError::Adapter(_) | ExecutionError::OperationNotFound) => {
                        self.fail_active(supervisor, task_id)?;
                        Err(AgentError::ToolFailed)
                    }
                }
            }
            PendingAdapter::Network => {
                let result = self
                    .network
                    .as_mut()
                    .ok_or(AgentError::InvalidState)?
                    .gate
                    .approve_and_execute(supervisor, approval_id);
                match result {
                    Ok(executed) => {
                        self.resume_with_output(supervisor, executed.output.into_bytes())
                    }
                    Err(ExecutionError::Supervisor(error)) => Err(AgentError::Supervisor(error)),
                    Err(ExecutionError::Adapter(NetworkAdapterError::BudgetExceeded)) => {
                        self.fail_budget_active(supervisor, task_id)?;
                        Err(AgentError::BudgetExceeded)
                    }
                    Err(ExecutionError::Adapter(_) | ExecutionError::OperationNotFound) => {
                        self.fail_active(supervisor, task_id)?;
                        Err(AgentError::NetworkFailed)
                    }
                }
            }
        }
    }

    /// Denies one exact pending operation and drops the associated model session.
    pub fn deny<S: EventStore>(
        &mut self,
        supervisor: &mut TaskSupervisor<S>,
        approval_id: ApprovalId,
    ) -> Result<(), AgentError> {
        let (_task_id, adapter) = self.require_pending(approval_id)?;
        match adapter {
            PendingAdapter::Tool => self
                .tool_gate
                .deny(supervisor, approval_id)
                .map_err(map_tool_error)?,
            PendingAdapter::Network => self
                .network
                .as_mut()
                .ok_or(AgentError::InvalidState)?
                .gate
                .deny(supervisor, approval_id)
                .map_err(map_network_error)?,
        };
        self.active = None;
        Ok(())
    }

    /// Expires pending approvals and drops a model session whose Task became terminal.
    pub fn expire<S: EventStore>(
        &mut self,
        supervisor: &mut TaskSupervisor<S>,
    ) -> Result<usize, AgentError> {
        if let Some(task_id) = self.active.as_ref().map(|active| active.task_id)
            && supervisor.wall_time_exceeded(task_id)?
        {
            let adapter = self.pending_adapter().unwrap_or(PendingAdapter::Tool);
            self.fail_budget_active(supervisor, task_id)?;
            self.expire_adapter(supervisor, adapter)?;
            return Err(AgentError::BudgetExceeded);
        }
        let adapter = self.pending_adapter().unwrap_or(PendingAdapter::Tool);
        let expired = self.expire_adapter(supervisor, adapter)?;
        if self.active.as_ref().is_some_and(|active| {
            supervisor
                .get(active.task_id)
                .is_some_and(|task| task.state.is_terminal())
        }) {
            self.active = None;
        }
        Ok(expired)
    }

    /// Cancels one Task through its active operation gate and drops its model session.
    pub fn cancel<S: EventStore>(
        &mut self,
        supervisor: &mut TaskSupervisor<S>,
        task_id: TaskId,
    ) -> Result<bool, AgentError> {
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.task_id != task_id)
        {
            return Err(AgentError::InvalidState);
        }
        let adapter = self.pending_adapter().unwrap_or(PendingAdapter::Tool);
        let changed = match adapter {
            PendingAdapter::Tool => self
                .tool_gate
                .cancel(supervisor, task_id)
                .map_err(map_tool_error)?,
            PendingAdapter::Network => self
                .network
                .as_mut()
                .ok_or(AgentError::InvalidState)?
                .gate
                .cancel(supervisor, task_id)
                .map_err(map_network_error)?,
        };
        self.active = None;
        Ok(changed)
    }

    #[must_use]
    pub fn active_task(&self) -> Option<TaskId> {
        self.active.as_ref().map(|active| active.task_id)
    }

    fn drive<S: EventStore>(
        &mut self,
        supervisor: &mut TaskSupervisor<S>,
    ) -> Result<AgentRunOutcome, AgentError> {
        loop {
            let (task_id, next_step) = self
                .active
                .as_ref()
                .map(|active| (active.task_id, active.next_step))
                .ok_or(AgentError::InvalidState)?;
            if supervisor
                .execution_context(task_id)?
                .remaining_wall_time()
                .is_zero()
            {
                self.fail_budget_active(supervisor, task_id)?;
                return Err(AgentError::BudgetExceeded);
            }
            if next_step > self.config.max_steps {
                self.fail_active(supervisor, task_id)?;
                return Err(AgentError::StepLimitExceeded);
            }

            let decision = {
                let active = self.active.as_mut().ok_or(AgentError::InvalidState)?;
                let request = ModelTurnRequest {
                    step: active.next_step,
                    previous_operation_output: active.previous_operation_output.as_deref(),
                };
                match active.model.decide(request) {
                    Ok(decision) => decision,
                    Err(_) => {
                        self.fail_active(supervisor, task_id)?;
                        return Err(AgentError::ModelFailed);
                    }
                }
            };
            if supervisor
                .execution_context(task_id)?
                .remaining_wall_time()
                .is_zero()
            {
                self.fail_budget_active(supervisor, task_id)?;
                return Err(AgentError::BudgetExceeded);
            }
            self.active
                .as_mut()
                .ok_or(AgentError::InvalidState)?
                .next_step += 1;

            match decision.kind {
                ModelDecisionKind::Finish(output) => {
                    self.active = None;
                    supervisor.succeed(task_id)?;
                    return Ok(AgentRunOutcome::Completed(output));
                }
                ModelDecisionKind::CallTool { route, arguments } => {
                    let operation = match self.tool_catalog.prepare(&route, arguments) {
                        Ok(operation) => operation,
                        Err(_) => {
                            self.fail_active(supervisor, task_id)?;
                            return Err(AgentError::InvalidDecision);
                        }
                    };
                    let outcome = self.tool_gate.request(
                        supervisor,
                        task_id,
                        operation,
                        self.config.approval_ttl,
                    );
                    match outcome {
                        Ok(ExecutionOutcome::Executed(executed)) => {
                            self.active
                                .as_mut()
                                .ok_or(AgentError::InvalidState)?
                                .previous_operation_output = Some(executed.output.into_bytes());
                        }
                        Ok(ExecutionOutcome::Denied { reason }) => {
                            self.fail_active(supervisor, task_id)?;
                            return Ok(AgentRunOutcome::Denied { reason });
                        }
                        Ok(ExecutionOutcome::ApprovalRequired(request)) => {
                            self.active
                                .as_mut()
                                .ok_or(AgentError::InvalidState)?
                                .pending_approval = Some(PendingApproval {
                                approval_id: request.approval_id,
                                adapter: PendingAdapter::Tool,
                            });
                            return Ok(AgentRunOutcome::WaitingApproval(request));
                        }
                        Err(ExecutionError::Supervisor(error)) => {
                            self.active = None;
                            return Err(AgentError::Supervisor(error));
                        }
                        Err(ExecutionError::Adapter(ToolAdapterError::BudgetExceeded)) => {
                            self.fail_budget_active(supervisor, task_id)?;
                            return Err(AgentError::BudgetExceeded);
                        }
                        Err(ExecutionError::Adapter(_) | ExecutionError::OperationNotFound) => {
                            self.fail_active(supervisor, task_id)?;
                            return Err(AgentError::ToolFailed);
                        }
                    }
                }
                ModelDecisionKind::TcpExchange {
                    host,
                    port,
                    request,
                } => {
                    let Some(network) = self.network.as_ref() else {
                        self.fail_active(supervisor, task_id)?;
                        return Err(AgentError::InvalidDecision);
                    };
                    let operation = match network.catalog.prepare_tcp_exchange(host, port, request)
                    {
                        Ok(operation) => operation,
                        Err(_) => {
                            self.fail_active(supervisor, task_id)?;
                            return Err(AgentError::InvalidDecision);
                        }
                    };
                    let outcome = self
                        .network
                        .as_mut()
                        .ok_or(AgentError::InvalidState)?
                        .gate
                        .request(supervisor, task_id, operation, self.config.approval_ttl);
                    match outcome {
                        Ok(ExecutionOutcome::Executed(executed)) => {
                            self.active
                                .as_mut()
                                .ok_or(AgentError::InvalidState)?
                                .previous_operation_output = Some(executed.output.into_bytes());
                        }
                        Ok(ExecutionOutcome::Denied { reason }) => {
                            self.fail_active(supervisor, task_id)?;
                            return Ok(AgentRunOutcome::Denied { reason });
                        }
                        Ok(ExecutionOutcome::ApprovalRequired(request)) => {
                            self.active
                                .as_mut()
                                .ok_or(AgentError::InvalidState)?
                                .pending_approval = Some(PendingApproval {
                                approval_id: request.approval_id,
                                adapter: PendingAdapter::Network,
                            });
                            return Ok(AgentRunOutcome::WaitingApproval(request));
                        }
                        Err(ExecutionError::Supervisor(error)) => {
                            self.active = None;
                            return Err(AgentError::Supervisor(error));
                        }
                        Err(ExecutionError::Adapter(NetworkAdapterError::BudgetExceeded)) => {
                            self.fail_budget_active(supervisor, task_id)?;
                            return Err(AgentError::BudgetExceeded);
                        }
                        Err(ExecutionError::Adapter(_) | ExecutionError::OperationNotFound) => {
                            self.fail_active(supervisor, task_id)?;
                            return Err(AgentError::NetworkFailed);
                        }
                    }
                }
            }
        }
    }

    fn require_pending(
        &self,
        approval_id: ApprovalId,
    ) -> Result<(TaskId, PendingAdapter), AgentError> {
        let active = self.active.as_ref().ok_or(AgentError::InvalidState)?;
        let pending = active.pending_approval.ok_or(AgentError::InvalidState)?;
        if pending.approval_id != approval_id {
            return Err(AgentError::InvalidState);
        }
        Ok((active.task_id, pending.adapter))
    }

    fn pending_adapter(&self) -> Option<PendingAdapter> {
        self.active
            .as_ref()
            .and_then(|active| active.pending_approval)
            .map(|pending| pending.adapter)
    }

    fn resume_with_output<S: EventStore>(
        &mut self,
        supervisor: &mut TaskSupervisor<S>,
        output: Vec<u8>,
    ) -> Result<AgentRunOutcome, AgentError> {
        let active = self.active.as_mut().ok_or(AgentError::InvalidState)?;
        active.pending_approval = None;
        active.previous_operation_output = Some(output);
        self.drive(supervisor)
    }

    fn expire_adapter<S: EventStore>(
        &mut self,
        supervisor: &mut TaskSupervisor<S>,
        adapter: PendingAdapter,
    ) -> Result<usize, AgentError> {
        match adapter {
            PendingAdapter::Tool => self.tool_gate.expire(supervisor).map_err(map_tool_error),
            PendingAdapter::Network => self
                .network
                .as_mut()
                .ok_or(AgentError::InvalidState)?
                .gate
                .expire(supervisor)
                .map_err(map_network_error),
        }
    }

    fn fail_active<S: EventStore>(
        &mut self,
        supervisor: &mut TaskSupervisor<S>,
        task_id: TaskId,
    ) -> Result<(), AgentError> {
        self.active = None;
        supervisor.fail(task_id)?;
        Ok(())
    }

    fn fail_budget_active<S: EventStore>(
        &mut self,
        supervisor: &mut TaskSupervisor<S>,
        task_id: TaskId,
    ) -> Result<(), AgentError> {
        self.active = None;
        supervisor.fail_budget_exceeded(task_id)?;
        Ok(())
    }
}

/// Result of driving a Task as far as possible without an external approval decision.
pub enum AgentRunOutcome {
    Completed(AgentOutput),
    WaitingApproval(ApprovalRequest),
    Denied { reason: DenialReason },
}

/// Resource-free Agent runtime failure categories.
pub enum AgentError {
    InvalidConfig,
    CapacityExceeded,
    InvalidState,
    InvalidDecision,
    StepLimitExceeded,
    ModelFailed,
    ToolFailed,
    NetworkFailed,
    BudgetExceeded,
    Supervisor(SupervisorError),
}

impl AgentError {
    #[must_use]
    pub const fn code(&self) -> Option<ErrorCode> {
        match self {
            Self::BudgetExceeded => Some(ErrorCode::BudgetExceeded),
            Self::InvalidConfig
            | Self::CapacityExceeded
            | Self::InvalidState
            | Self::InvalidDecision
            | Self::StepLimitExceeded
            | Self::ModelFailed
            | Self::ToolFailed
            | Self::NetworkFailed
            | Self::Supervisor(_) => None,
        }
    }
}

impl fmt::Debug for AgentError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(self, formatter)
    }
}

impl Display for AgentError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidConfig => "invalid Agent runtime configuration",
            Self::CapacityExceeded => "Agent runtime capacity exceeded",
            Self::InvalidState => "Agent runtime state is invalid",
            Self::InvalidDecision => "model decision is invalid",
            Self::StepLimitExceeded => "Agent step limit exceeded",
            Self::ModelFailed => "model adapter failed",
            Self::ToolFailed => "Tool execution failed",
            Self::NetworkFailed => "Network execution failed",
            Self::BudgetExceeded => "Task Budget exceeded",
            Self::Supervisor(_) => "Task supervision failed",
        };
        formatter.write_str(message)
    }
}

impl Error for AgentError {}

impl From<SupervisorError> for AgentError {
    fn from(error: SupervisorError) -> Self {
        Self::Supervisor(error)
    }
}

fn map_tool_error(error: ExecutionError<ToolAdapterError>) -> AgentError {
    match error {
        ExecutionError::Supervisor(error) => AgentError::Supervisor(error),
        ExecutionError::Adapter(ToolAdapterError::BudgetExceeded) => AgentError::BudgetExceeded,
        ExecutionError::Adapter(_) | ExecutionError::OperationNotFound => AgentError::ToolFailed,
    }
}

fn map_network_error(error: ExecutionError<NetworkAdapterError>) -> AgentError {
    match error {
        ExecutionError::Supervisor(error) => AgentError::Supervisor(error),
        ExecutionError::Adapter(NetworkAdapterError::BudgetExceeded) => AgentError::BudgetExceeded,
        ExecutionError::Adapter(_) | ExecutionError::OperationNotFound => AgentError::NetworkFailed,
    }
}

fn is_valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_IDENTIFIER_BYTES
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | ':' | '-')
        })
}

fn are_valid_arguments(arguments: &[String]) -> bool {
    if arguments.len() > MAX_ARGUMENTS {
        return false;
    }
    let mut total_bytes = 0_usize;
    for argument in arguments {
        if argument.len() > MAX_ARGUMENT_BYTES || argument.contains('\0') {
            return false;
        }
        let Some(next_total) = total_bytes.checked_add(argument.len()) else {
            return false;
        };
        total_bytes = next_total;
        if total_bytes > MAX_TOTAL_ARGUMENT_BYTES {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::cell::RefCell;
    use std::io::{ErrorKind, Read, Write};
    use std::net::TcpListener;
    use std::rc::Rc;
    use std::thread;
    use std::time::Duration;

    use aios_adapter_network::{MAX_TCP_REQUEST_BYTES, NetworkExecutionGate};
    use aios_adapter_tool::{ToolAdapterBuilder, ToolFailure, ToolOutput};
    use aios_core::{
        ApprovalPolicy, Budget, CapabilitySet, ErrorCode, NetworkDestination, NetworkPolicy,
        NetworkTransport, TaskSpec, TaskState,
    };
    use aios_runtime::{
        ApprovalId, InMemoryEventStore, SubmitResult, TaskEventKind, TaskSupervisor,
    };

    use super::{
        AgentConfig, AgentError, AgentRunOutcome, AgentRuntime, MAX_ARGUMENT_BYTES,
        MAX_FINAL_OUTPUT_BYTES, ModelAdapter, ModelDecision, ModelDecisionError, ModelSession,
        ModelStartRequest, ModelTurnRequest, ScriptedModelAdapter,
    };

    fn task(idempotency_key: &str, required_for: &[&str], tools: &[&str]) -> TaskSpec {
        TaskSpec {
            idempotency_key: idempotency_key.to_owned(),
            goal: "Run a bounded model-directed Task".to_owned(),
            capabilities: CapabilitySet {
                filesystem: Vec::new(),
                network: NetworkPolicy::Deny,
                tools: tools.iter().map(|tool| (*tool).to_owned()).collect(),
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

    fn network_task(
        idempotency_key: &str,
        host: &str,
        port: u16,
        required_for: &[&str],
    ) -> TaskSpec {
        let mut spec = task(idempotency_key, required_for, &[]);
        spec.capabilities.network = NetworkPolicy::Allow {
            destinations: vec![NetworkDestination {
                host: host.to_owned(),
                transport: NetworkTransport::Tcp,
                port,
            }],
        };
        spec
    }

    fn submit(
        supervisor: &mut TaskSupervisor,
        idempotency_key: &str,
        required_for: &[&str],
        tools: &[&str],
    ) -> aios_runtime::TaskId {
        submit_spec(supervisor, task(idempotency_key, required_for, tools))
    }

    fn submit_spec(supervisor: &mut TaskSupervisor, spec: TaskSpec) -> aios_runtime::TaskId {
        let SubmitResult::Accepted(task) = supervisor.submit(spec).expect("submit Task") else {
            panic!("expected accepted Task");
        };
        task.task_id
    }

    fn tools(
        seen: Rc<RefCell<Vec<Vec<String>>>>,
    ) -> (
        aios_adapter_tool::ToolCatalog,
        aios_adapter_tool::ToolExecutionGate,
    ) {
        let mut builder = ToolAdapterBuilder::default();
        builder
            .register(
                "run_tests",
                "test_runner",
                "test.run",
                move |arguments: Vec<String>| {
                    seen.borrow_mut().push(arguments);
                    ToolOutput::from_text("tool-ok".to_owned()).map_err(|_| ToolFailure::new())
                },
            )
            .expect("register Tool");
        builder.build()
    }

    #[test]
    fn validates_model_decision_bounds_before_catalog_lookup() {
        assert!(matches!(
            ModelDecision::finish(String::new()),
            Err(ModelDecisionError::InvalidFinalOutput)
        ));
        assert!(matches!(
            ModelDecision::finish("x".repeat(MAX_FINAL_OUTPUT_BYTES + 1)),
            Err(ModelDecisionError::InvalidFinalOutput)
        ));
        assert!(matches!(
            ModelDecision::call_tool("bad route".to_owned(), Vec::new()),
            Err(ModelDecisionError::InvalidToolRequest)
        ));
        assert!(matches!(
            ModelDecision::call_tool(
                "run_tests".to_owned(),
                vec!["x".repeat(MAX_ARGUMENT_BYTES + 1)]
            ),
            Err(ModelDecisionError::InvalidToolRequest)
        ));
        assert!(matches!(
            ModelDecision::tcp_exchange("127.0.0.1".to_owned(), 0, Vec::new()),
            Err(ModelDecisionError::InvalidNetworkRequest)
        ));
        assert!(matches!(
            ModelDecision::tcp_exchange(
                "127.0.0.1".to_owned(),
                443,
                vec![0; MAX_TCP_REQUEST_BYTES + 1]
            ),
            Err(ModelDecisionError::InvalidNetworkRequest)
        ));
    }

    #[test]
    fn completes_task_from_bounded_scripted_model_output() {
        let model = ScriptedModelAdapter::new(vec![
            ModelDecision::finish("completed safely".to_owned()).expect("valid decision"),
        ])
        .expect("valid script");
        let (catalog, gate) = tools(Rc::new(RefCell::new(Vec::new())));
        let mut runtime = AgentRuntime::new(model, catalog, gate, AgentConfig::default());
        let mut supervisor = TaskSupervisor::default();
        let task_id = submit(&mut supervisor, "agent-finish", &[], &[]);

        let outcome = runtime.start(&mut supervisor, task_id).expect("run Agent");

        let AgentRunOutcome::Completed(output) = outcome else {
            panic!("expected completion");
        };
        assert_eq!(output.as_str(), "completed safely");
        assert_eq!(
            supervisor.get(task_id).expect("Task exists").state,
            TaskState::Succeeded
        );
        assert_eq!(runtime.active_task(), None);
    }

    struct StartRecordingAdapter {
        started: Rc<Cell<bool>>,
    }

    impl ModelAdapter for StartRecordingAdapter {
        type Error = &'static str;
        type Session = ObservingSession;

        fn start_session(
            &mut self,
            _request: ModelStartRequest<'_>,
        ) -> Result<Self::Session, Self::Error> {
            self.started.set(true);
            Ok(ObservingSession { turn: 0 })
        }
    }

    #[test]
    fn audit_failure_prevents_goal_release_and_model_session_start() {
        let started = Rc::new(Cell::new(false));
        let model = StartRecordingAdapter {
            started: Rc::clone(&started),
        };
        let (catalog, gate) = tools(Rc::new(RefCell::new(Vec::new())));
        let mut runtime = AgentRuntime::new(model, catalog, gate, AgentConfig::default());
        let store = InMemoryEventStore::new(3).expect("submission-only capacity");
        let mut supervisor = TaskSupervisor::new(store);
        let task_id = submit(&mut supervisor, "agent-audit-failure", &[], &[]);

        assert!(matches!(
            runtime.start(&mut supervisor, task_id),
            Err(AgentError::Supervisor(_))
        ));
        assert!(!started.get());
        assert_eq!(
            supervisor.get(task_id).expect("Task exists").state,
            TaskState::Queued
        );
        assert_eq!(runtime.active_task(), None);
    }

    struct RouteRecordingAdapter {
        routes: Rc<RefCell<Vec<String>>>,
    }

    struct FinishingSession;

    impl ModelAdapter for RouteRecordingAdapter {
        type Error = &'static str;
        type Session = FinishingSession;

        fn start_session(
            &mut self,
            request: ModelStartRequest<'_>,
        ) -> Result<Self::Session, Self::Error> {
            self.routes.borrow_mut().extend(
                request
                    .tool_routes()
                    .iter()
                    .map(|route| (*route).to_owned()),
            );
            Ok(FinishingSession)
        }
    }

    impl ModelSession for FinishingSession {
        type Error = &'static str;

        fn decide(&mut self, _request: ModelTurnRequest<'_>) -> Result<ModelDecision, Self::Error> {
            ModelDecision::finish("no granted routes".to_owned())
                .map_err(|_| "invalid test decision")
        }
    }

    #[test]
    fn exposes_only_routes_backed_by_task_tool_capabilities() {
        let routes = Rc::new(RefCell::new(Vec::new()));
        let model = RouteRecordingAdapter {
            routes: Rc::clone(&routes),
        };
        let (catalog, gate) = tools(Rc::new(RefCell::new(Vec::new())));
        let mut runtime = AgentRuntime::new(model, catalog, gate, AgentConfig::default());
        let mut supervisor = TaskSupervisor::default();
        let task_id = submit(
            &mut supervisor,
            "agent-route-filter",
            &[],
            &["different_tool"],
        );

        assert!(matches!(
            runtime.start(&mut supervisor, task_id),
            Ok(AgentRunOutcome::Completed(_))
        ));
        assert!(routes.borrow().is_empty());
    }

    struct NetworkScopeRecordingAdapter {
        count: Rc<Cell<usize>>,
    }

    impl ModelAdapter for NetworkScopeRecordingAdapter {
        type Error = &'static str;
        type Session = FinishingSession;

        fn start_session(
            &mut self,
            request: ModelStartRequest<'_>,
        ) -> Result<Self::Session, Self::Error> {
            self.count.set(request.network_destinations().len());
            Ok(FinishingSession)
        }
    }

    #[test]
    fn exposes_only_destinations_supported_by_the_configured_network_gate() {
        let (catalog, gate) = tools(Rc::new(RefCell::new(Vec::new())));
        let unavailable_count = Rc::new(Cell::new(usize::MAX));
        let mut runtime = AgentRuntime::new(
            NetworkScopeRecordingAdapter {
                count: Rc::clone(&unavailable_count),
            },
            catalog,
            gate,
            AgentConfig::default(),
        );
        let mut supervisor = TaskSupervisor::default();
        let task_id = submit_spec(
            &mut supervisor,
            network_task("agent-network-hidden", "127.0.0.1", 443, &[]),
        );
        assert!(matches!(
            runtime.start(&mut supervisor, task_id),
            Ok(AgentRunOutcome::Completed(_))
        ));
        assert_eq!(unavailable_count.get(), 0);

        let (catalog, gate) = tools(Rc::new(RefCell::new(Vec::new())));
        let unsupported_count = Rc::new(Cell::new(usize::MAX));
        let network_gate =
            NetworkExecutionGate::new(Duration::from_secs(2)).expect("create Network gate");
        let mut runtime = AgentRuntime::new(
            NetworkScopeRecordingAdapter {
                count: Rc::clone(&unsupported_count),
            },
            catalog,
            gate,
            AgentConfig::default(),
        )
        .with_network_gate(network_gate);
        let mut supervisor = TaskSupervisor::default();
        let task_id = submit_spec(
            &mut supervisor,
            network_task("agent-hostname-hidden", "api.example.com", 443, &[]),
        );
        assert!(matches!(
            runtime.start(&mut supervisor, task_id),
            Ok(AgentRunOutcome::Completed(_))
        ));
        assert_eq!(unsupported_count.get(), 0);
    }

    struct NetworkObservingAdapter {
        port: u16,
    }

    struct NetworkObservingSession {
        port: u16,
        turn: u8,
    }

    impl ModelAdapter for NetworkObservingAdapter {
        type Error = &'static str;
        type Session = NetworkObservingSession;

        fn start_session(
            &mut self,
            request: ModelStartRequest<'_>,
        ) -> Result<Self::Session, Self::Error> {
            let [destination] = request.network_destinations() else {
                return Err("expected one Network destination");
            };
            if destination.host != "127.0.0.1"
                || destination.transport != NetworkTransport::Tcp
                || destination.port != self.port
            {
                return Err("unexpected Network destination");
            }
            Ok(NetworkObservingSession {
                port: self.port,
                turn: 0,
            })
        }
    }

    impl ModelSession for NetworkObservingSession {
        type Error = &'static str;

        fn decide(&mut self, request: ModelTurnRequest<'_>) -> Result<ModelDecision, Self::Error> {
            let decision = match self.turn {
                0 if request.previous_operation_output().is_none() => ModelDecision::tcp_exchange(
                    "127.0.0.1".to_owned(),
                    self.port,
                    b"agent request".to_vec(),
                )
                .map_err(|_| "invalid Network decision")?,
                1 if request.previous_operation_output() == Some(b"agent response".as_slice()) => {
                    ModelDecision::finish("network completed".to_owned())
                        .map_err(|_| "invalid final decision")?
                }
                _ => return Err("unexpected Network observation"),
            };
            self.turn += 1;
            Ok(decision)
        }
    }

    #[test]
    fn routes_task_scoped_network_exchange_and_returns_bounded_response() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind listener");
        let port = listener.local_addr().expect("listener address").port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept connection");
            let mut request = Vec::new();
            stream.read_to_end(&mut request).expect("read request");
            stream.write_all(b"agent response").expect("write response");
            request
        });
        let (catalog, gate) = tools(Rc::new(RefCell::new(Vec::new())));
        let network_gate =
            NetworkExecutionGate::new(Duration::from_secs(2)).expect("create Network gate");
        let mut runtime = AgentRuntime::new(
            NetworkObservingAdapter { port },
            catalog,
            gate,
            AgentConfig::default(),
        )
        .with_network_gate(network_gate);
        let mut supervisor = TaskSupervisor::default();
        let task_id = submit_spec(
            &mut supervisor,
            network_task("agent-network", "127.0.0.1", port, &[]),
        );

        let outcome = runtime
            .start(&mut supervisor, task_id)
            .expect("run Network Agent");

        let AgentRunOutcome::Completed(output) = outcome else {
            panic!("expected completion");
        };
        assert_eq!(output.as_str(), "network completed");
        assert_eq!(server.join().expect("join server"), b"agent request");
        assert_eq!(
            supervisor.get(task_id).expect("Task exists").state,
            TaskState::Succeeded
        );
    }

    #[test]
    fn network_approval_connects_only_after_exact_resume() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind listener");
        listener
            .set_nonblocking(true)
            .expect("set nonblocking listener");
        let port = listener.local_addr().expect("listener address").port();
        let (catalog, gate) = tools(Rc::new(RefCell::new(Vec::new())));
        let network_gate =
            NetworkExecutionGate::new(Duration::from_secs(2)).expect("create Network gate");
        let mut runtime = AgentRuntime::new(
            NetworkObservingAdapter { port },
            catalog,
            gate,
            AgentConfig::default(),
        )
        .with_network_gate(network_gate);
        let mut supervisor = TaskSupervisor::default();
        let task_id = submit_spec(
            &mut supervisor,
            network_task(
                "agent-network-approval",
                "127.0.0.1",
                port,
                &["network.egress"],
            ),
        );

        let AgentRunOutcome::WaitingApproval(request) = runtime
            .start(&mut supervisor, task_id)
            .expect("request Network approval")
        else {
            panic!("expected approval wait");
        };
        assert_eq!(
            listener
                .accept()
                .expect_err("approval wait must not connect")
                .kind(),
            ErrorKind::WouldBlock
        );
        listener
            .set_nonblocking(false)
            .expect("restore blocking listener");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept connection");
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes).expect("read request");
            stream.write_all(b"agent response").expect("write response");
        });

        let outcome = runtime
            .approve_and_resume(&mut supervisor, request.approval_id)
            .expect("approve Network operation");

        assert!(matches!(outcome, AgentRunOutcome::Completed(_)));
        server.join().expect("join server");
        assert_eq!(
            supervisor.get(task_id).expect("Task exists").state,
            TaskState::Succeeded
        );
    }

    #[test]
    fn network_proposal_without_configured_gate_fails_without_connecting() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind listener");
        listener
            .set_nonblocking(true)
            .expect("set nonblocking listener");
        let port = listener.local_addr().expect("listener address").port();
        let model = ScriptedModelAdapter::new(vec![
            ModelDecision::tcp_exchange("127.0.0.1".to_owned(), port, b"must not connect".to_vec())
                .expect("valid Network decision"),
        ])
        .expect("valid script");
        let (catalog, gate) = tools(Rc::new(RefCell::new(Vec::new())));
        let mut runtime = AgentRuntime::new(model, catalog, gate, AgentConfig::default());
        let mut supervisor = TaskSupervisor::default();
        let task_id = submit_spec(
            &mut supervisor,
            network_task("agent-network-unavailable", "127.0.0.1", port, &[]),
        );

        assert!(matches!(
            runtime.start(&mut supervisor, task_id),
            Err(AgentError::InvalidDecision)
        ));
        assert_eq!(
            listener
                .accept()
                .expect_err("missing gate must not connect")
                .kind(),
            ErrorKind::WouldBlock
        );
        assert_eq!(
            supervisor.get(task_id).expect("Task exists").state,
            TaskState::Failed
        );
    }

    #[test]
    fn denied_network_destination_never_connects() {
        let authorized = TcpListener::bind(("127.0.0.1", 0)).expect("bind authorized listener");
        let authorized_port = authorized.local_addr().expect("authorized address").port();
        let attempted = TcpListener::bind(("127.0.0.1", 0)).expect("bind attempted listener");
        attempted
            .set_nonblocking(true)
            .expect("set nonblocking listener");
        let attempted_port = attempted.local_addr().expect("attempted address").port();
        let model = ScriptedModelAdapter::new(vec![
            ModelDecision::tcp_exchange("127.0.0.1".to_owned(), attempted_port, b"denied".to_vec())
                .expect("valid Network decision"),
        ])
        .expect("valid script");
        let (catalog, gate) = tools(Rc::new(RefCell::new(Vec::new())));
        let network_gate =
            NetworkExecutionGate::new(Duration::from_secs(2)).expect("create Network gate");
        let mut runtime = AgentRuntime::new(model, catalog, gate, AgentConfig::default())
            .with_network_gate(network_gate);
        let mut supervisor = TaskSupervisor::default();
        let task_id = submit_spec(
            &mut supervisor,
            network_task("agent-network-denied", "127.0.0.1", authorized_port, &[]),
        );

        let outcome = runtime
            .start(&mut supervisor, task_id)
            .expect("policy denial is an Agent outcome");

        assert!(matches!(outcome, AgentRunOutcome::Denied { .. }));
        assert_eq!(
            attempted
                .accept()
                .expect_err("denied destination must not connect")
                .kind(),
            ErrorKind::WouldBlock
        );
        drop(authorized);
    }

    #[test]
    fn network_budget_failure_records_terminal_budget_event() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind listener");
        let port = listener.local_addr().expect("listener address").port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept connection");
            let mut request = Vec::new();
            stream.read_to_end(&mut request).expect("read request");
            thread::sleep(Duration::from_millis(1_500));
        });
        let model = ScriptedModelAdapter::new(vec![
            ModelDecision::tcp_exchange("127.0.0.1".to_owned(), port, b"bounded request".to_vec())
                .expect("valid Network decision"),
        ])
        .expect("valid script");
        let (catalog, gate) = tools(Rc::new(RefCell::new(Vec::new())));
        let network_gate =
            NetworkExecutionGate::new(Duration::from_secs(5)).expect("create Network gate");
        let mut runtime = AgentRuntime::new(model, catalog, gate, AgentConfig::default())
            .with_network_gate(network_gate);
        let mut supervisor = TaskSupervisor::default();
        let mut spec = network_task("agent-network-budget", "127.0.0.1", port, &[]);
        spec.budget.wall_time_seconds = 1;
        let task_id = submit_spec(&mut supervisor, spec);

        assert!(matches!(
            runtime.start(&mut supervisor, task_id),
            Err(AgentError::BudgetExceeded)
        ));
        server.join().expect("join server");
        assert_eq!(
            supervisor.get(task_id).expect("Task exists").state,
            TaskState::Failed
        );
        assert!(
            supervisor
                .events(task_id, 0)
                .expect("list Events")
                .iter()
                .any(|event| event.kind
                    == TaskEventKind::TaskFailed {
                        code: ErrorCode::BudgetExceeded,
                    })
        );
    }

    struct ObservingAdapter;

    struct ObservingSession {
        turn: u8,
    }

    impl ModelAdapter for ObservingAdapter {
        type Error = &'static str;
        type Session = ObservingSession;

        fn start_session(
            &mut self,
            request: ModelStartRequest<'_>,
        ) -> Result<Self::Session, Self::Error> {
            if request.goal() != "Run a bounded model-directed Task"
                || request.tool_routes() != ["run_tests"]
            {
                return Err("sensitive start detail");
            }
            Ok(ObservingSession { turn: 0 })
        }
    }

    impl ModelSession for ObservingSession {
        type Error = &'static str;

        fn decide(&mut self, request: ModelTurnRequest<'_>) -> Result<ModelDecision, Self::Error> {
            let decision = match self.turn {
                0 if request.step() == 1 && request.previous_tool_output().is_none() => {
                    ModelDecision::call_tool("run_tests".to_owned(), vec!["--safe".to_owned()])
                        .map_err(|_| "invalid test decision")?
                }
                1 if request.step() == 2
                    && request.previous_tool_output() == Some(b"tool-ok".as_slice()) =>
                {
                    ModelDecision::finish("observed bounded output".to_owned())
                        .map_err(|_| "invalid test decision")?
                }
                _ => return Err("sensitive turn detail"),
            };
            self.turn += 1;
            Ok(decision)
        }
    }

    #[test]
    fn routes_model_tool_request_through_catalog_and_returns_bounded_output() {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let (catalog, gate) = tools(Rc::clone(&seen));
        let mut runtime =
            AgentRuntime::new(ObservingAdapter, catalog, gate, AgentConfig::default());
        let mut supervisor = TaskSupervisor::default();
        let task_id = submit(&mut supervisor, "agent-tool", &[], &["test_runner"]);

        let outcome = runtime.start(&mut supervisor, task_id).expect("run Agent");

        assert!(matches!(outcome, AgentRunOutcome::Completed(_)));
        assert_eq!(seen.borrow().as_slice(), &[vec!["--safe".to_owned()]]);
        assert_eq!(
            supervisor.get(task_id).expect("Task exists").state,
            TaskState::Succeeded
        );
    }

    #[test]
    fn budget_exhaustion_stops_the_agent_and_records_the_stable_failure() {
        let model = ScriptedModelAdapter::new(vec![
            ModelDecision::call_tool("run_tests".to_owned(), Vec::new())
                .expect("valid Tool decision"),
        ])
        .expect("valid script");
        let mut builder = ToolAdapterBuilder::default();
        builder
            .register("run_tests", "test_runner", "test.run", |_| {
                Err(ToolFailure::budget_exceeded())
            })
            .expect("register Tool");
        let (catalog, gate) = builder.build();
        let mut runtime = AgentRuntime::new(model, catalog, gate, AgentConfig::default());
        let mut supervisor = TaskSupervisor::default();
        let task_id = submit(&mut supervisor, "agent-budget", &[], &["test_runner"]);

        let error = match runtime.start(&mut supervisor, task_id) {
            Err(error) => error,
            Ok(_) => panic!("Task Budget must stop execution"),
        };

        assert!(matches!(&error, AgentError::BudgetExceeded));
        assert_eq!(error.code(), Some(ErrorCode::BudgetExceeded));
        assert_eq!(
            supervisor.get(task_id).expect("Task exists").state,
            TaskState::Failed
        );
        let events = supervisor.events(task_id, 0).expect("list events");
        assert!(events.iter().any(|event| {
            event.kind
                == TaskEventKind::TaskFailed {
                    code: ErrorCode::BudgetExceeded,
                }
        }));
        assert_eq!(runtime.active_task(), None);
    }

    #[test]
    fn approved_operation_cannot_continue_after_budget_exhaustion() {
        let model = ScriptedModelAdapter::new(vec![
            ModelDecision::call_tool("run_tests".to_owned(), Vec::new())
                .expect("valid Tool decision"),
        ])
        .expect("valid script");
        let mut builder = ToolAdapterBuilder::default();
        builder
            .register("run_tests", "test_runner", "test.run", |_| {
                Err(ToolFailure::budget_exceeded())
            })
            .expect("register Tool");
        let (catalog, gate) = builder.build();
        let mut runtime = AgentRuntime::new(model, catalog, gate, AgentConfig::default());
        let mut supervisor = TaskSupervisor::default();
        let task_id = submit(
            &mut supervisor,
            "agent-approved-budget",
            &["test.run"],
            &["test_runner"],
        );
        let AgentRunOutcome::WaitingApproval(request) = runtime
            .start(&mut supervisor, task_id)
            .expect("wait for approval")
        else {
            panic!("expected approval wait");
        };

        assert!(matches!(
            runtime.approve_and_resume(&mut supervisor, request.approval_id),
            Err(AgentError::BudgetExceeded)
        ));
        assert_eq!(
            supervisor.get(task_id).expect("Task exists").state,
            TaskState::Failed
        );
        assert_eq!(runtime.active_task(), None);
    }

    #[test]
    fn deadline_monitor_fails_a_task_while_approval_is_waiting() {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let (catalog, gate) = tools(Rc::clone(&seen));
        let model = ScriptedModelAdapter::new(vec![
            ModelDecision::call_tool("run_tests".to_owned(), Vec::new())
                .expect("valid Tool decision"),
        ])
        .expect("valid script");
        let mut runtime = AgentRuntime::new(model, catalog, gate, AgentConfig::default());
        let mut supervisor = TaskSupervisor::default();
        let mut spec = task("agent-waiting-budget", &["test.run"], &["test_runner"]);
        spec.budget.wall_time_seconds = 1;
        let SubmitResult::Accepted(task) = supervisor.submit(spec).expect("submit Task") else {
            panic!("expected accepted Task");
        };
        assert!(matches!(
            runtime.start(&mut supervisor, task.task_id),
            Ok(AgentRunOutcome::WaitingApproval(_))
        ));
        std::thread::sleep(Duration::from_millis(1_100));

        assert!(matches!(
            runtime.expire(&mut supervisor),
            Err(AgentError::BudgetExceeded)
        ));
        assert!(seen.borrow().is_empty());
        assert_eq!(
            supervisor.get(task.task_id).expect("Task exists").state,
            TaskState::Failed
        );
        assert_eq!(runtime.active_task(), None);
    }

    #[test]
    fn slow_tool_cannot_reset_wall_time_between_model_turns() {
        let model = ScriptedModelAdapter::new(vec![
            ModelDecision::call_tool("run_tests".to_owned(), Vec::new())
                .expect("valid Tool decision"),
            ModelDecision::finish("must not complete".to_owned()).expect("valid final decision"),
        ])
        .expect("valid script");
        let mut builder = ToolAdapterBuilder::default();
        builder
            .register("run_tests", "test_runner", "test.run", |_| {
                std::thread::sleep(Duration::from_millis(1_100));
                ToolOutput::from_text("late".to_owned()).map_err(|_| ToolFailure::new())
            })
            .expect("register Tool");
        let (catalog, gate) = builder.build();
        let mut runtime = AgentRuntime::new(model, catalog, gate, AgentConfig::default());
        let mut supervisor = TaskSupervisor::default();
        let mut spec = task("agent-wall-time", &[], &["test_runner"]);
        spec.budget.wall_time_seconds = 1;
        let SubmitResult::Accepted(task) = supervisor.submit(spec).expect("submit Task") else {
            panic!("expected accepted Task");
        };

        assert!(matches!(
            runtime.start(&mut supervisor, task.task_id),
            Err(AgentError::BudgetExceeded)
        ));
        assert_eq!(
            supervisor.get(task.task_id).expect("Task exists").state,
            TaskState::Failed
        );
        assert_eq!(runtime.active_task(), None);
    }

    #[test]
    fn denied_capability_fails_task_without_calling_handler() {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let (catalog, gate) = tools(Rc::clone(&seen));
        let model = ScriptedModelAdapter::new(vec![
            ModelDecision::call_tool("run_tests".to_owned(), Vec::new()).expect("valid decision"),
        ])
        .expect("valid script");
        let mut runtime = AgentRuntime::new(model, catalog, gate, AgentConfig::default());
        let mut supervisor = TaskSupervisor::default();
        let task_id = submit(&mut supervisor, "agent-denied", &[], &["different_tool"]);

        let outcome = runtime
            .start(&mut supervisor, task_id)
            .expect("policy denial is an Agent outcome");

        assert!(matches!(outcome, AgentRunOutcome::Denied { .. }));
        assert!(seen.borrow().is_empty());
        assert_eq!(
            supervisor.get(task_id).expect("Task exists").state,
            TaskState::Failed
        );
    }

    #[test]
    fn approval_resumes_exact_retained_operation_and_model_session() {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let (catalog, gate) = tools(Rc::clone(&seen));
        let model = ScriptedModelAdapter::new(vec![
            ModelDecision::call_tool("run_tests".to_owned(), vec!["approved".to_owned()])
                .expect("valid Tool decision"),
            ModelDecision::finish("approved completion".to_owned()).expect("valid final decision"),
        ])
        .expect("valid script");
        let mut runtime = AgentRuntime::new(model, catalog, gate, AgentConfig::default());
        let mut supervisor = TaskSupervisor::default();
        let task_id = submit(
            &mut supervisor,
            "agent-approval",
            &["test.run"],
            &["test_runner"],
        );

        let AgentRunOutcome::WaitingApproval(request) = runtime
            .start(&mut supervisor, task_id)
            .expect("request approval")
        else {
            panic!("expected approval wait");
        };
        assert!(seen.borrow().is_empty());
        assert_eq!(runtime.active_task(), Some(task_id));
        assert_eq!(
            supervisor.get(task_id).expect("Task exists").state,
            TaskState::WaitingApproval
        );

        let mismatched: ApprovalId = "00000000-0000-0000-0000-000000000000"
            .parse()
            .expect("valid UUID");
        assert!(matches!(
            runtime.approve_and_resume(&mut supervisor, mismatched),
            Err(AgentError::InvalidState)
        ));
        assert!(seen.borrow().is_empty());

        let outcome = runtime
            .approve_and_resume(&mut supervisor, request.approval_id)
            .expect("approve and resume");
        assert!(matches!(outcome, AgentRunOutcome::Completed(_)));
        assert_eq!(seen.borrow().as_slice(), &[vec!["approved".to_owned()]]);
        assert_eq!(
            supervisor.get(task_id).expect("Task exists").state,
            TaskState::Succeeded
        );
    }

    #[test]
    fn approval_expiration_fails_task_and_drops_model_session() {
        let (catalog, gate) = tools(Rc::new(RefCell::new(Vec::new())));
        let model = ScriptedModelAdapter::new(vec![
            ModelDecision::call_tool("run_tests".to_owned(), Vec::new()).expect("valid decision"),
        ])
        .expect("valid script");
        let config = AgentConfig::new(4, Duration::from_millis(1)).expect("valid config");
        let mut runtime = AgentRuntime::new(model, catalog, gate, config);
        let mut supervisor = TaskSupervisor::default();
        let task_id = submit(
            &mut supervisor,
            "agent-expiration",
            &["test.run"],
            &["test_runner"],
        );

        assert!(matches!(
            runtime.start(&mut supervisor, task_id),
            Ok(AgentRunOutcome::WaitingApproval(_))
        ));
        std::thread::sleep(Duration::from_millis(5));

        assert_eq!(runtime.expire(&mut supervisor).expect("expire approval"), 1);
        assert_eq!(runtime.active_task(), None);
        assert_eq!(
            supervisor.get(task_id).expect("Task exists").state,
            TaskState::Failed
        );
    }

    #[test]
    fn step_limit_fails_task_after_bounded_number_of_model_turns() {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let (catalog, gate) = tools(Rc::clone(&seen));
        let model = ScriptedModelAdapter::new(vec![
            ModelDecision::call_tool("run_tests".to_owned(), Vec::new()).expect("first decision"),
            ModelDecision::finish("must not be reached".to_owned()).expect("second decision"),
        ])
        .expect("valid script");
        let config = AgentConfig::new(1, Duration::from_secs(30)).expect("valid config");
        let mut runtime = AgentRuntime::new(model, catalog, gate, config);
        let mut supervisor = TaskSupervisor::default();
        let task_id = submit(&mut supervisor, "agent-step-limit", &[], &["test_runner"]);

        let error = match runtime.start(&mut supervisor, task_id) {
            Err(error) => error,
            Ok(_) => panic!("step limit must fail"),
        };

        assert!(matches!(error, AgentError::StepLimitExceeded));
        assert_eq!(seen.borrow().len(), 1);
        assert_eq!(
            supervisor.get(task_id).expect("Task exists").state,
            TaskState::Failed
        );
    }

    #[test]
    fn unknown_route_and_model_failure_are_redacted_and_fail_closed() {
        let (catalog, gate) = tools(Rc::new(RefCell::new(Vec::new())));
        let model = ScriptedModelAdapter::new(vec![
            ModelDecision::call_tool("unknown_route".to_owned(), Vec::new())
                .expect("syntactically valid decision"),
        ])
        .expect("valid script");
        let mut runtime = AgentRuntime::new(model, catalog, gate, AgentConfig::default());
        let mut supervisor = TaskSupervisor::default();
        let task_id = submit(&mut supervisor, "agent-unknown", &[], &["test_runner"]);

        let error = match runtime.start(&mut supervisor, task_id) {
            Err(error) => error,
            Ok(_) => panic!("unknown route must fail"),
        };
        assert!(matches!(error, AgentError::InvalidDecision));
        assert!(!error.to_string().contains("unknown_route"));

        let (catalog, gate) = tools(Rc::new(RefCell::new(Vec::new())));
        let model = ScriptedModelAdapter::new(vec![
            ModelDecision::call_tool("run_tests".to_owned(), Vec::new()).expect("only decision"),
        ])
        .expect("valid script");
        let mut runtime = AgentRuntime::new(model, catalog, gate, AgentConfig::default());
        let task_id = submit(&mut supervisor, "agent-model-error", &[], &["test_runner"]);
        let error = match runtime.start(&mut supervisor, task_id) {
            Err(error) => error,
            Ok(_) => panic!("exhausted model must fail"),
        };
        assert!(matches!(error, AgentError::ModelFailed));
        assert_eq!(format!("{error:?}"), "model adapter failed");
        assert_eq!(
            supervisor.get(task_id).expect("Task exists").state,
            TaskState::Failed
        );
    }

    #[test]
    fn one_active_approval_wait_bounds_concurrent_agent_sessions() {
        let (catalog, gate) = tools(Rc::new(RefCell::new(Vec::new())));
        let model = ScriptedModelAdapter::new(vec![
            ModelDecision::call_tool("run_tests".to_owned(), Vec::new()).expect("valid decision"),
        ])
        .expect("valid script");
        let mut runtime = AgentRuntime::new(model, catalog, gate, AgentConfig::default());
        let mut supervisor = TaskSupervisor::default();
        let first = submit(
            &mut supervisor,
            "agent-first",
            &["test.run"],
            &["test_runner"],
        );
        let second = submit(&mut supervisor, "agent-second", &[], &["test_runner"]);

        assert!(matches!(
            runtime.start(&mut supervisor, first),
            Ok(AgentRunOutcome::WaitingApproval(_))
        ));
        assert!(matches!(
            runtime.start(&mut supervisor, second),
            Err(AgentError::CapacityExceeded)
        ));
        assert_eq!(
            supervisor.get(second).expect("Task exists").state,
            TaskState::Queued
        );
        assert!(
            runtime
                .cancel(&mut supervisor, first)
                .expect("cancel first Task")
        );
        assert_eq!(runtime.active_task(), None);
    }
}
