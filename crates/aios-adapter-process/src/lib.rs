//! Bounded child-process Tool handler for AI OS.
//!
//! The handler executes one trusted, explicitly configured absolute executable. It never invokes
//! a shell or searches `PATH`. Dynamic arguments are accepted only after a trusted policy approves
//! them, and the child receives an empty environment except for explicitly configured values.
//!
//! Linux callers may opt into an experimental Bubblewrap launcher with an explicit read-only
//! root filesystem, a separate writable scratch directory, namespace isolation, and no network.
//! This crate is still not complete operating-system Capability enforcement: cgroups, seccomp,
//! descriptor-bound filesystem access, and Capability-derived mounts remain future work.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use aios_adapter_tool::{ToolFailure, ToolHandler, ToolOutput};
use aios_runtime::TaskId;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
pub const MAX_TIMEOUT: Duration = Duration::from_secs(60 * 60);
pub const MAX_ARGUMENTS: usize = 64;
pub const MAX_ARGUMENT_BYTES: usize = 4_096;
pub const MAX_TOTAL_ARGUMENT_BYTES: usize = 64 * 1_024;
pub const MAX_ENVIRONMENT_VARIABLES: usize = 64;
pub const MAX_ENVIRONMENT_NAME_BYTES: usize = 128;
pub const MAX_ENVIRONMENT_VALUE_BYTES: usize = 4_096;
pub const MAX_TOTAL_ENVIRONMENT_BYTES: usize = 64 * 1_024;
pub const MAX_SANDBOX_PATH_BYTES: usize = 4_096;

const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Trusted owner of the host directory used for Task-scoped sandbox scratch space.
///
/// The configured root must already exist as a real, absolute, owner-only directory. Each call
/// to [`create`](Self::create) creates one new empty child named from a [`TaskId`]. Existing Task
/// directories are rejected rather than reused.
pub struct TaskScratchManager {
    root_directory: PathBuf,
    root_identity: DirectoryIdentity,
}

impl TaskScratchManager {
    pub fn new(root_directory: impl Into<PathBuf>) -> Result<Self, ProcessAdapterError> {
        let root_directory = canonical_task_scratch_root(&root_directory.into())?;
        let root_identity = DirectoryIdentity::read(&root_directory)?;
        Ok(Self {
            root_directory,
            root_identity,
        })
    }

    /// Creates a new empty scratch directory bound to `task_id`.
    pub fn create(&self, task_id: TaskId) -> Result<TaskScratch, ProcessAdapterError> {
        self.validate_root()?;

        let directory = self.root_directory.join(task_id.to_string());
        if directory.as_os_str().as_encoded_bytes().len() > MAX_SANDBOX_PATH_BYTES {
            return Err(ProcessAdapterError::InvalidSandbox);
        }
        create_owner_only_directory(&directory)?;

        let validated = self
            .validate_root()
            .and_then(|()| canonical_task_scratch_directory(&directory, &self.root_directory))
            .and_then(|directory| {
                DirectoryIdentity::read(&directory)
                    .map(|directory_identity| (directory, directory_identity))
            });
        match validated {
            Ok((directory, directory_identity)) => Ok(TaskScratch {
                task_id,
                directory,
                directory_identity,
            }),
            Err(error) => {
                let _result = fs::remove_dir(&directory);
                Err(error)
            }
        }
    }

    #[must_use]
    pub fn root_directory(&self) -> &Path {
        &self.root_directory
    }

    fn validate_root(&self) -> Result<(), ProcessAdapterError> {
        validate_owner_only_directory(&self.root_directory)?;
        if DirectoryIdentity::read(&self.root_directory)? != self.root_identity {
            return Err(ProcessAdapterError::InvalidSandbox);
        }
        Ok(())
    }
}

/// Newly created, Task-bound scratch directory authority.
///
/// This type intentionally does not implement `Clone`, `Debug`, or serialization. Cleanup is an
/// explicit runtime lifecycle concern; dropping this value does not delete Tool-created files.
pub struct TaskScratch {
    task_id: TaskId,
    directory: PathBuf,
    directory_identity: DirectoryIdentity,
}

impl TaskScratch {
    #[must_use]
    pub fn task_id(&self) -> TaskId {
        self.task_id
    }

    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }
}

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
    InvalidSandbox,
    UnsupportedPlatform,
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
            Self::InvalidSandbox => "invalid Process Adapter sandbox configuration",
            Self::UnsupportedPlatform => "Process Adapter sandbox is unsupported on this platform",
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

/// Trusted builder for one Linux Bubblewrap-isolated child-process Tool handler.
///
/// `root_filesystem` is mounted read-only at `/`. `scratch_directory` is the only configured
/// writable host path and is mounted at `/workspace`. The sandbox always receives a new network
/// namespace; this initial backend deliberately has no network-enabled mode.
pub struct BubblewrapProcessToolBuilder {
    bubblewrap: PathBuf,
    root_filesystem: PathBuf,
    sandbox_executable: PathBuf,
    scratch_directory: PathBuf,
    task_scratch_identity: Option<DirectoryIdentity>,
    fixed_arguments: Vec<String>,
    environment: Vec<(String, String)>,
    timeout: Duration,
    argument_policy: Box<dyn ProcessArgumentPolicy>,
}

impl BubblewrapProcessToolBuilder {
    pub fn new<P>(
        bubblewrap: impl Into<PathBuf>,
        root_filesystem: impl Into<PathBuf>,
        sandbox_executable: impl Into<PathBuf>,
        scratch_directory: impl Into<PathBuf>,
        argument_policy: P,
    ) -> Self
    where
        P: ProcessArgumentPolicy + 'static,
    {
        Self {
            bubblewrap: bubblewrap.into(),
            root_filesystem: root_filesystem.into(),
            sandbox_executable: sandbox_executable.into(),
            scratch_directory: scratch_directory.into(),
            task_scratch_identity: None,
            fixed_arguments: Vec::new(),
            environment: Vec::new(),
            timeout: DEFAULT_TIMEOUT,
            argument_policy: Box::new(argument_policy),
        }
    }

    /// Creates a builder using scratch space freshly allocated for one Task.
    pub fn new_for_task<P>(
        bubblewrap: impl Into<PathBuf>,
        root_filesystem: impl Into<PathBuf>,
        sandbox_executable: impl Into<PathBuf>,
        task_scratch: &TaskScratch,
        argument_policy: P,
    ) -> Self
    where
        P: ProcessArgumentPolicy + 'static,
    {
        let mut builder = Self::new(
            bubblewrap,
            root_filesystem,
            sandbox_executable,
            &task_scratch.directory,
            argument_policy,
        );
        builder.task_scratch_identity = Some(task_scratch.directory_identity);
        builder
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
        if !cfg!(target_os = "linux") {
            return Err(ProcessAdapterError::UnsupportedPlatform);
        }
        if self.timeout.is_zero() || self.timeout > MAX_TIMEOUT {
            return Err(ProcessAdapterError::InvalidConfig);
        }
        validate_arguments(&self.fixed_arguments, &[])?;

        let bubblewrap = canonical_executable(&self.bubblewrap)?;
        let bubblewrap_identity = ExecutableIdentity::read(&bubblewrap)?;
        let root_filesystem = canonical_sandbox_root(&self.root_filesystem)?;
        validate_sandbox_mount_points(&root_filesystem)?;
        let scratch_directory =
            canonical_scratch_directory(&self.scratch_directory, &root_filesystem)?;
        if let Some(identity) = self.task_scratch_identity {
            validate_task_scratch_identity(&scratch_directory, identity)?;
        }
        let (executable, executable_identity, sandbox_executable) =
            canonical_sandbox_executable(&root_filesystem, &self.sandbox_executable)?;
        let environment = validate_environment(self.environment)?;

        Ok(ProcessToolHandler {
            executable,
            executable_identity,
            working_directory: scratch_directory,
            working_directory_identity: self.task_scratch_identity,
            fixed_arguments: self.fixed_arguments,
            environment,
            timeout: self.timeout,
            argument_policy: self.argument_policy,
            launcher: ProcessLauncher::Bubblewrap {
                bubblewrap,
                bubblewrap_identity,
                root_filesystem,
                sandbox_executable,
            },
        })
    }
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
            working_directory_identity: None,
            fixed_arguments: self.fixed_arguments,
            environment,
            timeout: self.timeout,
            argument_policy: self.argument_policy,
            launcher: ProcessLauncher::Direct,
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
    working_directory_identity: Option<DirectoryIdentity>,
    fixed_arguments: Vec<String>,
    environment: BTreeMap<String, String>,
    timeout: Duration,
    argument_policy: Box<dyn ProcessArgumentPolicy>,
    launcher: ProcessLauncher,
}

enum ProcessLauncher {
    Direct,
    Bubblewrap {
        bubblewrap: PathBuf,
        bubblewrap_identity: ExecutableIdentity,
        root_filesystem: PathBuf,
        sandbox_executable: PathBuf,
    },
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
        if let Some(identity) = self.working_directory_identity {
            validate_task_scratch_identity(&self.working_directory, identity)?;
        }

        let mut command = match &self.launcher {
            ProcessLauncher::Direct => {
                let mut command = Command::new(&self.executable);
                command
                    .args(&self.fixed_arguments)
                    .args(&dynamic_arguments)
                    .current_dir(&self.working_directory);
                command
            }
            ProcessLauncher::Bubblewrap {
                bubblewrap,
                bubblewrap_identity,
                root_filesystem,
                sandbox_executable,
            } => {
                if ExecutableIdentity::read(bubblewrap)? != *bubblewrap_identity {
                    return Err(ProcessAdapterError::ExecutableChanged);
                }
                let mut command = Command::new(bubblewrap);
                command
                    .args(bubblewrap_arguments(
                        root_filesystem,
                        &self.working_directory,
                        sandbox_executable,
                    ))
                    .args(&self.fixed_arguments)
                    .args(&dynamic_arguments)
                    .current_dir("/");
                command
            }
        };
        command
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
#[derive(Clone, Copy, Eq, PartialEq)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
}

#[cfg(unix)]
impl DirectoryIdentity {
    fn read(path: &Path) -> Result<Self, ProcessAdapterError> {
        use std::os::unix::fs::MetadataExt;

        let metadata = fs::metadata(path).map_err(|_| ProcessAdapterError::InvalidSandbox)?;
        if !metadata.is_dir() {
            return Err(ProcessAdapterError::InvalidSandbox);
        }
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}

#[cfg(not(unix))]
#[derive(Clone, Copy, Eq, PartialEq)]
struct DirectoryIdentity;

#[cfg(not(unix))]
impl DirectoryIdentity {
    fn read(_path: &Path) -> Result<Self, ProcessAdapterError> {
        Err(ProcessAdapterError::UnsupportedPlatform)
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

#[cfg(unix)]
fn canonical_task_scratch_root(path: &Path) -> Result<PathBuf, ProcessAdapterError> {
    if !path.is_absolute() || path.as_os_str().as_encoded_bytes().len() > MAX_SANDBOX_PATH_BYTES {
        return Err(ProcessAdapterError::InvalidSandbox);
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| ProcessAdapterError::InvalidSandbox)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ProcessAdapterError::InvalidSandbox);
    }
    validate_owner_only_directory(path)?;

    let canonical = fs::canonicalize(path).map_err(|_| ProcessAdapterError::InvalidSandbox)?;
    if canonical == Path::new("/") {
        return Err(ProcessAdapterError::InvalidSandbox);
    }
    Ok(canonical)
}

#[cfg(not(unix))]
fn canonical_task_scratch_root(_path: &Path) -> Result<PathBuf, ProcessAdapterError> {
    Err(ProcessAdapterError::UnsupportedPlatform)
}

#[cfg(unix)]
fn validate_owner_only_directory(path: &Path) -> Result<(), ProcessAdapterError> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = fs::symlink_metadata(path).map_err(|_| ProcessAdapterError::InvalidSandbox)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(ProcessAdapterError::InvalidSandbox);
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_owner_only_directory(_path: &Path) -> Result<(), ProcessAdapterError> {
    Err(ProcessAdapterError::UnsupportedPlatform)
}

#[cfg(unix)]
fn create_owner_only_directory(path: &Path) -> Result<(), ProcessAdapterError> {
    use std::fs::DirBuilder;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    let mut builder = DirBuilder::new();
    builder.mode(0o700);
    builder
        .create(path)
        .map_err(|_| ProcessAdapterError::InvalidSandbox)?;
    if fs::set_permissions(path, fs::Permissions::from_mode(0o700)).is_err() {
        let _result = fs::remove_dir(path);
        return Err(ProcessAdapterError::InvalidSandbox);
    }
    Ok(())
}

#[cfg(not(unix))]
fn create_owner_only_directory(_path: &Path) -> Result<(), ProcessAdapterError> {
    Err(ProcessAdapterError::UnsupportedPlatform)
}

fn canonical_task_scratch_directory(
    path: &Path,
    root_directory: &Path,
) -> Result<PathBuf, ProcessAdapterError> {
    validate_owner_only_directory(path)?;
    let canonical = fs::canonicalize(path).map_err(|_| ProcessAdapterError::InvalidSandbox)?;
    if canonical.parent() != Some(root_directory) {
        return Err(ProcessAdapterError::InvalidSandbox);
    }
    Ok(canonical)
}

fn validate_task_scratch_identity(
    path: &Path,
    expected: DirectoryIdentity,
) -> Result<(), ProcessAdapterError> {
    validate_owner_only_directory(path)?;
    if DirectoryIdentity::read(path)? != expected {
        return Err(ProcessAdapterError::InvalidSandbox);
    }
    Ok(())
}

fn canonical_sandbox_root(path: &Path) -> Result<PathBuf, ProcessAdapterError> {
    let canonical = canonical_directory(path).map_err(|_| ProcessAdapterError::InvalidSandbox)?;
    if canonical == Path::new("/") {
        return Err(ProcessAdapterError::InvalidSandbox);
    }
    Ok(canonical)
}

fn canonical_scratch_directory(
    path: &Path,
    root_filesystem: &Path,
) -> Result<PathBuf, ProcessAdapterError> {
    let canonical = canonical_directory(path).map_err(|_| ProcessAdapterError::InvalidSandbox)?;
    if canonical == Path::new("/")
        || canonical.starts_with(root_filesystem)
        || root_filesystem.starts_with(&canonical)
    {
        return Err(ProcessAdapterError::InvalidSandbox);
    }
    Ok(canonical)
}

fn canonical_sandbox_executable(
    root_filesystem: &Path,
    sandbox_executable: &Path,
) -> Result<(PathBuf, ExecutableIdentity, PathBuf), ProcessAdapterError> {
    validate_sandbox_absolute_path(sandbox_executable)?;
    let relative = sandbox_executable
        .strip_prefix("/")
        .map_err(|_| ProcessAdapterError::InvalidSandbox)?;
    let executable = fs::canonicalize(root_filesystem.join(relative))
        .map_err(|_| ProcessAdapterError::InvalidSandbox)?;
    if !executable.starts_with(root_filesystem) {
        return Err(ProcessAdapterError::InvalidSandbox);
    }
    let identity =
        ExecutableIdentity::read(&executable).map_err(|_| ProcessAdapterError::InvalidSandbox)?;
    let resolved_relative = executable
        .strip_prefix(root_filesystem)
        .map_err(|_| ProcessAdapterError::InvalidSandbox)?;
    let resolved_sandbox_path = Path::new("/").join(resolved_relative);
    Ok((executable, identity, resolved_sandbox_path))
}

fn validate_sandbox_absolute_path(path: &Path) -> Result<(), ProcessAdapterError> {
    if !path.is_absolute()
        || path.as_os_str().as_encoded_bytes().len() > MAX_SANDBOX_PATH_BYTES
        || path.as_os_str().as_encoded_bytes().contains(&0)
    {
        return Err(ProcessAdapterError::InvalidSandbox);
    }

    let mut components = path.components();
    if components.next() != Some(Component::RootDir)
        || !components.all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(ProcessAdapterError::InvalidSandbox);
    }
    Ok(())
}

fn validate_sandbox_mount_points(root_filesystem: &Path) -> Result<(), ProcessAdapterError> {
    for relative in ["proc", "dev", "tmp", "workspace"] {
        let metadata = fs::symlink_metadata(root_filesystem.join(relative))
            .map_err(|_| ProcessAdapterError::InvalidSandbox)?;
        if !metadata.file_type().is_dir() {
            return Err(ProcessAdapterError::InvalidSandbox);
        }
    }
    Ok(())
}

fn bubblewrap_arguments(
    root_filesystem: &Path,
    scratch_directory: &Path,
    sandbox_executable: &Path,
) -> Vec<std::ffi::OsString> {
    [
        std::ffi::OsString::from("--unshare-all"),
        std::ffi::OsString::from("--unshare-user"),
        std::ffi::OsString::from("--disable-userns"),
        std::ffi::OsString::from("--die-with-parent"),
        std::ffi::OsString::from("--new-session"),
        std::ffi::OsString::from("--cap-drop"),
        std::ffi::OsString::from("ALL"),
        std::ffi::OsString::from("--ro-bind"),
        root_filesystem.as_os_str().to_owned(),
        std::ffi::OsString::from("/"),
        std::ffi::OsString::from("--proc"),
        std::ffi::OsString::from("/proc"),
        std::ffi::OsString::from("--dev"),
        std::ffi::OsString::from("/dev"),
        std::ffi::OsString::from("--tmpfs"),
        std::ffi::OsString::from("/tmp"),
        std::ffi::OsString::from("--bind"),
        scratch_directory.as_os_str().to_owned(),
        std::ffi::OsString::from("/workspace"),
        std::ffi::OsString::from("--chdir"),
        std::ffi::OsString::from("/workspace"),
        std::ffi::OsString::from("--"),
        sandbox_executable.as_os_str().to_owned(),
    ]
    .into()
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
    use std::fs::{self, DirBuilder};
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use aios_adapter_tool::ToolAdapterBuilder;
    use aios_core::{ApprovalPolicy, Budget, CapabilitySet, NetworkPolicy, TaskSpec};
    use aios_runtime::{ExecutionOutcome, SubmitResult, TaskId, TaskSupervisor};

    #[cfg(not(target_os = "linux"))]
    use super::BubblewrapProcessToolBuilder;
    use super::{
        MAX_ARGUMENT_BYTES, MAX_ARGUMENTS, ProcessAdapterError, ProcessToolBuilder,
        TaskScratchManager, bubblewrap_arguments, canonical_sandbox_executable,
        canonical_sandbox_root, canonical_scratch_directory, validate_sandbox_absolute_path,
        validate_sandbox_mount_points, validate_task_scratch_identity,
    };

    fn private_test_directory(label: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "aios-process-{label}-{}-{}",
            std::process::id(),
            TaskId::new()
        ));
        let mut builder = DirBuilder::new();
        builder.mode(0o700);
        builder.create(&directory).expect("create test directory");
        directory
    }

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
    fn bubblewrap_plan_denies_network_and_exposes_only_declared_mounts() {
        let arguments = bubblewrap_arguments(
            Path::new("/opt/aios/rootfs"),
            Path::new("/var/lib/aios/tasks/task-1"),
            Path::new("/usr/bin/tool"),
        );
        let arguments: Vec<String> = arguments
            .into_iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();

        assert_eq!(
            arguments,
            [
                "--unshare-all",
                "--unshare-user",
                "--disable-userns",
                "--die-with-parent",
                "--new-session",
                "--cap-drop",
                "ALL",
                "--ro-bind",
                "/opt/aios/rootfs",
                "/",
                "--proc",
                "/proc",
                "--dev",
                "/dev",
                "--tmpfs",
                "/tmp",
                "--bind",
                "/var/lib/aios/tasks/task-1",
                "/workspace",
                "--chdir",
                "/workspace",
                "--",
                "/usr/bin/tool",
            ]
        );
        assert!(!arguments.iter().any(|argument| argument == "--share-net"));
        assert!(!arguments.iter().any(|argument| argument == "/run"));
    }

    #[test]
    fn task_scratch_is_new_owner_only_and_task_derived() {
        let root = private_test_directory("task-scratch");
        let manager = TaskScratchManager::new(&root).expect("open scratch root");
        let first_task = TaskId::new();
        let second_task = TaskId::new();

        let first = manager.create(first_task).expect("create first scratch");
        let second = manager.create(second_task).expect("create second scratch");

        assert_eq!(first.task_id(), first_task);
        assert_eq!(first.directory().parent(), Some(manager.root_directory()));
        assert_eq!(
            first.directory().file_name(),
            Some(first_task.to_string().as_ref())
        );
        assert_ne!(first.directory(), second.directory());
        assert_eq!(
            fs::metadata(first.directory())
                .expect("read scratch metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::read_dir(first.directory())
                .expect("read empty scratch")
                .count(),
            0
        );
        assert!(matches!(
            manager.create(first_task),
            Err(ProcessAdapterError::InvalidSandbox)
        ));

        fs::remove_dir_all(root).expect("remove scratch fixture");
    }

    #[test]
    fn task_scratch_rejects_unsafe_or_replaced_roots() {
        let fixture = private_test_directory("task-scratch-boundary");
        let public_root = fixture.join("public");
        fs::create_dir(&public_root).expect("create public root");
        fs::set_permissions(&public_root, fs::Permissions::from_mode(0o755))
            .expect("set public root permissions");
        assert!(matches!(
            TaskScratchManager::new(&public_root),
            Err(ProcessAdapterError::InvalidSandbox)
        ));

        let private_root = fixture.join("private");
        let mut builder = DirBuilder::new();
        builder.mode(0o700);
        builder.create(&private_root).expect("create private root");
        let symlink_root = fixture.join("symlink");
        symlink(&private_root, &symlink_root).expect("create scratch root symlink");
        assert!(matches!(
            TaskScratchManager::new(&symlink_root),
            Err(ProcessAdapterError::InvalidSandbox)
        ));

        let manager = TaskScratchManager::new(&private_root).expect("open private root");
        let moved_root = fixture.join("moved");
        fs::rename(&private_root, &moved_root).expect("replace scratch root");
        builder
            .create(&private_root)
            .expect("create replacement root");
        assert!(matches!(
            manager.create(TaskId::new()),
            Err(ProcessAdapterError::InvalidSandbox)
        ));

        fs::remove_dir_all(fixture).expect("remove scratch boundary fixture");
    }

    #[test]
    fn task_scratch_identity_rejects_permission_changes_and_path_replacement() {
        let root = private_test_directory("task-scratch-identity");
        let manager = TaskScratchManager::new(&root).expect("open scratch root");
        let scratch = manager.create(TaskId::new()).expect("create Task scratch");

        fs::set_permissions(scratch.directory(), fs::Permissions::from_mode(0o755))
            .expect("change scratch permissions");
        assert!(matches!(
            validate_task_scratch_identity(scratch.directory(), scratch.directory_identity),
            Err(ProcessAdapterError::InvalidSandbox)
        ));

        fs::set_permissions(scratch.directory(), fs::Permissions::from_mode(0o700))
            .expect("restore scratch permissions");
        let replacement = scratch.directory().to_path_buf();
        let moved = root.join("moved-task-scratch");
        fs::rename(&replacement, &moved).expect("move original scratch");
        let mut builder = DirBuilder::new();
        builder.mode(0o700);
        builder
            .create(&replacement)
            .expect("create replacement scratch");
        assert!(matches!(
            validate_task_scratch_identity(&replacement, scratch.directory_identity),
            Err(ProcessAdapterError::InvalidSandbox)
        ));

        fs::remove_dir_all(root).expect("remove scratch identity fixture");
    }

    #[test]
    fn sandbox_executable_path_must_be_bounded_absolute_and_traversal_free() {
        assert!(validate_sandbox_absolute_path(Path::new("/usr/bin/tool")).is_ok());
        assert!(validate_sandbox_absolute_path(Path::new("usr/bin/tool")).is_err());
        assert!(validate_sandbox_absolute_path(Path::new("/usr/../bin/tool")).is_err());
    }

    #[test]
    fn sandbox_rejects_host_root_overlapping_scratch_and_symlink_escape() {
        assert!(matches!(
            canonical_sandbox_root(Path::new("/")),
            Err(ProcessAdapterError::InvalidSandbox)
        ));

        let fixture = std::env::temp_dir().join(format!(
            "aios-process-sandbox-boundary-{}",
            std::process::id()
        ));
        let root = fixture.join("root");
        let scratch = root.join("scratch");
        let executable_parent = root.join("usr/bin");
        fs::create_dir_all(&scratch).expect("create overlapping scratch");
        fs::create_dir_all(&executable_parent).expect("create executable parent");
        symlink("/bin/true", executable_parent.join("tool")).expect("create escaping symlink");
        let root = fs::canonicalize(root).expect("canonical root");
        let scratch = fs::canonicalize(scratch).expect("canonical scratch");

        assert!(matches!(
            canonical_scratch_directory(&scratch, &root),
            Err(ProcessAdapterError::InvalidSandbox)
        ));
        assert!(matches!(
            canonical_sandbox_executable(&root, Path::new("/usr/bin/tool")),
            Err(ProcessAdapterError::InvalidSandbox)
        ));

        fs::remove_dir_all(fixture).expect("remove sandbox boundary fixture");
    }

    #[test]
    fn sandbox_mount_points_must_be_real_directories() {
        let fixture = std::env::temp_dir().join(format!(
            "aios-process-sandbox-mounts-{}",
            std::process::id()
        ));
        for directory in ["proc", "dev", "tmp"] {
            fs::create_dir_all(fixture.join(directory)).expect("create mount point");
        }
        symlink("/tmp", fixture.join("workspace")).expect("create mount-point symlink");

        assert!(matches!(
            validate_sandbox_mount_points(&fixture),
            Err(ProcessAdapterError::InvalidSandbox)
        ));

        fs::remove_dir_all(fixture).expect("remove sandbox mount fixture");
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn bubblewrap_builder_fails_closed_outside_linux() {
        let result = BubblewrapProcessToolBuilder::new(
            "/usr/bin/bwrap",
            "/opt/aios/rootfs",
            "/usr/bin/tool",
            "/var/lib/aios/tasks/task-1",
            |_: &[String]| true,
        )
        .build();

        assert!(matches!(
            result,
            Err(ProcessAdapterError::UnsupportedPlatform)
        ));
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
