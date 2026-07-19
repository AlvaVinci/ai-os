//! A bounded local API transported over a Unix domain socket.
//!
//! Each connection carries exactly one length-prefixed JSON request and one
//! response. The API never listens on a network interface.

#![cfg(unix)]

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::fs::{self, DirBuilder};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use aios_core::{TaskSpec, TaskState, ValidationErrors};
use aios_runtime::{
    EventStore, SubmitResult, SupervisorError, TaskEvent, TaskId, TaskSnapshot, TaskSupervisor,
};
use serde::{Deserialize, Serialize};

pub const DEFAULT_MAX_FRAME_BYTES: usize = 64 * 1024;
pub const DEFAULT_EVENT_PAGE_SIZE: u16 = 100;
pub const MAX_EVENT_PAGE_SIZE: u16 = 256;
pub const PROTOCOL_VERSION: u16 = 1;
const MIN_MAX_FRAME_BYTES: usize = 1024;
const MAX_MAX_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct ServerConfig {
    pub max_frame_bytes: usize,
    pub connection_timeout: Duration,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
            connection_timeout: Duration::from_secs(5),
        }
    }
}

impl ServerConfig {
    fn validate(self) -> Result<Self, LocalApiError> {
        if !(MIN_MAX_FRAME_BYTES..=MAX_MAX_FRAME_BYTES).contains(&self.max_frame_bytes)
            || self.connection_timeout.is_zero()
        {
            return Err(LocalApiError::InvalidConfig);
        }
        Ok(self)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApiRequest {
    pub protocol_version: u16,
    pub request: ApiMethod,
}

impl ApiRequest {
    #[must_use]
    pub const fn new(request: ApiMethod) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            request,
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum ApiMethod {
    Health,
    Submit {
        task: Box<TaskSpec>,
    },
    GetTask {
        task_id: TaskId,
    },
    Events {
        task_id: TaskId,
        #[serde(default)]
        after_sequence: u64,
        #[serde(default = "default_event_page_size")]
        limit: u16,
    },
    Start {
        task_id: TaskId,
    },
    WaitForApproval {
        task_id: TaskId,
    },
    Approve {
        task_id: TaskId,
    },
    Succeed {
        task_id: TaskId,
    },
    Fail {
        task_id: TaskId,
    },
    Cancel {
        task_id: TaskId,
    },
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ApiResponse {
    pub protocol_version: u16,
    #[serde(flatten)]
    pub outcome: ApiOutcome,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ApiOutcome {
    Ok { result: ApiResult },
    Error { error: ApiError },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApiResult {
    Healthy,
    Submitted {
        disposition: SubmissionDisposition,
        task: TaskView,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        validation_errors: Vec<ValidationIssue>,
    },
    Task {
        task: TaskView,
    },
    Events {
        events: Vec<TaskEvent>,
        has_more: bool,
    },
    Cancelled {
        task: TaskView,
        changed: bool,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionDisposition {
    Accepted,
    Existing,
    Rejected,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct TaskView {
    pub task_id: TaskId,
    pub state: TaskState,
}

impl From<TaskSnapshot> for TaskView {
    fn from(task: TaskSnapshot) -> Self {
        Self {
            task_id: task.task_id,
            state: task.state,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ValidationIssue {
    pub field: String,
    pub message: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ApiError {
    pub code: ApiErrorCode,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ApiErrorCode {
    InvalidRequest,
    UnsupportedProtocolVersion,
    FrameTooLarge,
    TaskNotFound,
    IdempotencyConflict,
    CapacityExceeded,
    InvalidStateTransition,
    StorageUnavailable,
    ResponseTooLarge,
}

pub struct ApiService<S> {
    supervisor: TaskSupervisor<S>,
}

impl<S: EventStore> ApiService<S> {
    #[must_use]
    pub fn new(supervisor: TaskSupervisor<S>) -> Self {
        Self { supervisor }
    }

    pub fn handle(&mut self, request: ApiRequest) -> ApiResponse {
        if request.protocol_version != PROTOCOL_VERSION {
            return api_error(
                ApiErrorCode::UnsupportedProtocolVersion,
                "protocol version is not supported",
            );
        }

        match request.request {
            ApiMethod::Health => ok(ApiResult::Healthy),
            ApiMethod::Submit { task } => match self.supervisor.submit(*task) {
                Ok(result) => submission_response(result),
                Err(error) => supervisor_error_response(error),
            },
            ApiMethod::GetTask { task_id } => self.supervisor.get(task_id).map_or_else(
                || api_error(ApiErrorCode::TaskNotFound, "task not found"),
                |task| ok(ApiResult::Task { task: task.into() }),
            ),
            ApiMethod::Events {
                task_id,
                after_sequence,
                limit,
            } => {
                if limit == 0 || limit > MAX_EVENT_PAGE_SIZE {
                    return invalid_request();
                }
                match self.supervisor.events(task_id, after_sequence) {
                    Ok(mut events) => {
                        let has_more = events.len() > usize::from(limit);
                        events.truncate(usize::from(limit));
                        ok(ApiResult::Events { events, has_more })
                    }
                    Err(error) => supervisor_error_response(error),
                }
            }
            ApiMethod::Start { task_id } => {
                transition_response(&mut self.supervisor, task_id, TaskSupervisor::start)
            }
            ApiMethod::WaitForApproval { task_id } => transition_response(
                &mut self.supervisor,
                task_id,
                TaskSupervisor::wait_for_approval,
            ),
            ApiMethod::Approve { task_id } => transition_response(
                &mut self.supervisor,
                task_id,
                TaskSupervisor::resume_after_approval,
            ),
            ApiMethod::Succeed { task_id } => {
                transition_response(&mut self.supervisor, task_id, TaskSupervisor::succeed)
            }
            ApiMethod::Fail { task_id } => {
                transition_response(&mut self.supervisor, task_id, TaskSupervisor::fail)
            }
            ApiMethod::Cancel { task_id } => match self.supervisor.cancel(task_id) {
                Ok(changed) => task_response(&self.supervisor, task_id, |task| {
                    ApiResult::Cancelled { task, changed }
                }),
                Err(error) => supervisor_error_response(error),
            },
        }
    }
}

pub struct LocalServer {
    listener: UnixListener,
    socket_path: PathBuf,
    socket_device: u64,
    socket_inode: u64,
    config: ServerConfig,
}

impl LocalServer {
    pub fn bind(
        socket_path: impl AsRef<Path>,
        config: ServerConfig,
    ) -> Result<Self, LocalApiError> {
        let config = config.validate()?;
        let socket_path = socket_path.as_ref();
        prepare_socket_parent(socket_path)?;
        match socket_path.symlink_metadata() {
            Ok(_) => return Err(LocalApiError::SocketAlreadyExists),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(LocalApiError::Io(error)),
        }

        let listener = UnixListener::bind(socket_path).map_err(LocalApiError::Io)?;
        fs::set_permissions(socket_path, fs::Permissions::from_mode(0o600))
            .map_err(LocalApiError::Io)?;
        let metadata = socket_path.symlink_metadata().map_err(LocalApiError::Io)?;

        Ok(Self {
            listener,
            socket_path: socket_path.to_owned(),
            socket_device: metadata.dev(),
            socket_inode: metadata.ino(),
            config,
        })
    }

    pub fn serve_once<S: EventStore>(
        &self,
        service: &mut ApiService<S>,
    ) -> Result<(), LocalApiError> {
        let (mut stream, _) = self.listener.accept().map_err(LocalApiError::Io)?;
        configure_stream(&stream, self.config.connection_timeout)?;
        serve_stream(&mut stream, service, self.config.max_frame_bytes)
    }

    pub fn serve_forever<S: EventStore>(
        &self,
        service: &mut ApiService<S>,
    ) -> Result<(), LocalApiError> {
        loop {
            let (mut stream, _) = self.listener.accept().map_err(LocalApiError::Io)?;
            if configure_stream(&stream, self.config.connection_timeout).is_err() {
                continue;
            }
            let _ = serve_stream(&mut stream, service, self.config.max_frame_bytes);
        }
    }
}

impl Drop for LocalServer {
    fn drop(&mut self) {
        let Ok(metadata) = self.socket_path.symlink_metadata() else {
            return;
        };
        if metadata.file_type().is_socket()
            && metadata.dev() == self.socket_device
            && metadata.ino() == self.socket_inode
        {
            let _ = fs::remove_file(&self.socket_path);
        }
    }
}

pub fn send_request(
    socket_path: impl AsRef<Path>,
    request: &ApiRequest,
    config: ServerConfig,
) -> Result<ApiResponse, LocalApiError> {
    let config = config.validate()?;
    let mut stream = UnixStream::connect(socket_path).map_err(LocalApiError::Io)?;
    configure_stream(&stream, config.connection_timeout)?;
    let payload = serde_json::to_vec(request).map_err(|_| LocalApiError::Protocol)?;
    write_frame(&mut stream, &payload, config.max_frame_bytes)?;
    let response =
        read_frame(&mut stream, config.max_frame_bytes)?.ok_or(LocalApiError::Protocol)?;
    let response: ApiResponse =
        serde_json::from_slice(&response).map_err(|_| LocalApiError::Protocol)?;
    if response.protocol_version != PROTOCOL_VERSION {
        return Err(LocalApiError::Protocol);
    }
    Ok(response)
}

#[derive(Debug)]
pub enum LocalApiError {
    InvalidConfig,
    UnsafeSocketDirectory,
    SocketAlreadyExists,
    FrameTooLarge,
    Protocol,
    Io(io::Error),
}

impl Display for LocalApiError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidConfig => "invalid local API configuration",
            Self::UnsafeSocketDirectory => "socket directory permissions are unsafe",
            Self::SocketAlreadyExists => "socket path already exists",
            Self::FrameTooLarge => "frame exceeds configured limit",
            Self::Protocol => "invalid local API protocol data",
            Self::Io(_) => "local API I/O failure",
        };
        formatter.write_str(message)
    }
}

impl Error for LocalApiError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::InvalidConfig
            | Self::UnsafeSocketDirectory
            | Self::SocketAlreadyExists
            | Self::FrameTooLarge
            | Self::Protocol => None,
        }
    }
}

fn serve_stream<S: EventStore>(
    stream: &mut UnixStream,
    service: &mut ApiService<S>,
    max_frame_bytes: usize,
) -> Result<(), LocalApiError> {
    let response = match read_frame(stream, max_frame_bytes) {
        Ok(Some(payload)) => match serde_json::from_slice(&payload) {
            Ok(request) => service.handle(request),
            Err(_) => invalid_request(),
        },
        Ok(None) => return Err(LocalApiError::Protocol),
        Err(LocalApiError::FrameTooLarge) => {
            api_error(ApiErrorCode::FrameTooLarge, "request frame is too large")
        }
        Err(error) => return Err(error),
    };

    write_response(stream, response, max_frame_bytes)
}

fn write_response(
    stream: &mut UnixStream,
    response: ApiResponse,
    max_frame_bytes: usize,
) -> Result<(), LocalApiError> {
    let mut payload = serde_json::to_vec(&response).map_err(|_| LocalApiError::Protocol)?;
    if payload.len() > max_frame_bytes {
        payload = serde_json::to_vec(&api_error(
            ApiErrorCode::ResponseTooLarge,
            "response exceeds configured limit",
        ))
        .map_err(|_| LocalApiError::Protocol)?;
    }
    write_frame(stream, &payload, max_frame_bytes)
}

fn read_frame(
    reader: &mut impl Read,
    max_frame_bytes: usize,
) -> Result<Option<Vec<u8>>, LocalApiError> {
    let mut header = [0_u8; 4];
    let first = reader.read(&mut header[..1]).map_err(LocalApiError::Io)?;
    if first == 0 {
        return Ok(None);
    }
    reader
        .read_exact(&mut header[1..])
        .map_err(|_| LocalApiError::Protocol)?;
    let length =
        usize::try_from(u32::from_be_bytes(header)).map_err(|_| LocalApiError::Protocol)?;
    if length == 0 {
        return Err(LocalApiError::Protocol);
    }
    if length > max_frame_bytes {
        return Err(LocalApiError::FrameTooLarge);
    }

    let mut payload = vec![0_u8; length];
    reader
        .read_exact(&mut payload)
        .map_err(|_| LocalApiError::Protocol)?;
    Ok(Some(payload))
}

fn write_frame(
    writer: &mut impl Write,
    payload: &[u8],
    max_frame_bytes: usize,
) -> Result<(), LocalApiError> {
    if payload.is_empty() || payload.len() > max_frame_bytes {
        return Err(LocalApiError::FrameTooLarge);
    }
    let length = u32::try_from(payload.len()).map_err(|_| LocalApiError::FrameTooLarge)?;
    writer
        .write_all(&length.to_be_bytes())
        .and_then(|()| writer.write_all(payload))
        .and_then(|()| writer.flush())
        .map_err(LocalApiError::Io)
}

fn prepare_socket_parent(socket_path: &Path) -> Result<(), LocalApiError> {
    let parent = socket_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or(LocalApiError::InvalidConfig)?;
    match parent.symlink_metadata() {
        Ok(metadata) => {
            if metadata.file_type().is_symlink()
                || !metadata.is_dir()
                || metadata.permissions().mode() & 0o077 != 0
            {
                return Err(LocalApiError::UnsafeSocketDirectory);
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut builder = DirBuilder::new();
            builder.mode(0o700);
            builder.create(parent).map_err(LocalApiError::Io)?;
        }
        Err(error) => return Err(LocalApiError::Io(error)),
    }
    Ok(())
}

fn configure_stream(stream: &UnixStream, timeout: Duration) -> Result<(), LocalApiError> {
    stream
        .set_read_timeout(Some(timeout))
        .and_then(|()| stream.set_write_timeout(Some(timeout)))
        .map_err(LocalApiError::Io)
}

fn transition_response<S: EventStore>(
    supervisor: &mut TaskSupervisor<S>,
    task_id: TaskId,
    transition: fn(&mut TaskSupervisor<S>, TaskId) -> Result<(), SupervisorError>,
) -> ApiResponse {
    match transition(supervisor, task_id) {
        Ok(()) => task_response(supervisor, task_id, |task| ApiResult::Task { task }),
        Err(error) => supervisor_error_response(error),
    }
}

fn task_response<S: EventStore>(
    supervisor: &TaskSupervisor<S>,
    task_id: TaskId,
    result: impl FnOnce(TaskView) -> ApiResult,
) -> ApiResponse {
    supervisor.get(task_id).map_or_else(
        || api_error(ApiErrorCode::TaskNotFound, "task not found"),
        |task| ok(result(task.into())),
    )
}

fn submission_response(result: SubmitResult) -> ApiResponse {
    match result {
        SubmitResult::Accepted(task) => ok(ApiResult::Submitted {
            disposition: SubmissionDisposition::Accepted,
            task: task.into(),
            validation_errors: Vec::new(),
        }),
        SubmitResult::Existing(task) => ok(ApiResult::Submitted {
            disposition: SubmissionDisposition::Existing,
            task: task.into(),
            validation_errors: Vec::new(),
        }),
        SubmitResult::Rejected { task, errors } => ok(ApiResult::Submitted {
            disposition: SubmissionDisposition::Rejected,
            task: task.into(),
            validation_errors: validation_issues(errors),
        }),
    }
}

fn validation_issues(errors: ValidationErrors) -> Vec<ValidationIssue> {
    errors
        .errors()
        .iter()
        .map(|error| ValidationIssue {
            field: error.field().to_owned(),
            message: error.message().to_owned(),
        })
        .collect()
}

fn supervisor_error_response(error: SupervisorError) -> ApiResponse {
    match error {
        SupervisorError::TaskNotFound => api_error(ApiErrorCode::TaskNotFound, "task not found"),
        SupervisorError::IdempotencyConflict => api_error(
            ApiErrorCode::IdempotencyConflict,
            "idempotency key conflicts with existing input",
        ),
        SupervisorError::CapacityExceeded => {
            api_error(ApiErrorCode::CapacityExceeded, "task capacity exceeded")
        }
        SupervisorError::InvalidStateTransition(_) => api_error(
            ApiErrorCode::InvalidStateTransition,
            "requested task transition is not allowed",
        ),
        SupervisorError::EventStore(_) => api_error(
            ApiErrorCode::StorageUnavailable,
            "event storage is unavailable",
        ),
    }
}

fn ok(result: ApiResult) -> ApiResponse {
    ApiResponse {
        protocol_version: PROTOCOL_VERSION,
        outcome: ApiOutcome::Ok { result },
    }
}

fn invalid_request() -> ApiResponse {
    api_error(ApiErrorCode::InvalidRequest, "request is invalid")
}

fn api_error(code: ApiErrorCode, message: &'static str) -> ApiResponse {
    ApiResponse {
        protocol_version: PROTOCOL_VERSION,
        outcome: ApiOutcome::Error {
            error: ApiError {
                code,
                message: message.to_owned(),
            },
        },
    }
}

const fn default_event_page_size() -> u16 {
    DEFAULT_EVENT_PAGE_SIZE
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Cursor;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixStream;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;

    use aios_core::{TaskSpec, TaskState};
    use aios_runtime::{InMemoryEventStore, TaskSupervisor};

    use super::{
        ApiMethod, ApiOutcome, ApiRequest, ApiResult, ApiService, LocalApiError, LocalServer,
        PROTOCOL_VERSION, ServerConfig, read_frame, send_request, write_frame,
    };

    struct TestSocketDirectory {
        path: PathBuf,
    }

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    impl TestSocketDirectory {
        fn new() -> Self {
            let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            Self {
                path: std::env::temp_dir().join(format!("ao-{}-{sequence}", std::process::id())),
            }
        }

        fn socket_path(&self) -> PathBuf {
            self.path.join("aiosd.sock")
        }
    }

    impl Drop for TestSocketDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_file(self.socket_path());
            let _ = fs::remove_dir(&self.path);
        }
    }

    #[test]
    fn framing_round_trip_preserves_payload() {
        let payload = br#"{"method":"health"}"#;
        let mut bytes = Vec::new();
        write_frame(&mut bytes, payload, 1024).expect("write frame");

        let mut cursor = Cursor::new(bytes);
        assert_eq!(
            read_frame(&mut cursor, 1024).expect("read frame"),
            Some(payload.to_vec())
        );
    }

    #[test]
    fn framing_rejects_oversized_payload_before_allocation() {
        let mut cursor = Cursor::new(2048_u32.to_be_bytes());
        assert!(matches!(
            read_frame(&mut cursor, 1024),
            Err(LocalApiError::FrameTooLarge)
        ));
    }

    #[test]
    fn service_reports_health_without_task_access() {
        let supervisor = TaskSupervisor::new(InMemoryEventStore::default());
        let mut service = ApiService::new(supervisor);
        let response = service.handle(ApiRequest::new(ApiMethod::Health));
        let json = serde_json::to_string(&response).expect("serialize response");

        assert_eq!(response.protocol_version, PROTOCOL_VERSION);
        assert!(json.contains("healthy"));
        assert!(!json.contains("goal"));
    }

    #[test]
    fn service_rejects_unsupported_protocol_version() {
        let supervisor = TaskSupervisor::new(InMemoryEventStore::default());
        let mut service = ApiService::new(supervisor);
        let response = service.handle(ApiRequest {
            protocol_version: PROTOCOL_VERSION + 1,
            request: ApiMethod::Health,
        });

        assert!(matches!(
            response.outcome,
            ApiOutcome::Error {
                error: super::ApiError {
                    code: super::ApiErrorCode::UnsupportedProtocolVersion,
                    ..
                }
            }
        ));
    }

    #[test]
    fn service_runs_task_lifecycle_and_pages_events() {
        let task: TaskSpec = serde_json::from_str(include_str!("../../../examples/task.json"))
            .expect("valid example task");
        let supervisor = TaskSupervisor::new(InMemoryEventStore::default());
        let mut service = ApiService::new(supervisor);

        let task_id = match service
            .handle(ApiRequest::new(ApiMethod::Submit {
                task: Box::new(task),
            }))
            .outcome
        {
            ApiOutcome::Ok {
                result: ApiResult::Submitted { task, .. },
            } => task.task_id,
            _ => panic!("task should be accepted"),
        };

        for request in [
            ApiMethod::Start { task_id },
            ApiMethod::WaitForApproval { task_id },
            ApiMethod::Approve { task_id },
            ApiMethod::Succeed { task_id },
        ] {
            assert!(matches!(
                service.handle(ApiRequest::new(request)).outcome,
                ApiOutcome::Ok { .. }
            ));
        }

        assert!(matches!(
            service
                .handle(ApiRequest::new(ApiMethod::GetTask { task_id }))
                .outcome,
            ApiOutcome::Ok {
                result: ApiResult::Task { task }
            } if task.state == TaskState::Succeeded
        ));
        assert!(matches!(
            service.handle(ApiRequest::new(ApiMethod::Events {
                task_id,
                after_sequence: 0,
                limit: 2,
            })).outcome,
            ApiOutcome::Ok {
                result: ApiResult::Events {
                    events,
                    has_more: true,
                }
            } if events.len() == 2
        ));
    }

    #[test]
    fn socket_is_owner_only_and_removed_on_drop() {
        let directory = TestSocketDirectory::new();
        let socket_path = directory.socket_path();
        let server = LocalServer::bind(&socket_path, ServerConfig::default()).expect("bind socket");

        let mode = fs::metadata(&socket_path)
            .expect("socket metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0);
        drop(server);
        assert!(!socket_path.exists());
    }

    #[test]
    fn rejects_socket_directory_with_public_permissions() {
        let directory = TestSocketDirectory::new();
        fs::create_dir(&directory.path).expect("create directory");
        fs::set_permissions(&directory.path, fs::Permissions::from_mode(0o755))
            .expect("set directory permissions");

        assert!(matches!(
            LocalServer::bind(directory.socket_path(), ServerConfig::default()),
            Err(LocalApiError::UnsafeSocketDirectory)
        ));
    }

    #[test]
    fn server_handles_one_health_request() {
        let directory = TestSocketDirectory::new();
        let socket_path = directory.socket_path();
        let server = LocalServer::bind(&socket_path, ServerConfig::default()).expect("bind socket");

        let handle = thread::spawn(move || {
            let supervisor = TaskSupervisor::new(InMemoryEventStore::default());
            let mut service = ApiService::new(supervisor);
            server.serve_once(&mut service).expect("serve request");
        });

        let mut stream = UnixStream::connect(&socket_path).expect("connect socket");
        let request = br#"{"protocol_version":1,"request":{"method":"health"}}"#;
        write_frame(&mut stream, request, 1024).expect("write request");
        let response = read_frame(&mut stream, 1024)
            .expect("read response")
            .expect("response frame");
        let json = String::from_utf8(response).expect("UTF-8 response");
        assert!(json.contains("\"protocol_version\":1"));
        assert!(json.contains("healthy"));

        handle.join().expect("server thread");
    }

    #[test]
    fn client_and_server_exchange_versioned_health_request() {
        let directory = TestSocketDirectory::new();
        let socket_path = directory.socket_path();
        let server = LocalServer::bind(&socket_path, ServerConfig::default()).expect("bind socket");

        let handle = thread::spawn(move || {
            let supervisor = TaskSupervisor::new(InMemoryEventStore::default());
            let mut service = ApiService::new(supervisor);
            server.serve_once(&mut service).expect("serve request");
        });

        let response = send_request(
            &socket_path,
            &ApiRequest::new(ApiMethod::Health),
            ServerConfig::default(),
        )
        .expect("health response");
        assert_eq!(response.protocol_version, PROTOCOL_VERSION);
        assert!(matches!(
            response.outcome,
            ApiOutcome::Ok {
                result: ApiResult::Healthy
            }
        ));

        handle.join().expect("server thread");
    }

    #[test]
    fn client_and_server_exchange_task_submission() {
        let directory = TestSocketDirectory::new();
        let socket_path = directory.socket_path();
        let server = LocalServer::bind(&socket_path, ServerConfig::default()).expect("bind socket");

        let handle = thread::spawn(move || {
            let supervisor = TaskSupervisor::new(InMemoryEventStore::default());
            let mut service = ApiService::new(supervisor);
            server.serve_once(&mut service).expect("serve request");
        });

        let task = serde_json::from_str(include_str!("../../../examples/task.json"))
            .expect("valid example task");
        let response = send_request(
            &socket_path,
            &ApiRequest::new(ApiMethod::Submit {
                task: Box::new(task),
            }),
            ServerConfig::default(),
        )
        .expect("submission response");
        assert!(matches!(
            response.outcome,
            ApiOutcome::Ok {
                result: ApiResult::Submitted { .. }
            }
        ));

        handle.join().expect("server thread");
    }

    #[test]
    fn rejects_existing_socket_path_without_deleting_it() {
        let directory = TestSocketDirectory::new();
        fs::create_dir(&directory.path).expect("create directory");
        fs::set_permissions(&directory.path, fs::Permissions::from_mode(0o700))
            .expect("set permissions");
        let socket_path = directory.socket_path();
        fs::write(&socket_path, b"do not delete").expect("create file");

        assert!(matches!(
            LocalServer::bind(&socket_path, ServerConfig::default()),
            Err(LocalApiError::SocketAlreadyExists)
        ));
        assert_eq!(
            fs::read(&socket_path).expect("existing file"),
            b"do not delete"
        );
    }
}
