//! Bounded child-process Tool handler for AI OS.
//!
//! The handler executes one trusted, explicitly configured absolute executable. It never invokes
//! a shell or searches `PATH`. Dynamic arguments are accepted only after a trusted policy approves
//! them, and the child receives an empty environment except for explicitly configured values.
//!
//! This crate is not an operating-system sandbox. In particular, it does not yet provide a
//! separate principal, descriptor allowlist, namespaces, cgroups, network isolation, or reliable
//! descendant-process termination.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use aios_adapter_tool::{ToolFailure, ToolHandler, ToolOutput};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
pub const MAX_TIMEOUT: Duration = Duration::from_secs(60 * 60);
pub const MAX_ARGUMENTS: usize = 64;
pub const MAX_ARGUMENT_BYTES: usize = 4_096;
pub const MAX_TOTAL_ARGUMENT_BYTES: usize = 64 * 1_024;
pub const MAX_ENVIRONMENT_VARIABLES: usize = 64;
pub const MAX_ENVIRONMENT_NAME_BYTES: usize = 128;
pub const MAX_ENVIRONMENT_VALUE_BYTES: usize = 4_096;
pub const MAX_TOTAL_ENVIRONMENT_BYTES: usize = 64 * 1_024;

const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Trusted policy for model-controlled arguments passed to one configured executable.
///
/// The policy receives only dynamic arguments. Fixed arguments are supplied by trusted startup
/// configuration and are validated separately for size and NUL bytes.
pub trait ProcessArgumentPolicy {
    fn allows(&self, arguments: &[String]) -> bool;
}

impl<F> ProcessArgumentPolicy for F
where
    F: Fn(&[String]) -> bool,
{
    fn allows(&self, arguments: &[String]) -> bool {
        self(arguments)
    }
}

/// Stable, redacted child-process failure categories.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessAdapterError {
    InvalidConfig,
    InvalidArguments,
    ArgumentsDenied,
    ExecutableChanged,
    SpawnFailed,
    WaitFailed,
    TimedOut,
    ExitFailed,
    OutputFailed,
}

impl Display for ProcessAdapterError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidConfig => "invalid Process Adapter configuration",
            Self::InvalidArguments => "invalid process arguments",
            Self::ArgumentsDenied => "process arguments denied",
            Self::ExecutableChanged => "configured executable identity changed",
            Self::SpawnFailed => "process spawn failed",
            Self::WaitFailed => "process wait failed",
            Self::TimedOut => "process timed out",
            Self::ExitFailed => "process exited unsuccessfully",
            Self::OutputFailed => "process output construction failed",
        };
        formatter.write_str(message)
    }
}

impl Error for ProcessAdapterError {}

/// Trusted builder for one fixed child-process Tool handler.
///
/// This type intentionally does not implement `Debug` or serialization because environment values
/// may be sensitive. The executable and working directory are canonicalized during `build`.
pub struct ProcessToolBuilder {
    executable: PathBuf,
    working_directory: PathBuf,
    fixed_arguments: Vec<String>,
    environment: Vec<(String, String)>,
    timeout: Duration,
    argument_policy: Box<dyn ProcessArgumentPolicy>,
}

impl ProcessToolBuilder {
    pub fn new<P>(
        executable: impl Into<PathBuf>,
        working_directory: impl Into<PathBuf>,
        argument_policy: P,
    ) -> Self
    where
        P: ProcessArgumentPolicy + 'static,
    {
        Self {
            executable: executable.into(),
            working_directory: working_directory.into(),
            fixed_arguments: Vec::new(),
            environment: Vec::new(),
            timeout: DEFAULT_TIMEOUT,
            argument_policy: Box::new(argument_policy),
        }
    }

    #[must_use]
    pub fn fixed_arguments(mut self, arguments: Vec<String>) -> Self {
        self.fixed_arguments = arguments;
        self
    }

    #[must_use]
    pub fn environment(mut self, environment: Vec<(String, String)>) -> Self {
        self.environment = environment;
        self
    }

    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn build(self) -> Result<ProcessToolHandler, ProcessAdapterError> {
        if self.timeout.is_zero() || self.timeout > MAX_TIMEOUT {
            return Err(ProcessAdapterError::InvalidConfig);
        }
        validate_arguments(&self.fixed_arguments, &[])?;

        let executable = canonical_executable(&self.executable)?;
        let executable_identity = ExecutableIdentity::read(&executable)?;
        let working_directory = canonical_directory(&self.working_directory)?;
        let environment = validate_environment(self.environment)?;

        Ok(ProcessToolHandler {
            executable,
            executable_identity,
            working_directory,
            fixed_arguments: self.fixed_arguments,
            environment,
            timeout: self.timeout,
            argument_policy: self.argument_policy,
        })
    }
}

/// A fixed executable that can be registered as an in-process [`ToolHandler`].
///
/// Standard input is null, standard output and standard error are discarded, and the successful
/// Tool output is empty. Discarding process output avoids unbounded pipe buffering until a future
/// isolated process protocol can provide bounded streaming and descendant cleanup.
pub struct ProcessToolHandler {
    executable: PathBuf,
    executable_identity: ExecutableIdentity,
    working_directory: PathBuf,
    fixed_arguments: Vec<String>,
    environment: BTreeMap<String, String>,
    timeout: Duration,
    argument_policy: Box<dyn ProcessArgumentPolicy>,
}

impl ProcessToolHandler {
    /// Executes one validated argument vector and returns a redacted failure category.
    pub fn run_checked(
        &mut self,
        dynamic_arguments: Vec<String>,
    ) -> Result<ToolOutput, ProcessAdapterError> {
        validate_arguments(&self.fixed_arguments, &dynamic_arguments)?;
        if !self.argument_policy.allows(&dynamic_arguments) {
            return Err(ProcessAdapterError::ArgumentsDenied);
        }
        if ExecutableIdentity::read(&self.executable)? != self.executable_identity {
            return Err(ProcessAdapterError::ExecutableChanged);
        }

        let mut command = Command::new(&self.executable);
        command
            .args(&self.fixed_arguments)
            .args(dynamic_arguments)
            .current_dir(&self.working_directory)
            .env_clear()
            .envs(&self.environment)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }

        let deadline = Instant::now()
            .checked_add(self.timeout)
            .ok_or(ProcessAdapterError::InvalidConfig)?;
        let mut child = command
            .spawn()
            .map_err(|_| ProcessAdapterError::SpawnFailed)?;

        loop {
            match child
                .try_wait()
                .map_err(|_| ProcessAdapterError::WaitFailed)?
            {
                Some(status) if status.success() => {
                    return ToolOutput::from_bytes(Vec::new())
                        .map_err(|_| ProcessAdapterError::OutputFailed);
                }
                Some(_) => return Err(ProcessAdapterError::ExitFailed),
                None => {}
            }

            let now = Instant::now();
            if now >= deadline {
                if child.kill().is_ok() {
                    child.wait().map_err(|_| ProcessAdapterError::WaitFailed)?;
                } else if child
                    .try_wait()
                    .map_err(|_| ProcessAdapterError::WaitFailed)?
                    .is_none()
                {
                    return Err(ProcessAdapterError::WaitFailed);
                }
                return Err(ProcessAdapterError::TimedOut);
            }
            thread::sleep(WAIT_POLL_INTERVAL.min(deadline.saturating_duration_since(now)));
        }
    }
}

impl ToolHandler for ProcessToolHandler {
    fn execute(&mut self, arguments: Vec<String>) -> Result<ToolOutput, ToolFailure> {
        self.run_checked(arguments).map_err(|_| ToolFailure::new())
    }
}

#[cfg(unix)]
#[derive(Eq, PartialEq)]
struct ExecutableIdentity {
    device: u64,
    inode: u64,
}

#[cfg(unix)]
impl ExecutableIdentity {
    fn read(path: &Path) -> Result<Self, ProcessAdapterError> {
        use std::os::unix::fs::MetadataExt;

        let metadata = fs::metadata(path).map_err(|_| ProcessAdapterError::ExecutableChanged)?;
        if !metadata.is_file() || metadata.mode() & 0o111 == 0 {
            return Err(ProcessAdapterError::ExecutableChanged);
        }
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}

#[cfg(not(unix))]
#[derive(Eq, PartialEq)]
struct ExecutableIdentity;

#[cfg(not(unix))]
impl ExecutableIdentity {
    fn read(path: &Path) -> Result<Self, ProcessAdapterError> {
        let metadata = fs::metadata(path).map_err(|_| ProcessAdapterError::ExecutableChanged)?;
        if !metadata.is_file() {
            return Err(ProcessAdapterError::ExecutableChanged);
        }
        Ok(Self)
    }
}

fn canonical_executable(path: &Path) -> Result<PathBuf, ProcessAdapterError> {
    if !path.is_absolute() {
        return Err(ProcessAdapterError::InvalidConfig);
    }
    let canonical = fs::canonicalize(path).map_err(|_| ProcessAdapterError::InvalidConfig)?;
    ExecutableIdentity::read(&canonical).map_err(|_| ProcessAdapterError::InvalidConfig)?;
    Ok(canonical)
}

fn canonical_directory(path: &Path) -> Result<PathBuf, ProcessAdapterError> {
    if !path.is_absolute() {
        return Err(ProcessAdapterError::InvalidConfig);
    }
    let canonical = fs::canonicalize(path).map_err(|_| ProcessAdapterError::InvalidConfig)?;
    if !canonical.is_dir() {
        return Err(ProcessAdapterError::InvalidConfig);
    }
    Ok(canonical)
}

fn validate_arguments(fixed: &[String], dynamic: &[String]) -> Result<(), ProcessAdapterError> {
    let count = fixed
        .len()
        .checked_add(dynamic.len())
        .ok_or(ProcessAdapterError::InvalidArguments)?;
    if count > MAX_ARGUMENTS {
        return Err(ProcessAdapterError::InvalidArguments);
    }

    let mut total_bytes = 0_usize;
    for argument in fixed.iter().chain(dynamic) {
        if argument.len() > MAX_ARGUMENT_BYTES || argument.contains('\0') {
            return Err(ProcessAdapterError::InvalidArguments);
        }
        total_bytes = total_bytes
            .checked_add(argument.len())
            .ok_or(ProcessAdapterError::InvalidArguments)?;
        if total_bytes > MAX_TOTAL_ARGUMENT_BYTES {
            return Err(ProcessAdapterError::InvalidArguments);
        }
    }
    Ok(())
}

fn validate_environment(
    entries: Vec<(String, String)>,
) -> Result<BTreeMap<String, String>, ProcessAdapterError> {
    if entries.len() > MAX_ENVIRONMENT_VARIABLES {
        return Err(ProcessAdapterError::InvalidConfig);
    }

    let mut environment = BTreeMap::new();
    let mut total_bytes = 0_usize;
    for (name, value) in entries {
        if !is_valid_environment_name(&name)
            || value.len() > MAX_ENVIRONMENT_VALUE_BYTES
            || value.contains('\0')
        {
            return Err(ProcessAdapterError::InvalidConfig);
        }
        total_bytes = total_bytes
            .checked_add(name.len())
            .and_then(|total| total.checked_add(value.len()))
            .ok_or(ProcessAdapterError::InvalidConfig)?;
        if total_bytes > MAX_TOTAL_ENVIRONMENT_BYTES || environment.insert(name, value).is_some() {
            return Err(ProcessAdapterError::InvalidConfig);
        }
    }
    Ok(environment)
}

fn is_valid_environment_name(name: &str) -> bool {
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    name.len() <= MAX_ENVIRONMENT_NAME_BYTES
        && (first.is_ascii_alphabetic() || first == '_')
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

#[cfg(all(test, unix))]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use aios_adapter_tool::ToolAdapterBuilder;
    use aios_core::{ApprovalPolicy, Budget, CapabilitySet, NetworkPolicy, TaskSpec};
    use aios_runtime::{ExecutionOutcome, SubmitResult, TaskSupervisor};

    use super::{MAX_ARGUMENT_BYTES, MAX_ARGUMENTS, ProcessAdapterError, ProcessToolBuilder};

    fn executable(candidates: &[&str]) -> PathBuf {
        candidates
            .iter()
            .map(PathBuf::from)
            .find(|path| path.is_file())
            .expect("test executable must exist")
    }

    fn builder<P>(executable: &Path, argument_policy: P) -> ProcessToolBuilder
    where
        P: super::ProcessArgumentPolicy + 'static,
    {
        ProcessToolBuilder::new(executable, Path::new("/"), argument_policy)
    }

    #[test]
    fn executes_an_explicit_program_without_a_shell() {
        let echo = executable(&["/bin/echo", "/usr/bin/echo"]);
        let mut handler = builder(&echo, |arguments: &[String]| arguments.len() == 1)
            .build()
            .expect("build handler");

        let output = handler
            .run_checked(vec!["literal;$PATH".to_owned()])
            .expect("execute direct process");

        assert!(output.as_bytes().is_empty());
    }

    #[test]
    fn requires_absolute_existing_paths_and_bounded_configuration() {
        let relative = ProcessToolBuilder::new("bin/echo", "/", |_: &[String]| true).build();
        assert!(matches!(relative, Err(ProcessAdapterError::InvalidConfig)));

        let echo = executable(&["/bin/echo", "/usr/bin/echo"]);
        let relative_directory = ProcessToolBuilder::new(&echo, ".", |_: &[String]| true).build();
        assert!(matches!(
            relative_directory,
            Err(ProcessAdapterError::InvalidConfig)
        ));

        let invalid_timeout = builder(&echo, |_: &[String]| true)
            .timeout(Duration::ZERO)
            .build();
        assert!(matches!(
            invalid_timeout,
            Err(ProcessAdapterError::InvalidConfig)
        ));
    }

    #[test]
    fn rejects_invalid_or_policy_denied_arguments_before_spawn() {
        let echo = executable(&["/bin/echo", "/usr/bin/echo"]);
        let mut handler = builder(&echo, |arguments: &[String]| arguments.is_empty())
            .build()
            .expect("build handler");

        assert!(matches!(
            handler.run_checked(vec!["denied".to_owned()]),
            Err(ProcessAdapterError::ArgumentsDenied)
        ));
        assert!(matches!(
            handler.run_checked(vec![String::new(); MAX_ARGUMENTS + 1]),
            Err(ProcessAdapterError::InvalidArguments)
        ));
        assert!(matches!(
            handler.run_checked(vec!["x".repeat(MAX_ARGUMENT_BYTES + 1)]),
            Err(ProcessAdapterError::InvalidArguments)
        ));
        assert!(matches!(
            handler.run_checked(vec!["before\0after".to_owned()]),
            Err(ProcessAdapterError::InvalidArguments)
        ));

        let mut fixed = builder(&echo, |_: &[String]| true)
            .fixed_arguments(vec![String::new(); MAX_ARGUMENTS])
            .build()
            .expect("build bounded fixed arguments");
        assert!(matches!(
            fixed.run_checked(vec!["one-too-many".to_owned()]),
            Err(ProcessAdapterError::InvalidArguments)
        ));
    }

    #[test]
    fn clears_the_ambient_environment_and_allows_fixed_entries() {
        let printenv = executable(&["/usr/bin/printenv", "/bin/printenv"]);
        let mut cleared = builder(&printenv, |arguments: &[String]| arguments == ["PATH"])
            .build()
            .expect("build cleared handler");
        assert!(matches!(
            cleared.run_checked(vec!["PATH".to_owned()]),
            Err(ProcessAdapterError::ExitFailed)
        ));

        let mut configured = builder(&printenv, |arguments: &[String]| {
            arguments == ["AIOS_PROCESS_TEST"]
        })
        .environment(vec![(
            "AIOS_PROCESS_TEST".to_owned(),
            "configured".to_owned(),
        )])
        .build()
        .expect("build configured handler");
        configured
            .run_checked(vec!["AIOS_PROCESS_TEST".to_owned()])
            .expect("read configured variable");
    }

    #[test]
    fn rejects_duplicate_or_invalid_environment_entries() {
        let echo = executable(&["/bin/echo", "/usr/bin/echo"]);
        let duplicate = builder(&echo, |_: &[String]| true)
            .environment(vec![
                ("NAME".to_owned(), "first".to_owned()),
                ("NAME".to_owned(), "second".to_owned()),
            ])
            .build();
        assert!(matches!(duplicate, Err(ProcessAdapterError::InvalidConfig)));

        let invalid = builder(&echo, |_: &[String]| true)
            .environment(vec![("BAD=NAME".to_owned(), "value".to_owned())])
            .build();
        assert!(matches!(invalid, Err(ProcessAdapterError::InvalidConfig)));
    }

    #[test]
    fn enforces_timeout_and_nonzero_exit() {
        let sleep = executable(&["/bin/sleep", "/usr/bin/sleep"]);
        let mut timed = builder(&sleep, |arguments: &[String]| arguments == ["1"])
            .timeout(Duration::from_millis(20))
            .build()
            .expect("build timed handler");
        assert!(matches!(
            timed.run_checked(vec!["1".to_owned()]),
            Err(ProcessAdapterError::TimedOut)
        ));

        let false_program = executable(&["/usr/bin/false", "/bin/false"]);
        let mut failing = builder(&false_program, |arguments: &[String]| arguments.is_empty())
            .build()
            .expect("build failing handler");
        assert!(matches!(
            failing.run_checked(Vec::new()),
            Err(ProcessAdapterError::ExitFailed)
        ));
    }

    #[test]
    fn integrates_with_tool_catalog_and_execution_gate() {
        let true_program = executable(&["/usr/bin/true", "/bin/true"]);
        let handler = builder(&true_program, |arguments: &[String]| arguments.is_empty())
            .build()
            .expect("build handler");
        let mut builder = ToolAdapterBuilder::default();
        builder
            .register("run_process", "process_runner", "process.run", handler)
            .expect("register process Tool");
        let (catalog, mut gate) = builder.build();

        let mut supervisor = TaskSupervisor::default();
        let SubmitResult::Accepted(task) = supervisor
            .submit(TaskSpec {
                idempotency_key: "process-adapter-test".to_owned(),
                goal: "Run one fixed process".to_owned(),
                capabilities: CapabilitySet {
                    filesystem: Vec::new(),
                    network: NetworkPolicy::Deny,
                    tools: vec!["process_runner".to_owned()],
                },
                budget: Budget {
                    wall_time_seconds: 60,
                    memory_bytes: 64 * 1024 * 1024,
                    max_parallel_agents: 1,
                },
                approval: ApprovalPolicy {
                    required_for: Vec::new(),
                },
            })
            .expect("submit Task")
        else {
            panic!("expected accepted Task");
        };
        supervisor.start(task.task_id).expect("start Task");
        let operation = catalog
            .prepare("run_process", Vec::new())
            .expect("prepare process Tool");

        let result = gate
            .request(
                &mut supervisor,
                task.task_id,
                operation,
                Duration::from_secs(30),
            )
            .expect("execute process Tool");

        assert!(matches!(result, ExecutionOutcome::Executed(_)));
    }
}
