//! IP-bound bounded TCP Network Adapter for AI OS.
//!
//! The adapter performs one deliberately narrow operation: connect directly to one explicit IP
//! address and TCP port, write bounded bytes, close the write half, read a bounded response, and
//! close the socket. It does not resolve DNS, interpret application protocols, follow redirects,
//! use proxies, negotiate TLS, expose a socket, or accept inbound connections.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpStream};
use std::time::Duration;

use aios_core::{CapabilityRequest, NetworkTransport};
use aios_runtime::{
    ApprovalId, EventStore, Executed, ExecutionAdapter, ExecutionError, ExecutionGate,
    ExecutionOutcome, GuardedOperation, TaskExecutionContext, TaskId, TaskSnapshot, TaskSupervisor,
};

pub const MAX_NETWORK_HOST_BYTES: usize = 253;
pub const MAX_TCP_REQUEST_BYTES: usize = 64 * 1_024;
pub const MAX_TCP_RESPONSE_BYTES: usize = 1_024 * 1_024;
pub const MAX_NETWORK_TIMEOUT: Duration = Duration::from_secs(30);

/// Stable, redacted Network Adapter failure categories.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkAdapterError {
    InvalidConfig,
    InvalidOperation,
    UnsupportedDestination,
    ScopeMismatch,
    BudgetExceeded,
    ConnectFailed,
    SocketConfigurationFailed,
    PeerMismatch,
    WriteFailed,
    ReadFailed,
    ResponseTooLarge,
}

impl Display for NetworkAdapterError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidConfig => "invalid Network Adapter configuration",
            Self::InvalidOperation => "invalid network operation",
            Self::UnsupportedDestination => "network destination is unsupported",
            Self::ScopeMismatch => "network operation scope does not match the Task",
            Self::BudgetExceeded => "network operation exceeded the Task Budget",
            Self::ConnectFailed => "network connection failed",
            Self::SocketConfigurationFailed => "network socket configuration failed",
            Self::PeerMismatch => "network peer verification failed",
            Self::WriteFailed => "network write failed",
            Self::ReadFailed => "network read failed",
            Self::ResponseTooLarge => "network response exceeded its limit",
        };
        formatter.write_str(message)
    }
}

impl Error for NetworkAdapterError {}

/// Complete one-shot TCP exchange retained privately while approval is pending.
///
/// This type intentionally does not implement `Clone`, `Debug`, or serialization because the
/// destination and request bytes may be sensitive.
pub struct TcpExchangeOperation {
    host: String,
    port: u16,
    request: Vec<u8>,
    execution_context: Option<TaskExecutionContext>,
}

impl GuardedOperation for TcpExchangeOperation {
    fn capability_request(&self) -> CapabilityRequest<'_> {
        CapabilityRequest::Network {
            host: &self.host,
            transport: NetworkTransport::Tcp,
            port: self.port,
        }
    }
}

/// Safe operation constructor that validates all model-controlled values before authorization.
#[derive(Default)]
pub struct NetworkCatalog;

impl NetworkCatalog {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    pub fn prepare_tcp_exchange(
        &self,
        host: String,
        port: u16,
        request: Vec<u8>,
    ) -> Result<TcpExchangeOperation, NetworkAdapterError> {
        validate_operation(&host, port, &request)?;
        Ok(TcpExchangeOperation {
            host,
            port,
            request,
            execution_context: None,
        })
    }
}

/// Bounded untrusted bytes returned by one completed exchange.
///
/// The response intentionally omits `Clone`, `Debug`, and serialization.
pub struct TcpExchangeResponse {
    bytes: Vec<u8>,
}

impl TcpExchangeResponse {
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

struct NetworkAdapter {
    timeout: Duration,
}

impl NetworkAdapter {
    fn new(timeout: Duration) -> Result<Self, NetworkAdapterError> {
        if timeout.is_zero() || timeout > MAX_NETWORK_TIMEOUT {
            return Err(NetworkAdapterError::InvalidConfig);
        }
        Ok(Self { timeout })
    }
}

impl ExecutionAdapter<TcpExchangeOperation> for NetworkAdapter {
    type Output = TcpExchangeResponse;
    type Error = NetworkAdapterError;

    fn execute(&mut self, operation: TcpExchangeOperation) -> Result<Self::Output, Self::Error> {
        let ip = validate_operation(&operation.host, operation.port, &operation.request)?;
        let context = operation
            .execution_context
            .as_ref()
            .ok_or(NetworkAdapterError::ScopeMismatch)?;
        let destination = SocketAddr::new(ip, operation.port);
        let connect_timeout = effective_timeout(context, self.timeout)?;
        let mut stream = TcpStream::connect_timeout(&destination, connect_timeout)
            .map_err(|_| io_failure(context, NetworkAdapterError::ConnectFailed))?;
        require_remaining_budget(context)?;
        if stream
            .peer_addr()
            .map_err(|_| NetworkAdapterError::PeerMismatch)?
            != destination
        {
            return Err(NetworkAdapterError::PeerMismatch);
        }
        write_request(&mut stream, &operation.request, context, self.timeout)?;
        require_remaining_budget(context)?;
        stream
            .shutdown(Shutdown::Write)
            .map_err(|_| io_failure(context, NetworkAdapterError::WriteFailed))?;
        let bytes = read_response(&mut stream, context, self.timeout)?;
        Ok(TcpExchangeResponse { bytes })
    }
}

/// Capability- and approval-gated facade that never exposes the raw adapter or socket.
pub struct NetworkExecutionGate {
    gate: ExecutionGate<NetworkAdapter, TcpExchangeOperation>,
}

impl NetworkExecutionGate {
    pub fn new(timeout: Duration) -> Result<Self, NetworkAdapterError> {
        Ok(Self {
            gate: ExecutionGate::new(NetworkAdapter::new(timeout)?),
        })
    }

    pub fn request<S: EventStore>(
        &mut self,
        supervisor: &mut TaskSupervisor<S>,
        task_id: TaskId,
        operation: TcpExchangeOperation,
        approval_ttl: Duration,
    ) -> Result<ExecutionOutcome<TcpExchangeResponse>, ExecutionError<NetworkAdapterError>> {
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
    ) -> Result<Executed<TcpExchangeResponse>, ExecutionError<NetworkAdapterError>> {
        self.gate.approve_and_execute(supervisor, approval_id)
    }

    pub fn deny<S: EventStore>(
        &mut self,
        supervisor: &mut TaskSupervisor<S>,
        approval_id: ApprovalId,
    ) -> Result<TaskSnapshot, ExecutionError<NetworkAdapterError>> {
        self.gate.deny(supervisor, approval_id)
    }

    pub fn expire<S: EventStore>(
        &mut self,
        supervisor: &mut TaskSupervisor<S>,
    ) -> Result<usize, ExecutionError<NetworkAdapterError>> {
        self.gate.expire(supervisor)
    }

    pub fn cancel<S: EventStore>(
        &mut self,
        supervisor: &mut TaskSupervisor<S>,
        task_id: TaskId,
    ) -> Result<bool, ExecutionError<NetworkAdapterError>> {
        self.gate.cancel(supervisor, task_id)
    }

    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.gate.pending_count()
    }
}

fn validate_operation(
    host: &str,
    port: u16,
    request: &[u8],
) -> Result<IpAddr, NetworkAdapterError> {
    if host.is_empty()
        || host.len() > MAX_NETWORK_HOST_BYTES
        || port == 0
        || request.len() > MAX_TCP_REQUEST_BYTES
    {
        return Err(NetworkAdapterError::InvalidOperation);
    }
    let ip = host
        .parse::<IpAddr>()
        .map_err(|_| NetworkAdapterError::UnsupportedDestination)?;
    if ip.is_unspecified() || ip.is_multicast() || ip == IpAddr::V4(Ipv4Addr::BROADCAST) {
        return Err(NetworkAdapterError::UnsupportedDestination);
    }
    Ok(ip)
}

fn effective_timeout(
    context: &TaskExecutionContext,
    configured_timeout: Duration,
) -> Result<Duration, NetworkAdapterError> {
    let remaining = context.remaining_wall_time();
    if remaining.is_zero() {
        return Err(NetworkAdapterError::BudgetExceeded);
    }
    Ok(remaining.min(configured_timeout))
}

fn require_remaining_budget(context: &TaskExecutionContext) -> Result<(), NetworkAdapterError> {
    effective_timeout(context, MAX_NETWORK_TIMEOUT).map(|_| ())
}

fn io_failure(
    context: &TaskExecutionContext,
    fallback: NetworkAdapterError,
) -> NetworkAdapterError {
    if context.remaining_wall_time().is_zero() {
        NetworkAdapterError::BudgetExceeded
    } else {
        fallback
    }
}

fn write_request(
    stream: &mut TcpStream,
    request: &[u8],
    context: &TaskExecutionContext,
    configured_timeout: Duration,
) -> Result<(), NetworkAdapterError> {
    let mut offset = 0;
    let mut installed_timeout = None;
    while offset < request.len() {
        let timeout = effective_timeout(context, configured_timeout)?;
        if installed_timeout != Some(timeout) {
            stream
                .set_write_timeout(Some(timeout))
                .map_err(|_| io_failure(context, NetworkAdapterError::SocketConfigurationFailed))?;
            installed_timeout = Some(timeout);
        }
        let written = stream
            .write(&request[offset..])
            .map_err(|_| io_failure(context, NetworkAdapterError::WriteFailed))?;
        if written == 0 {
            return Err(NetworkAdapterError::WriteFailed);
        }
        offset += written;
    }
    Ok(())
}

fn read_response(
    stream: &mut TcpStream,
    context: &TaskExecutionContext,
    configured_timeout: Duration,
) -> Result<Vec<u8>, NetworkAdapterError> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8 * 1_024];
    let mut installed_timeout = None;
    loop {
        let timeout = effective_timeout(context, configured_timeout)?;
        if installed_timeout != Some(timeout) {
            stream
                .set_read_timeout(Some(timeout))
                .map_err(|_| io_failure(context, NetworkAdapterError::SocketConfigurationFailed))?;
            installed_timeout = Some(timeout);
        }
        let remaining_capacity = MAX_TCP_RESPONSE_BYTES + 1 - bytes.len();
        let read_limit = remaining_capacity.min(buffer.len());
        let read = stream
            .read(&mut buffer[..read_limit])
            .map_err(|_| io_failure(context, NetworkAdapterError::ReadFailed))?;
        if read == 0 {
            require_remaining_budget(context)?;
            return Ok(bytes);
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.len() > MAX_TCP_RESPONSE_BYTES {
            return Err(NetworkAdapterError::ResponseTooLarge);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{ErrorKind, Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    use aios_core::{
        ApprovalPolicy, Budget, CapabilitySet, NetworkDestination, NetworkPolicy, NetworkTransport,
        TaskSpec,
    };
    use aios_runtime::{ExecutionError, ExecutionOutcome, SubmitResult, TaskId, TaskSupervisor};

    use super::{
        MAX_NETWORK_HOST_BYTES, MAX_NETWORK_TIMEOUT, MAX_TCP_REQUEST_BYTES, MAX_TCP_RESPONSE_BYTES,
        NetworkAdapterError, NetworkCatalog, NetworkExecutionGate,
    };

    fn task(host: &str, port: u16, required_for: &[&str]) -> TaskSpec {
        TaskSpec {
            idempotency_key: format!("network-adapter-{}", TaskId::new()),
            goal: "Perform one bounded TCP exchange".to_owned(),
            capabilities: CapabilitySet {
                filesystem: Vec::new(),
                network: NetworkPolicy::Allow {
                    destinations: vec![NetworkDestination {
                        host: host.to_owned(),
                        transport: NetworkTransport::Tcp,
                        port,
                    }],
                },
                tools: Vec::new(),
            },
            budget: Budget {
                wall_time_seconds: 60,
                memory_bytes: 64 * 1_024 * 1_024,
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
        host: &str,
        port: u16,
        required_for: &[&str],
    ) -> (TaskSupervisor, TaskId) {
        running_supervisor_with_wall_time(host, port, required_for, 60)
    }

    fn running_supervisor_with_wall_time(
        host: &str,
        port: u16,
        required_for: &[&str],
        wall_time_seconds: u64,
    ) -> (TaskSupervisor, TaskId) {
        let mut supervisor = TaskSupervisor::default();
        let mut spec = task(host, port, required_for);
        spec.budget.wall_time_seconds = wall_time_seconds;
        let SubmitResult::Accepted(task) = supervisor.submit(spec).expect("submit Task") else {
            panic!("expected accepted Task");
        };
        supervisor.start(task.task_id).expect("start Task");
        (supervisor, task.task_id)
    }

    fn alternate_port(port: u16) -> u16 {
        if port == u16::MAX { port - 1 } else { port + 1 }
    }

    #[test]
    fn catalog_accepts_ip_destinations_and_bounded_requests() {
        let catalog = NetworkCatalog::new();
        for host in ["127.0.0.1", "::1"] {
            assert!(
                catalog
                    .prepare_tcp_exchange(host.to_owned(), 443, b"request".to_vec())
                    .is_ok()
            );
        }
    }

    #[test]
    fn catalog_rejects_hostnames_unsafe_addresses_and_invalid_bounds() {
        let catalog = NetworkCatalog::new();
        for host in [
            "api.example.com",
            "0.0.0.0",
            "::",
            "224.0.0.1",
            "ff02::1",
            "255.255.255.255",
        ] {
            assert!(matches!(
                catalog.prepare_tcp_exchange(host.to_owned(), 443, Vec::new()),
                Err(NetworkAdapterError::UnsupportedDestination)
            ));
        }
        assert!(matches!(
            catalog.prepare_tcp_exchange(String::new(), 443, Vec::new()),
            Err(NetworkAdapterError::InvalidOperation)
        ));
        assert!(matches!(
            catalog.prepare_tcp_exchange("1".repeat(MAX_NETWORK_HOST_BYTES + 1), 443, Vec::new(),),
            Err(NetworkAdapterError::InvalidOperation)
        ));
        assert!(matches!(
            catalog.prepare_tcp_exchange("127.0.0.1".to_owned(), 0, Vec::new()),
            Err(NetworkAdapterError::InvalidOperation)
        ));
        assert!(matches!(
            catalog.prepare_tcp_exchange(
                "127.0.0.1".to_owned(),
                443,
                vec![0; MAX_TCP_REQUEST_BYTES + 1],
            ),
            Err(NetworkAdapterError::InvalidOperation)
        ));
    }

    #[test]
    fn gate_rejects_zero_and_excessive_timeouts() {
        assert!(matches!(
            NetworkExecutionGate::new(Duration::ZERO),
            Err(NetworkAdapterError::InvalidConfig)
        ));
        assert!(matches!(
            NetworkExecutionGate::new(MAX_NETWORK_TIMEOUT + Duration::from_nanos(1)),
            Err(NetworkAdapterError::InvalidConfig)
        ));
    }

    #[test]
    fn executes_exact_ip_and_port_without_exposing_socket() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind listener");
        let port = listener.local_addr().expect("listener address").port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept connection");
            let mut request = Vec::new();
            stream.read_to_end(&mut request).expect("read request");
            stream
                .write_all(b"bounded response")
                .expect("write response");
            request
        });
        let catalog = NetworkCatalog::new();
        let mut gate =
            NetworkExecutionGate::new(Duration::from_secs(2)).expect("create network gate");
        let (mut supervisor, task_id) = running_supervisor("127.0.0.1", port, &[]);

        let result = gate
            .request(
                &mut supervisor,
                task_id,
                catalog
                    .prepare_tcp_exchange("127.0.0.1".to_owned(), port, b"exact request".to_vec())
                    .expect("prepare exchange"),
                Duration::from_secs(30),
            )
            .expect("execute exchange");
        let ExecutionOutcome::Executed(executed) = result else {
            panic!("expected execution");
        };

        assert_eq!(executed.output.as_bytes(), b"bounded response");
        assert_eq!(server.join().expect("join server"), b"exact request");
    }

    #[test]
    fn approval_retains_exchange_without_connecting_early() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind listener");
        listener
            .set_nonblocking(true)
            .expect("set nonblocking listener");
        let port = listener.local_addr().expect("listener address").port();
        let catalog = NetworkCatalog::new();
        let mut gate =
            NetworkExecutionGate::new(Duration::from_secs(2)).expect("create network gate");
        let (mut supervisor, task_id) = running_supervisor("127.0.0.1", port, &["network.egress"]);

        let result = gate
            .request(
                &mut supervisor,
                task_id,
                catalog
                    .prepare_tcp_exchange(
                        "127.0.0.1".to_owned(),
                        port,
                        b"approved request".to_vec(),
                    )
                    .expect("prepare exchange"),
                Duration::from_secs(30),
            )
            .expect("request approval");
        let ExecutionOutcome::ApprovalRequired(request) = result else {
            panic!("expected approval request");
        };
        assert_eq!(
            listener
                .accept()
                .expect_err("must not connect before approval")
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
            stream
                .write_all(b"approved response")
                .expect("write response");
            bytes
        });
        let executed = gate
            .approve_and_execute(&mut supervisor, request.approval_id)
            .expect("approve and execute");

        assert_eq!(executed.output.as_bytes(), b"approved response");
        assert_eq!(server.join().expect("join server"), b"approved request");
        assert_eq!(gate.pending_count(), 0);
    }

    #[test]
    fn approval_wait_cannot_reset_task_wall_time() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind listener");
        listener
            .set_nonblocking(true)
            .expect("set nonblocking listener");
        let port = listener.local_addr().expect("listener address").port();
        let catalog = NetworkCatalog::new();
        let mut gate =
            NetworkExecutionGate::new(Duration::from_secs(5)).expect("create network gate");
        let (mut supervisor, task_id) =
            running_supervisor_with_wall_time("127.0.0.1", port, &["network.egress"], 1);
        let result = gate
            .request(
                &mut supervisor,
                task_id,
                catalog
                    .prepare_tcp_exchange(
                        "127.0.0.1".to_owned(),
                        port,
                        b"must not be sent".to_vec(),
                    )
                    .expect("prepare exchange"),
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
            Err(ExecutionError::Adapter(NetworkAdapterError::BudgetExceeded))
        ));
        assert_eq!(
            listener
                .accept()
                .expect_err("expired Task must not connect")
                .kind(),
            ErrorKind::WouldBlock
        );
        assert_eq!(gate.pending_count(), 0);
    }

    #[test]
    fn task_wall_time_bounds_an_in_flight_read() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind listener");
        let port = listener.local_addr().expect("listener address").port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept connection");
            let mut request = Vec::new();
            stream.read_to_end(&mut request).expect("read request");
            thread::sleep(Duration::from_millis(1_500));
        });
        let catalog = NetworkCatalog::new();
        let mut gate =
            NetworkExecutionGate::new(Duration::from_secs(5)).expect("create network gate");
        let (mut supervisor, task_id) =
            running_supervisor_with_wall_time("127.0.0.1", port, &[], 1);

        let result = gate.request(
            &mut supervisor,
            task_id,
            catalog
                .prepare_tcp_exchange("127.0.0.1".to_owned(), port, b"request".to_vec())
                .expect("prepare exchange"),
            Duration::from_secs(30),
        );

        assert!(matches!(
            result,
            Err(ExecutionError::Adapter(NetworkAdapterError::BudgetExceeded))
        ));
        server.join().expect("join server");
    }

    #[test]
    fn denied_port_never_connects() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind listener");
        listener
            .set_nonblocking(true)
            .expect("set nonblocking listener");
        let port = listener.local_addr().expect("listener address").port();
        let catalog = NetworkCatalog::new();
        let mut gate =
            NetworkExecutionGate::new(Duration::from_secs(2)).expect("create network gate");
        let (mut supervisor, task_id) = running_supervisor("127.0.0.1", alternate_port(port), &[]);

        let result = gate
            .request(
                &mut supervisor,
                task_id,
                catalog
                    .prepare_tcp_exchange("127.0.0.1".to_owned(), port, Vec::new())
                    .expect("prepare exchange"),
                Duration::from_secs(30),
            )
            .expect("evaluate exchange");

        assert!(matches!(result, ExecutionOutcome::Denied { .. }));
        assert_eq!(
            listener
                .accept()
                .expect_err("denial must not connect")
                .kind(),
            ErrorKind::WouldBlock
        );
    }

    #[test]
    fn rejects_response_above_the_bounded_limit() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind listener");
        let port = listener.local_addr().expect("listener address").port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept connection");
            let mut request = Vec::new();
            stream.read_to_end(&mut request).expect("read request");
            stream
                .write_all(&vec![b'x'; MAX_TCP_RESPONSE_BYTES + 1])
                .expect("write oversized response");
        });
        let catalog = NetworkCatalog::new();
        let mut gate =
            NetworkExecutionGate::new(Duration::from_secs(2)).expect("create network gate");
        let (mut supervisor, task_id) = running_supervisor("127.0.0.1", port, &[]);

        let result = gate.request(
            &mut supervisor,
            task_id,
            catalog
                .prepare_tcp_exchange("127.0.0.1".to_owned(), port, Vec::new())
                .expect("prepare exchange"),
            Duration::from_secs(30),
        );

        assert!(matches!(
            result,
            Err(aios_runtime::ExecutionError::Adapter(
                NetworkAdapterError::ResponseTooLarge
            ))
        ));
        server.join().expect("join server");
    }
}
