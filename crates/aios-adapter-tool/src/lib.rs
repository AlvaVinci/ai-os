//! Bounded in-process Tool Adapter for AI OS.
//!
//! This crate never invokes a shell, searches `PATH`, or starts a process. Trusted startup code
//! registers typed handlers and maps model-visible route names to fixed capability tool and action
//! identifiers. [`ToolCatalog`] may then prepare bounded operations for [`ToolExecutionGate`].

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::time::Duration;

use aios_core::CapabilityRequest;
use aios_runtime::{
    ApprovalId, EventStore, Executed, ExecutionAdapter, ExecutionError, ExecutionGate,
    ExecutionOutcome, GuardedOperation, TaskId, TaskSnapshot, TaskSupervisor,
};

pub const DEFAULT_MAX_ROUTES: usize = 256;
pub const MAX_MAX_ROUTES: usize = 4_096;
pub const MAX_IDENTIFIER_BYTES: usize = 64;
pub const MAX_ARGUMENTS: usize = 64;
pub const MAX_ARGUMENT_BYTES: usize = 4_096;
pub const MAX_TOTAL_ARGUMENT_BYTES: usize = 64 * 1_024;
pub const MAX_OUTPUT_BYTES: usize = 1_024 * 1_024;

#[derive(Clone, Eq, PartialEq)]
struct RouteDefinition {
    capability_tool: String,
    action: String,
}

/// Complete Tool operation retained by `ExecutionGate` while approval is pending.
///
/// This type intentionally does not implement `Clone`, `Debug`, or serialization. Operations can
/// only be constructed by a catalog built from the same trusted registration format as an adapter.
pub struct ToolOperation {
    route: String,
    capability_tool: String,
    action: String,
    arguments: Vec<String>,
}

impl GuardedOperation for ToolOperation {
    fn capability_request(&self) -> CapabilityRequest<'_> {
        CapabilityRequest::Tool {
            tool: &self.capability_tool,
            action: &self.action,
        }
    }
}

/// Bounded opaque Tool output. It intentionally has no `Debug` or serialization implementation.
pub struct ToolOutput(Vec<u8>);

impl ToolOutput {
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, ToolAdapterError> {
        if bytes.len() > MAX_OUTPUT_BYTES {
            return Err(ToolAdapterError::OutputTooLarge);
        }
        Ok(Self(bytes))
    }

    pub fn from_text(text: String) -> Result<Self, ToolAdapterError> {
        Self::from_bytes(text.into_bytes())
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

/// Redacted failure returned by a Tool handler.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ToolFailure;

impl ToolFailure {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Display for ToolFailure {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str("tool handler failed")
    }
}

impl Error for ToolFailure {}

/// In-process handler registered by trusted startup code.
pub trait ToolHandler {
    fn execute(&mut self, arguments: Vec<String>) -> Result<ToolOutput, ToolFailure>;
}

impl<F> ToolHandler for F
where
    F: FnMut(Vec<String>) -> Result<ToolOutput, ToolFailure>,
{
    fn execute(&mut self, arguments: Vec<String>) -> Result<ToolOutput, ToolFailure> {
        self(arguments)
    }
}

/// Stable Tool Adapter failure categories without route, argument, or handler values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolAdapterError {
    InvalidConfig,
    InvalidRegistration,
    CapacityExceeded,
    DuplicateRoute,
    RouteNotFound,
    InvalidArguments,
    ScopeMismatch,
    HandlerFailed,
    OutputTooLarge,
}

impl Display for ToolAdapterError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidConfig => "invalid Tool Adapter configuration",
            Self::InvalidRegistration => "invalid Tool registration",
            Self::CapacityExceeded => "Tool route capacity exceeded",
            Self::DuplicateRoute => "Tool route already registered",
            Self::RouteNotFound => "Tool route not found",
            Self::InvalidArguments => "Tool arguments are invalid",
            Self::ScopeMismatch => "Tool operation does not match the registered scope",
            Self::HandlerFailed => "Tool handler failed",
            Self::OutputTooLarge => "Tool output exceeds the configured limit",
        };
        formatter.write_str(message)
    }
}

impl Error for ToolAdapterError {}

/// Trusted builder that creates a matching catalog and private handler adapter.
pub struct ToolAdapterBuilder {
    definitions: BTreeMap<String, RouteDefinition>,
    handlers: BTreeMap<String, Box<dyn ToolHandler>>,
    max_routes: usize,
}

impl ToolAdapterBuilder {
    pub fn new(max_routes: usize) -> Result<Self, ToolAdapterError> {
        if max_routes == 0 || max_routes > MAX_MAX_ROUTES {
            return Err(ToolAdapterError::InvalidConfig);
        }
        Ok(Self {
            definitions: BTreeMap::new(),
            handlers: BTreeMap::new(),
            max_routes,
        })
    }

    /// Registers one model-visible route with fixed capability identifiers and one handler.
    pub fn register<H>(
        &mut self,
        route: &str,
        capability_tool: &str,
        action: &str,
        handler: H,
    ) -> Result<(), ToolAdapterError>
    where
        H: ToolHandler + 'static,
    {
        if !is_valid_identifier(route)
            || !is_valid_identifier(capability_tool)
            || !is_valid_identifier(action)
        {
            return Err(ToolAdapterError::InvalidRegistration);
        }
        if self.definitions.contains_key(route) {
            return Err(ToolAdapterError::DuplicateRoute);
        }
        if self.definitions.len() >= self.max_routes {
            return Err(ToolAdapterError::CapacityExceeded);
        }

        self.definitions.insert(
            route.to_owned(),
            RouteDefinition {
                capability_tool: capability_tool.to_owned(),
                action: action.to_owned(),
            },
        );
        self.handlers.insert(route.to_owned(), Box::new(handler));
        Ok(())
    }

    #[must_use]
    pub fn build(self) -> (ToolCatalog, ToolExecutionGate) {
        let catalog = ToolCatalog {
            definitions: self.definitions.clone(),
        };
        let adapter = ToolAdapter {
            definitions: self.definitions,
            handlers: self.handlers,
        };
        (
            catalog,
            ToolExecutionGate {
                gate: ExecutionGate::new(adapter),
            },
        )
    }
}

impl Default for ToolAdapterBuilder {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_ROUTES).expect("default Tool Adapter limits must be valid")
    }
}

/// Read-only route catalog safe to expose to operation preparation code.
pub struct ToolCatalog {
    definitions: BTreeMap<String, RouteDefinition>,
}

impl ToolCatalog {
    /// Creates a bounded operation using capability identifiers fixed at registration time.
    pub fn prepare(
        &self,
        route: &str,
        arguments: Vec<String>,
    ) -> Result<ToolOperation, ToolAdapterError> {
        let definition = self
            .definitions
            .get(route)
            .ok_or(ToolAdapterError::RouteNotFound)?;
        validate_arguments(&arguments)?;

        Ok(ToolOperation {
            route: route.to_owned(),
            capability_tool: definition.capability_tool.clone(),
            action: definition.action.clone(),
            arguments,
        })
    }

    #[must_use]
    pub fn route_count(&self) -> usize {
        self.definitions.len()
    }

    /// Returns only routes whose fixed Capability Tool is granted to the Task.
    pub fn route_names_for_tools<'a>(
        &'a self,
        capability_tools: &'a [String],
    ) -> impl Iterator<Item = &'a str> {
        self.definitions
            .iter()
            .filter(move |(_, definition)| {
                capability_tools
                    .iter()
                    .any(|tool| tool == &definition.capability_tool)
            })
            .map(|(route, _)| route.as_str())
    }
}

struct ToolAdapter {
    definitions: BTreeMap<String, RouteDefinition>,
    handlers: BTreeMap<String, Box<dyn ToolHandler>>,
}

/// Guarded Tool execution boundary that never exposes its raw handler adapter.
pub struct ToolExecutionGate {
    gate: ExecutionGate<ToolAdapter, ToolOperation>,
}

impl ToolExecutionGate {
    pub fn request<S: EventStore>(
        &mut self,
        supervisor: &mut TaskSupervisor<S>,
        task_id: TaskId,
        operation: ToolOperation,
        approval_ttl: Duration,
    ) -> Result<ExecutionOutcome<ToolOutput>, ExecutionError<ToolAdapterError>> {
        self.gate
            .request(supervisor, task_id, operation, approval_ttl)
    }

    pub fn approve_and_execute<S: EventStore>(
        &mut self,
        supervisor: &mut TaskSupervisor<S>,
        approval_id: ApprovalId,
    ) -> Result<Executed<ToolOutput>, ExecutionError<ToolAdapterError>> {
        self.gate.approve_and_execute(supervisor, approval_id)
    }

    pub fn deny<S: EventStore>(
        &mut self,
        supervisor: &mut TaskSupervisor<S>,
        approval_id: ApprovalId,
    ) -> Result<TaskSnapshot, ExecutionError<ToolAdapterError>> {
        self.gate.deny(supervisor, approval_id)
    }

    pub fn expire<S: EventStore>(
        &mut self,
        supervisor: &mut TaskSupervisor<S>,
    ) -> Result<usize, ExecutionError<ToolAdapterError>> {
        self.gate.expire(supervisor)
    }

    pub fn cancel<S: EventStore>(
        &mut self,
        supervisor: &mut TaskSupervisor<S>,
        task_id: TaskId,
    ) -> Result<bool, ExecutionError<ToolAdapterError>> {
        self.gate.cancel(supervisor, task_id)
    }

    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.gate.pending_count()
    }
}

impl ExecutionAdapter<ToolOperation> for ToolAdapter {
    type Output = ToolOutput;
    type Error = ToolAdapterError;

    fn execute(&mut self, operation: ToolOperation) -> Result<Self::Output, Self::Error> {
        validate_arguments(&operation.arguments)?;
        let definition = self
            .definitions
            .get(&operation.route)
            .ok_or(ToolAdapterError::RouteNotFound)?;
        if definition.capability_tool != operation.capability_tool
            || definition.action != operation.action
        {
            return Err(ToolAdapterError::ScopeMismatch);
        }
        let handler = self
            .handlers
            .get_mut(&operation.route)
            .ok_or(ToolAdapterError::RouteNotFound)?;
        handler
            .execute(operation.arguments)
            .map_err(|_| ToolAdapterError::HandlerFailed)
    }
}

fn is_valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_IDENTIFIER_BYTES
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | ':' | '-')
        })
}

fn validate_arguments(arguments: &[String]) -> Result<(), ToolAdapterError> {
    if arguments.len() > MAX_ARGUMENTS {
        return Err(ToolAdapterError::InvalidArguments);
    }
    let mut total_bytes = 0_usize;
    for argument in arguments {
        if argument.len() > MAX_ARGUMENT_BYTES || argument.contains('\0') {
            return Err(ToolAdapterError::InvalidArguments);
        }
        total_bytes = total_bytes
            .checked_add(argument.len())
            .ok_or(ToolAdapterError::InvalidArguments)?;
        if total_bytes > MAX_TOTAL_ARGUMENT_BYTES {
            return Err(ToolAdapterError::InvalidArguments);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    use aios_core::{ApprovalPolicy, Budget, CapabilitySet, NetworkPolicy, TaskSpec};
    use aios_runtime::{ExecutionError, ExecutionOutcome, SubmitResult, TaskSupervisor};

    use super::{
        MAX_ARGUMENT_BYTES, MAX_ARGUMENTS, MAX_OUTPUT_BYTES, ToolAdapterBuilder, ToolAdapterError,
        ToolFailure, ToolOutput,
    };

    fn task(required_for: &[&str], tools: &[&str]) -> TaskSpec {
        TaskSpec {
            idempotency_key: "tool-adapter-test".to_owned(),
            goal: "Run one registered Tool handler".to_owned(),
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

    fn running_supervisor(
        required_for: &[&str],
        tools: &[&str],
    ) -> (TaskSupervisor, aios_runtime::TaskId) {
        let mut supervisor = TaskSupervisor::default();
        let SubmitResult::Accepted(task) = supervisor
            .submit(task(required_for, tools))
            .expect("submit Task")
        else {
            panic!("expected accepted Task");
        };
        supervisor.start(task.task_id).expect("start Task");
        (supervisor, task.task_id)
    }

    fn configured_adapter(
        seen: Rc<RefCell<Vec<Vec<String>>>>,
    ) -> (super::ToolCatalog, super::ToolExecutionGate) {
        let mut builder = ToolAdapterBuilder::default();
        builder
            .register(
                "run_tests",
                "test_runner",
                "test.run",
                move |arguments: Vec<String>| {
                    seen.borrow_mut().push(arguments);
                    ToolOutput::from_text("passed".to_owned()).map_err(|_| ToolFailure::new())
                },
            )
            .expect("register Tool");
        builder.build()
    }

    #[test]
    fn executes_registered_route_without_interpreting_arguments() {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let (catalog, mut gate) = configured_adapter(Rc::clone(&seen));
        let (mut supervisor, task_id) = running_supervisor(&[], &["test_runner"]);
        let operation = catalog
            .prepare(
                "run_tests",
                vec!["--filter=a b".to_owned(), "literal;$value".to_owned()],
            )
            .expect("prepare Tool");

        let result = gate
            .request(&mut supervisor, task_id, operation, Duration::from_secs(30))
            .expect("execute Tool");

        let ExecutionOutcome::Executed(executed) = result else {
            panic!("expected execution");
        };
        assert_eq!(executed.output.as_bytes(), b"passed");
        assert_eq!(
            seen.borrow().as_slice(),
            &[vec!["--filter=a b".to_owned(), "literal;$value".to_owned()]]
        );
    }

    #[test]
    fn approval_executes_only_the_catalog_prepared_operation() {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let (catalog, mut gate) = configured_adapter(Rc::clone(&seen));
        let (mut supervisor, task_id) = running_supervisor(&["test.run"], &["test_runner"]);
        let operation = catalog
            .prepare("run_tests", vec!["original".to_owned()])
            .expect("prepare Tool");
        let ExecutionOutcome::ApprovalRequired(request) = gate
            .request(&mut supervisor, task_id, operation, Duration::from_secs(30))
            .expect("request approval")
        else {
            panic!("expected approval request");
        };

        let executed = gate
            .approve_and_execute(&mut supervisor, request.approval_id)
            .expect("approve and execute");

        assert_eq!(executed.output.as_bytes(), b"passed");
        assert_eq!(seen.borrow().as_slice(), &[vec!["original".to_owned()]]);
        assert_eq!(gate.pending_count(), 0);
    }

    #[test]
    fn capability_denial_never_calls_handler() {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let (catalog, mut gate) = configured_adapter(Rc::clone(&seen));
        let (mut supervisor, task_id) = running_supervisor(&[], &["different_tool"]);
        let operation = catalog
            .prepare("run_tests", Vec::new())
            .expect("prepare Tool");

        let result = gate
            .request(&mut supervisor, task_id, operation, Duration::from_secs(30))
            .expect("deny Tool");

        assert!(matches!(result, ExecutionOutcome::Denied { .. }));
        assert!(seen.borrow().is_empty());
    }

    #[test]
    fn rejects_unknown_duplicate_and_excess_routes_without_echoing_values() {
        let builder = ToolAdapterBuilder::new(1).expect("valid limit");
        let (catalog, _) = builder.build();
        let Err(error) = catalog.prepare("secret-route", Vec::new()) else {
            panic!("unknown route must fail");
        };
        assert_eq!(error, ToolAdapterError::RouteNotFound);
        assert!(!error.to_string().contains("secret-route"));

        let mut builder = ToolAdapterBuilder::new(1).expect("valid limit");
        builder
            .register("route", "tool", "action", |_| {
                ToolOutput::from_text(String::new()).map_err(|_| ToolFailure::new())
            })
            .expect("register route");
        assert_eq!(
            builder.register("route", "tool", "action", |_| {
                ToolOutput::from_text(String::new()).map_err(|_| ToolFailure::new())
            }),
            Err(ToolAdapterError::DuplicateRoute)
        );
        assert_eq!(
            builder.register("second", "tool", "action", |_| {
                ToolOutput::from_text(String::new()).map_err(|_| ToolFailure::new())
            }),
            Err(ToolAdapterError::CapacityExceeded)
        );
    }

    #[test]
    fn rejects_unbounded_or_nul_arguments() {
        let (catalog, _) = configured_adapter(Rc::new(RefCell::new(Vec::new())));

        assert!(matches!(
            catalog.prepare("run_tests", vec![String::new(); MAX_ARGUMENTS + 1]),
            Err(ToolAdapterError::InvalidArguments)
        ));
        assert!(matches!(
            catalog.prepare("run_tests", vec!["x".repeat(MAX_ARGUMENT_BYTES + 1)]),
            Err(ToolAdapterError::InvalidArguments)
        ));
        assert!(matches!(
            catalog.prepare("run_tests", vec!["before\0after".to_owned()]),
            Err(ToolAdapterError::InvalidArguments)
        ));
    }

    #[test]
    fn rejects_oversized_output_and_redacts_handler_failure() {
        assert_eq!(
            ToolOutput::from_bytes(vec![0; MAX_OUTPUT_BYTES + 1]).err(),
            Some(ToolAdapterError::OutputTooLarge)
        );

        let mut builder = ToolAdapterBuilder::default();
        builder
            .register("fail", "test_runner", "test.run", |_| {
                Err(ToolFailure::new())
            })
            .expect("register Tool");
        let (catalog, mut gate) = builder.build();
        let (mut supervisor, task_id) = running_supervisor(&[], &["test_runner"]);
        let operation = catalog.prepare("fail", Vec::new()).expect("prepare Tool");

        let error = match gate.request(&mut supervisor, task_id, operation, Duration::from_secs(30))
        {
            Err(error) => error,
            Ok(_) => panic!("handler must fail"),
        };

        assert_eq!(error.to_string(), "adapter execution failed");
        assert_eq!(format!("{error:?}"), "adapter execution failed");
    }

    #[test]
    fn rejects_invalid_builder_configuration_and_registration() {
        assert!(matches!(
            ToolAdapterBuilder::new(0),
            Err(ToolAdapterError::InvalidConfig)
        ));
        let mut builder = ToolAdapterBuilder::default();
        assert_eq!(
            builder.register("bad route", "tool", "action", |_| {
                ToolOutput::from_text(String::new()).map_err(|_| ToolFailure::new())
            }),
            Err(ToolAdapterError::InvalidRegistration)
        );
    }

    #[test]
    fn adapter_rejects_operation_from_a_different_catalog_scope() {
        let mut first = ToolAdapterBuilder::default();
        first
            .register("route", "test_runner", "test.run", |_| {
                ToolOutput::from_text("first".to_owned()).map_err(|_| ToolFailure::new())
            })
            .expect("register first Tool");
        let (catalog, _) = first.build();

        let called = Rc::new(RefCell::new(false));
        let called_by_handler = Rc::clone(&called);
        let mut second = ToolAdapterBuilder::default();
        second
            .register("route", "test_runner", "test.write", move |_| {
                *called_by_handler.borrow_mut() = true;
                ToolOutput::from_text("second".to_owned()).map_err(|_| ToolFailure::new())
            })
            .expect("register second Tool");
        let (_, mut gate) = second.build();
        let (mut supervisor, task_id) = running_supervisor(&[], &["test_runner"]);
        let operation = catalog.prepare("route", Vec::new()).expect("prepare Tool");

        let result = gate.request(&mut supervisor, task_id, operation, Duration::from_secs(30));

        assert!(matches!(
            result,
            Err(ExecutionError::Adapter(ToolAdapterError::ScopeMismatch))
        ));
        assert!(!*called.borrow());
    }
}
