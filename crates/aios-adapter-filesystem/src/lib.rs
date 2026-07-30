//! Descriptor-relative write-only Filesystem Adapter for AI OS.
//!
//! The adapter currently implements one deliberately narrow operation: create one new regular
//! file beneath a trusted root and write bounded bytes to it. It never returns file contents,
//! opens an existing destination, follows symlinks, or exposes a filesystem descriptor to model
//! or Tool code.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::path::{Path, PathBuf};
use std::time::Duration;

use aios_core::{CapabilityRequest, FileAccess};
use aios_runtime::{
    ApprovalId, EventStore, Executed, ExecutionAdapter, ExecutionError, ExecutionGate,
    ExecutionOutcome, GuardedOperation, TaskExecutionContext, TaskId, TaskSnapshot, TaskSupervisor,
};

pub const MAX_FILESYSTEM_PATH_BYTES: usize = 4_096;
pub const MAX_CREATE_BYTES: usize = 1_024 * 1_024;

/// Stable, redacted Filesystem Adapter failure categories.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilesystemAdapterError {
    InvalidConfig,
    InvalidOperation,
    UnsupportedPlatform,
    ScopeMismatch,
    BudgetExceeded,
    CreateFailed,
    WriteFailed,
}

impl Display for FilesystemAdapterError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidConfig => "invalid Filesystem Adapter configuration",
            Self::InvalidOperation => "invalid filesystem operation",
            Self::UnsupportedPlatform => "Filesystem Adapter is unsupported on this platform",
            Self::ScopeMismatch => "filesystem operation scope does not match the Task",
            Self::BudgetExceeded => "filesystem operation exceeded the Task Budget",
            Self::CreateFailed => "filesystem create operation failed",
            Self::WriteFailed => "filesystem write operation failed",
        };
        formatter.write_str(message)
    }
}

impl Error for FilesystemAdapterError {}

/// Complete create-new operation retained privately while approval is pending.
///
/// This type intentionally does not implement `Clone`, `Debug`, or serialization because the path
/// and contents may be sensitive.
pub struct FilesystemWriteOperation {
    path: String,
    contents: Vec<u8>,
    execution_context: Option<TaskExecutionContext>,
}

impl GuardedOperation for FilesystemWriteOperation {
    fn capability_request(&self) -> CapabilityRequest<'_> {
        CapabilityRequest::File {
            path: &self.path,
            access: FileAccess::Write,
        }
    }
}

/// Safe operation constructor that validates all model-controlled values before authorization.
#[derive(Default)]
pub struct FilesystemCatalog;

impl FilesystemCatalog {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    pub fn prepare_create(
        &self,
        path: String,
        contents: Vec<u8>,
    ) -> Result<FilesystemWriteOperation, FilesystemAdapterError> {
        validate_operation(&path, &contents)?;
        Ok(FilesystemWriteOperation {
            path,
            contents,
            execution_context: None,
        })
    }
}

/// Resource-free receipt for one successful create-new write.
pub struct FilesystemWriteReceipt {
    bytes_written: usize,
}

impl FilesystemWriteReceipt {
    #[must_use]
    pub const fn bytes_written(&self) -> usize {
        self.bytes_written
    }
}

/// Linux-only adapter holding an opened trusted root directory.
///
/// The root descriptor, operations, and contents intentionally omit `Debug` and serialization.
struct FilesystemAdapter {
    #[cfg(target_os = "linux")]
    root: std::os::fd::OwnedFd,
}

impl FilesystemAdapter {
    fn new(root: impl Into<PathBuf>) -> Result<Self, FilesystemAdapterError> {
        if !cfg!(target_os = "linux") {
            return Err(FilesystemAdapterError::UnsupportedPlatform);
        }
        open_root(&root.into())
    }
}

impl ExecutionAdapter<FilesystemWriteOperation> for FilesystemAdapter {
    type Output = FilesystemWriteReceipt;
    type Error = FilesystemAdapterError;

    fn execute(
        &mut self,
        operation: FilesystemWriteOperation,
    ) -> Result<Self::Output, Self::Error> {
        validate_operation(&operation.path, &operation.contents)?;
        create_new(self, operation)
    }
}

/// Capability- and approval-gated facade that never exposes the raw adapter.
pub struct FilesystemExecutionGate {
    gate: ExecutionGate<FilesystemAdapter, FilesystemWriteOperation>,
}

impl FilesystemExecutionGate {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, FilesystemAdapterError> {
        Ok(Self {
            gate: ExecutionGate::new(FilesystemAdapter::new(root)?),
        })
    }

    pub fn request<S: EventStore>(
        &mut self,
        supervisor: &mut TaskSupervisor<S>,
        task_id: TaskId,
        operation: FilesystemWriteOperation,
        approval_ttl: Duration,
    ) -> Result<ExecutionOutcome<FilesystemWriteReceipt>, ExecutionError<FilesystemAdapterError>>
    {
        let mut operation = operation;
        operation.execution_context = Some(
            supervisor
                .execution_context(task_id)
                .map_err(ExecutionError::Supervisor)?,
        );
        self.gate
            .request(supervisor, task_id, operation, approval_ttl)
    }

    pub fn approve_and_execute<S: EventStore>(
        &mut self,
        supervisor: &mut TaskSupervisor<S>,
        approval_id: ApprovalId,
    ) -> Result<Executed<FilesystemWriteReceipt>, ExecutionError<FilesystemAdapterError>> {
        self.gate.approve_and_execute(supervisor, approval_id)
    }

    pub fn deny<S: EventStore>(
        &mut self,
        supervisor: &mut TaskSupervisor<S>,
        approval_id: ApprovalId,
    ) -> Result<TaskSnapshot, ExecutionError<FilesystemAdapterError>> {
        self.gate.deny(supervisor, approval_id)
    }

    pub fn expire<S: EventStore>(
        &mut self,
        supervisor: &mut TaskSupervisor<S>,
    ) -> Result<usize, ExecutionError<FilesystemAdapterError>> {
        self.gate.expire(supervisor)
    }

    pub fn cancel<S: EventStore>(
        &mut self,
        supervisor: &mut TaskSupervisor<S>,
        task_id: TaskId,
    ) -> Result<bool, ExecutionError<FilesystemAdapterError>> {
        self.gate.cancel(supervisor, task_id)
    }

    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.gate.pending_count()
    }
}

fn validate_operation(path: &str, contents: &[u8]) -> Result<(), FilesystemAdapterError> {
    if contents.len() > MAX_CREATE_BYTES
        || !path.starts_with('/')
        || path == "/"
        || path.len() > MAX_FILESYSTEM_PATH_BYTES
        || path.contains('\0')
        || !path
            .split('/')
            .skip(1)
            .all(|component| !component.is_empty() && component != "." && component != "..")
    {
        return Err(FilesystemAdapterError::InvalidOperation);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn open_root(path: &Path) -> Result<FilesystemAdapter, FilesystemAdapterError> {
    use rustix::fs::{Mode, OFlags, ResolveFlags, openat2};

    if !path.is_absolute()
        || path.as_os_str().as_encoded_bytes().len() > MAX_FILESYSTEM_PATH_BYTES
        || path == Path::new("/")
    {
        return Err(FilesystemAdapterError::InvalidConfig);
    }
    let canonical =
        std::fs::canonicalize(path).map_err(|_| FilesystemAdapterError::InvalidConfig)?;
    if canonical == Path::new("/") {
        return Err(FilesystemAdapterError::InvalidConfig);
    }
    let relative = canonical
        .strip_prefix("/")
        .map_err(|_| FilesystemAdapterError::InvalidConfig)?;
    let host_root = std::fs::File::open("/").map_err(|_| FilesystemAdapterError::InvalidConfig)?;
    let root = openat2(
        &host_root,
        relative,
        OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::BENEATH | ResolveFlags::NO_MAGICLINKS | ResolveFlags::NO_SYMLINKS,
    )
    .map_err(|_| FilesystemAdapterError::InvalidConfig)?;
    Ok(FilesystemAdapter { root })
}

#[cfg(not(target_os = "linux"))]
fn open_root(_path: &Path) -> Result<FilesystemAdapter, FilesystemAdapterError> {
    Err(FilesystemAdapterError::UnsupportedPlatform)
}

#[cfg(target_os = "linux")]
fn create_new(
    adapter: &FilesystemAdapter,
    operation: FilesystemWriteOperation,
) -> Result<FilesystemWriteReceipt, FilesystemAdapterError> {
    use std::io::Write;

    use rustix::fs::{Mode, OFlags, ResolveFlags, openat2};

    let context = operation
        .execution_context
        .as_ref()
        .ok_or(FilesystemAdapterError::ScopeMismatch)?;
    require_remaining_budget(context)?;
    let relative = Path::new(&operation.path)
        .strip_prefix("/")
        .map_err(|_| FilesystemAdapterError::InvalidOperation)?;
    let descriptor = openat2(
        &adapter.root,
        relative,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
        ResolveFlags::BENEATH
            | ResolveFlags::NO_MAGICLINKS
            | ResolveFlags::NO_SYMLINKS
            | ResolveFlags::NO_XDEV,
    )
    .map_err(|_| FilesystemAdapterError::CreateFailed)?;
    let mut file = std::fs::File::from(descriptor);
    require_remaining_budget(context)?;
    file.write_all(&operation.contents)
        .and_then(|()| file.sync_data())
        .map_err(|_| FilesystemAdapterError::WriteFailed)?;
    require_remaining_budget(context)?;
    Ok(FilesystemWriteReceipt {
        bytes_written: operation.contents.len(),
    })
}

#[cfg(not(target_os = "linux"))]
fn create_new(
    _adapter: &FilesystemAdapter,
    _operation: FilesystemWriteOperation,
) -> Result<FilesystemWriteReceipt, FilesystemAdapterError> {
    Err(FilesystemAdapterError::UnsupportedPlatform)
}

#[cfg(target_os = "linux")]
fn require_remaining_budget(context: &TaskExecutionContext) -> Result<(), FilesystemAdapterError> {
    if context.remaining_wall_time().is_zero() {
        return Err(FilesystemAdapterError::BudgetExceeded);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        FilesystemAdapterError, FilesystemCatalog, MAX_CREATE_BYTES, MAX_FILESYSTEM_PATH_BYTES,
    };

    #[test]
    fn catalog_accepts_bounded_normalized_create() {
        let catalog = FilesystemCatalog::new();
        assert!(
            catalog
                .prepare_create(
                    "/workspace/output/result.txt".to_owned(),
                    b"result".to_vec()
                )
                .is_ok()
        );
    }

    #[test]
    fn catalog_rejects_invalid_paths_and_oversized_contents() {
        let catalog = FilesystemCatalog::new();
        for path in [
            "",
            "/",
            "workspace/output",
            "/workspace/../secret",
            "/workspace//output",
            "/workspace/\0output",
        ] {
            assert!(matches!(
                catalog.prepare_create(path.to_owned(), Vec::new()),
                Err(FilesystemAdapterError::InvalidOperation)
            ));
        }
        assert!(matches!(
            catalog.prepare_create(
                format!("/{}", "a".repeat(MAX_FILESYSTEM_PATH_BYTES)),
                Vec::new(),
            ),
            Err(FilesystemAdapterError::InvalidOperation)
        ));
        assert!(matches!(
            catalog.prepare_create(
                "/workspace/output/result.txt".to_owned(),
                vec![0; MAX_CREATE_BYTES + 1],
            ),
            Err(FilesystemAdapterError::InvalidOperation)
        ));
    }
}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use std::fs::{self, DirBuilder};
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
    use std::path::{Path, PathBuf};
    use std::thread;
    use std::time::Duration;

    use aios_core::{
        ApprovalPolicy, Budget, CapabilitySet, FileAccess, FileCapability, NetworkPolicy, TaskSpec,
    };
    use aios_runtime::{ExecutionError, ExecutionOutcome, SubmitResult, TaskId, TaskSupervisor};

    use super::{FilesystemAdapterError, FilesystemCatalog, FilesystemExecutionGate};

    struct TestDirectory {
        path: PathBuf,
    }

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "aios-filesystem-{label}-{}-{}",
                std::process::id(),
                TaskId::new()
            ));
            let mut builder = DirBuilder::new();
            builder.mode(0o700);
            builder.create(&path).expect("create test directory");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.path).expect("remove test directory");
        }
    }

    fn create_sandbox_path(root: &Path) {
        fs::create_dir_all(root.join("workspace/output")).expect("create sandbox path");
    }

    fn task(
        idempotency_key: &str,
        filesystem: Vec<FileCapability>,
        required_for: &[&str],
    ) -> TaskSpec {
        TaskSpec {
            idempotency_key: idempotency_key.to_owned(),
            goal: "Create one bounded output file".to_owned(),
            capabilities: CapabilitySet {
                filesystem,
                network: NetworkPolicy::Deny,
                tools: Vec::new(),
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

    fn write_capability(path: &str) -> FileCapability {
        FileCapability {
            path: path.to_owned(),
            access: FileAccess::Write,
        }
    }

    fn running_supervisor(
        filesystem: Vec<FileCapability>,
        required_for: &[&str],
    ) -> (TaskSupervisor, TaskId) {
        running_supervisor_with_wall_time(filesystem, required_for, 60)
    }

    fn running_supervisor_with_wall_time(
        filesystem: Vec<FileCapability>,
        required_for: &[&str],
        wall_time_seconds: u64,
    ) -> (TaskSupervisor, TaskId) {
        let mut supervisor = TaskSupervisor::default();
        let mut spec = task("filesystem-adapter-test", filesystem, required_for);
        spec.budget.wall_time_seconds = wall_time_seconds;
        let SubmitResult::Accepted(task) = supervisor.submit(spec).expect("submit Task") else {
            panic!("expected accepted Task");
        };
        supervisor.start(task.task_id).expect("start Task");
        (supervisor, task.task_id)
    }

    #[test]
    fn creates_new_file_with_private_mode_and_never_overwrites() {
        let directory = TestDirectory::new("create");
        create_sandbox_path(directory.path());
        let catalog = FilesystemCatalog::new();
        let mut gate = FilesystemExecutionGate::new(directory.path()).expect("open adapter root");
        let (mut supervisor, task_id) =
            running_supervisor(vec![write_capability("/workspace/output")], &[]);
        let destination = directory.path().join("workspace/output/result.txt");

        let result = gate
            .request(
                &mut supervisor,
                task_id,
                catalog
                    .prepare_create(
                        "/workspace/output/result.txt".to_owned(),
                        b"first result".to_vec(),
                    )
                    .expect("prepare create"),
                Duration::from_secs(30),
            )
            .expect("execute create");
        let ExecutionOutcome::Executed(executed) = result else {
            panic!("expected execution");
        };

        assert_eq!(executed.output.bytes_written(), 12);
        assert_eq!(
            fs::read(&destination).expect("read test output"),
            b"first result"
        );
        assert_eq!(
            fs::metadata(&destination)
                .expect("read output metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        let result = gate.request(
            &mut supervisor,
            task_id,
            catalog
                .prepare_create(
                    "/workspace/output/result.txt".to_owned(),
                    b"replacement".to_vec(),
                )
                .expect("prepare second create"),
            Duration::from_secs(30),
        );
        assert!(matches!(
            result,
            Err(ExecutionError::Adapter(
                FilesystemAdapterError::CreateFailed
            ))
        ));
        assert_eq!(
            fs::read(destination).expect("read original output"),
            b"first result"
        );
    }

    #[test]
    fn approval_retains_exact_operation_without_writing_early() {
        let directory = TestDirectory::new("approval");
        create_sandbox_path(directory.path());
        let catalog = FilesystemCatalog::new();
        let mut gate = FilesystemExecutionGate::new(directory.path()).expect("open adapter root");
        let (mut supervisor, task_id) = running_supervisor(
            vec![write_capability("/workspace/output")],
            &["filesystem.write"],
        );
        let destination = directory.path().join("workspace/output/approved.txt");

        let result = gate
            .request(
                &mut supervisor,
                task_id,
                catalog
                    .prepare_create(
                        "/workspace/output/approved.txt".to_owned(),
                        b"approved bytes".to_vec(),
                    )
                    .expect("prepare create"),
                Duration::from_secs(30),
            )
            .expect("request approval");
        let ExecutionOutcome::ApprovalRequired(request) = result else {
            panic!("expected approval request");
        };
        assert!(!destination.exists());

        let executed = gate
            .approve_and_execute(&mut supervisor, request.approval_id)
            .expect("approve and execute");

        assert_eq!(executed.output.bytes_written(), 14);
        assert_eq!(
            fs::read(destination).expect("read approved output"),
            b"approved bytes"
        );
        assert_eq!(gate.pending_count(), 0);
    }

    #[test]
    fn approval_wait_cannot_reset_task_wall_time() {
        let directory = TestDirectory::new("approval-budget");
        create_sandbox_path(directory.path());
        let catalog = FilesystemCatalog::new();
        let mut gate = FilesystemExecutionGate::new(directory.path()).expect("open adapter root");
        let (mut supervisor, task_id) = running_supervisor_with_wall_time(
            vec![write_capability("/workspace/output")],
            &["filesystem.write"],
            1,
        );
        let destination = directory.path().join("workspace/output/expired.txt");
        let result = gate
            .request(
                &mut supervisor,
                task_id,
                catalog
                    .prepare_create(
                        "/workspace/output/expired.txt".to_owned(),
                        b"must not be written".to_vec(),
                    )
                    .expect("prepare create"),
                Duration::from_secs(30),
            )
            .expect("request approval");
        let ExecutionOutcome::ApprovalRequired(request) = result else {
            panic!("expected approval request");
        };
        thread::sleep(Duration::from_millis(1_100));

        let result = gate.approve_and_execute(&mut supervisor, request.approval_id);

        assert!(matches!(
            result,
            Err(ExecutionError::Adapter(
                FilesystemAdapterError::BudgetExceeded
            ))
        ));
        assert!(!destination.exists());
        assert_eq!(gate.pending_count(), 0);
    }

    #[test]
    fn denies_prefix_sibling_and_read_only_capabilities() {
        let directory = TestDirectory::new("denial");
        create_sandbox_path(directory.path());
        fs::create_dir_all(directory.path().join("workspace/output-private"))
            .expect("create sibling path");
        let catalog = FilesystemCatalog::new();
        let mut gate = FilesystemExecutionGate::new(directory.path()).expect("open adapter root");
        let (mut supervisor, task_id) =
            running_supervisor(vec![write_capability("/workspace/output")], &[]);
        let sibling = directory.path().join("workspace/output-private/result.txt");

        let result = gate
            .request(
                &mut supervisor,
                task_id,
                catalog
                    .prepare_create(
                        "/workspace/output-private/result.txt".to_owned(),
                        b"denied".to_vec(),
                    )
                    .expect("prepare sibling create"),
                Duration::from_secs(30),
            )
            .expect("evaluate sibling create");
        assert!(matches!(result, ExecutionOutcome::Denied { .. }));
        assert!(!sibling.exists());

        let (mut supervisor, task_id) = running_supervisor(
            vec![FileCapability {
                path: "/workspace/output".to_owned(),
                access: FileAccess::Read,
            }],
            &[],
        );
        let result = gate
            .request(
                &mut supervisor,
                task_id,
                catalog
                    .prepare_create(
                        "/workspace/output/read-only.txt".to_owned(),
                        b"denied".to_vec(),
                    )
                    .expect("prepare read-only create"),
                Duration::from_secs(30),
            )
            .expect("evaluate read-only create");
        assert!(matches!(result, ExecutionOutcome::Denied { .. }));
        assert!(
            !directory
                .path()
                .join("workspace/output/read-only.txt")
                .exists()
        );
    }

    #[test]
    fn opened_root_survives_path_replacement() {
        let directory = TestDirectory::new("root-replacement");
        let configured_root = directory.path().join("configured");
        create_sandbox_path(&configured_root);
        let catalog = FilesystemCatalog::new();
        let mut gate = FilesystemExecutionGate::new(&configured_root).expect("open adapter root");
        let retained_root = directory.path().join("retained");
        fs::rename(&configured_root, &retained_root).expect("replace configured root");
        create_sandbox_path(&configured_root);
        let (mut supervisor, task_id) =
            running_supervisor(vec![write_capability("/workspace/output")], &[]);

        let result = gate
            .request(
                &mut supervisor,
                task_id,
                catalog
                    .prepare_create(
                        "/workspace/output/result.txt".to_owned(),
                        b"descriptor-bound".to_vec(),
                    )
                    .expect("prepare create"),
                Duration::from_secs(30),
            )
            .expect("execute create");

        assert!(matches!(result, ExecutionOutcome::Executed(_)));
        assert_eq!(
            fs::read(retained_root.join("workspace/output/result.txt"))
                .expect("read retained-root output"),
            b"descriptor-bound"
        );
        assert!(!configured_root.join("workspace/output/result.txt").exists());
    }

    #[test]
    fn rejects_symlink_escape_without_creating_outside_root() {
        let directory = TestDirectory::new("symlink");
        let root = directory.path().join("root");
        fs::create_dir_all(root.join("workspace")).expect("create sandbox parent");
        let outside = directory.path().join("outside");
        fs::create_dir(&outside).expect("create outside directory");
        symlink(&outside, root.join("workspace/output")).expect("create escape symlink");
        let catalog = FilesystemCatalog::new();
        let mut gate = FilesystemExecutionGate::new(&root).expect("open adapter root");
        let (mut supervisor, task_id) =
            running_supervisor(vec![write_capability("/workspace/output")], &[]);

        let result = gate.request(
            &mut supervisor,
            task_id,
            catalog
                .prepare_create(
                    "/workspace/output/result.txt".to_owned(),
                    b"must not escape".to_vec(),
                )
                .expect("prepare create"),
            Duration::from_secs(30),
        );

        assert!(matches!(
            result,
            Err(ExecutionError::Adapter(
                FilesystemAdapterError::CreateFailed
            ))
        ));
        assert!(!outside.join("result.txt").exists());
    }
}
