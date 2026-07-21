//! Bounded Agent execution and model adapter contracts for AI OS.
//!
//! Model decisions are untrusted proposals. [`AgentRuntime`] exposes only model-visible Tool route
//! names, prepares operations through the trusted Tool catalog, and executes them through the
//! Capability and approval gate.

use std::collections::VecDeque;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::time::Duration;

use aios_adapter_tool::{
    MAX_ARGUMENT_BYTES, MAX_ARGUMENTS, MAX_IDENTIFIER_BYTES, MAX_TOTAL_ARGUMENT_BYTES,
    ToolAdapterError, ToolCatalog, ToolExecutionGate, ToolOutput,
};
use aios_core::DenialReason;
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
}

/// Sensitive input for one bounded model turn.
///
/// Only the immediately preceding Tool output is exposed. This type intentionally does not
/// implement `Debug` or serialization.
pub struct ModelTurnRequest<'a> {
    step: u16,
    previous_tool_output: Option<&'a [u8]>,
}

impl<'a> ModelTurnRequest<'a> {
    #[must_use]
    pub const fn step(&self) -> u16 {
        self.step
    }

    #[must_use]
    pub const fn previous_tool_output(&self) -> Option<&[u8]> {
        self.previous_tool_output
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
}

/// Stable validation category for one untrusted model proposal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelDecisionError {
    InvalidFinalOutput,
    InvalidToolRequest,
}

impl Display for ModelDecisionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidFinalOutput => "invalid final model output",
            Self::InvalidToolRequest => "invalid model Tool request",
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
    previous_tool_output: Option<ToolOutput>,
    pending_approval: Option<ApprovalId>,
}

/// Synchronous single-Task Agent runtime with no raw Tool adapter escape hatch.
pub struct AgentRuntime<M: ModelAdapter> {
    model_adapter: M,
    tool_catalog: ToolCatalog,
    tool_gate: ToolExecutionGate,
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
            config,
            active: None,
        }
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
        let model = match self.model_adapter.start_session(ModelStartRequest {
            goal: input.goal(),
            tool_routes: &routes,
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
            previous_tool_output: None,
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
        let task_id = self.require_pending(approval_id)?;
        match self.tool_gate.approve_and_execute(supervisor, approval_id) {
            Ok(executed) => {
                let active = self.active.as_mut().ok_or(AgentError::InvalidState)?;
                active.pending_approval = None;
                active.previous_tool_output = Some(executed.output);
                self.drive(supervisor)
            }
            Err(ExecutionError::Supervisor(error)) => Err(AgentError::Supervisor(error)),
            Err(ExecutionError::Adapter(_) | ExecutionError::OperationNotFound) => {
                self.fail_active(supervisor, task_id)?;
                Err(AgentError::ToolFailed)
            }
        }
    }

    /// Denies one exact pending operation and drops the associated model session.
    pub fn deny<S: EventStore>(
        &mut self,
        supervisor: &mut TaskSupervisor<S>,
        approval_id: ApprovalId,
    ) -> Result<(), AgentError> {
        let _task_id = self.require_pending(approval_id)?;
        self.tool_gate
            .deny(supervisor, approval_id)
            .map_err(map_tool_error)?;
        self.active = None;
        Ok(())
    }

    /// Expires pending approvals and drops a model session whose Task became terminal.
    pub fn expire<S: EventStore>(
        &mut self,
        supervisor: &mut TaskSupervisor<S>,
    ) -> Result<usize, AgentError> {
        let expired = self.tool_gate.expire(supervisor).map_err(map_tool_error)?;
        if self.active.as_ref().is_some_and(|active| {
            supervisor
                .get(active.task_id)
                .is_some_and(|task| task.state.is_terminal())
        }) {
            self.active = None;
        }
        Ok(expired)
    }

    /// Cancels one Task through the Tool gate and drops its model session.
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
        let changed = self
            .tool_gate
            .cancel(supervisor, task_id)
            .map_err(map_tool_error)?;
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
            if next_step > self.config.max_steps {
                self.fail_active(supervisor, task_id)?;
                return Err(AgentError::StepLimitExceeded);
            }

            let decision = {
                let active = self.active.as_mut().ok_or(AgentError::InvalidState)?;
                let request = ModelTurnRequest {
                    step: active.next_step,
                    previous_tool_output: active
                        .previous_tool_output
                        .as_ref()
                        .map(ToolOutput::as_bytes),
                };
                match active.model.decide(request) {
                    Ok(decision) => decision,
                    Err(_) => {
                        self.fail_active(supervisor, task_id)?;
                        return Err(AgentError::ModelFailed);
                    }
                }
            };
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
                                .previous_tool_output = Some(executed.output);
                        }
                        Ok(ExecutionOutcome::Denied { reason }) => {
                            self.fail_active(supervisor, task_id)?;
                            return Ok(AgentRunOutcome::Denied { reason });
                        }
                        Ok(ExecutionOutcome::ApprovalRequired(request)) => {
                            self.active
                                .as_mut()
                                .ok_or(AgentError::InvalidState)?
                                .pending_approval = Some(request.approval_id);
                            return Ok(AgentRunOutcome::WaitingApproval(request));
                        }
                        Err(ExecutionError::Supervisor(error)) => {
                            self.active = None;
                            return Err(AgentError::Supervisor(error));
                        }
                        Err(ExecutionError::Adapter(_) | ExecutionError::OperationNotFound) => {
                            self.fail_active(supervisor, task_id)?;
                            return Err(AgentError::ToolFailed);
                        }
                    }
                }
            }
        }
    }

    fn require_pending(&self, approval_id: ApprovalId) -> Result<TaskId, AgentError> {
        let active = self.active.as_ref().ok_or(AgentError::InvalidState)?;
        if active.pending_approval != Some(approval_id) {
            return Err(AgentError::InvalidState);
        }
        Ok(active.task_id)
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
    Supervisor(SupervisorError),
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
        ExecutionError::Adapter(_) | ExecutionError::OperationNotFound => AgentError::ToolFailed,
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
    use std::rc::Rc;
    use std::time::Duration;

    use aios_adapter_tool::{ToolAdapterBuilder, ToolFailure, ToolOutput};
    use aios_core::{ApprovalPolicy, Budget, CapabilitySet, NetworkPolicy, TaskSpec, TaskState};
    use aios_runtime::{ApprovalId, InMemoryEventStore, SubmitResult, TaskSupervisor};

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

    fn submit(
        supervisor: &mut TaskSupervisor,
        idempotency_key: &str,
        required_for: &[&str],
        tools: &[&str],
    ) -> aios_runtime::TaskId {
        let SubmitResult::Accepted(task) = supervisor
            .submit(task(idempotency_key, required_for, tools))
            .expect("submit Task")
        else {
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
