# ADR-0006: Use a rootfs-scoped Bubblewrap backend for initial Linux process isolation

- Status: Accepted
- Date: 2026-07-22
- Related threats: TM-002, TM-003, TM-008, TM-009, TM-018, TM-019
- Related objectives: SEC-003, SEC-008, SEC-009, SEC-010

## Context

The direct Process Adapter constrains executable selection, arguments, environment, and direct-child lifetime, but the child still inherits the daemon's operating-system view. That view can include host files, network interfaces, IPC objects, credentials, and local control sockets. It therefore cannot run untrusted model or Tool code.

ADR-0001 also blocks a public approval API until runner isolation makes the approval channel unreachable. Owner-only permissions are insufficient while untrusted code runs with the same host user and filesystem view.

AI OS needs an incremental Linux backend that can be configured without running the daemon as root, preserves direct argument execution, and fails closed when isolation is unavailable. Bubblewrap constructs an empty mount namespace and supports user, PID, IPC, network, UTS, and cgroup namespaces. Its security depends on the caller supplying a complete restrictive policy; Bubblewrap alone is not a ready-made sandbox.

## Decision

The first Linux isolation backend uses an explicitly configured absolute Bubblewrap executable and a prepared root filesystem.

- The prepared root filesystem is mounted read-only at `/` and must not be the host root.
- One separate scratch directory is mounted read-write at `/workspace`. It must not contain or contain the prepared root filesystem.
- The sandbox creates all namespaces supported by `--unshare-all`. Network sharing is not configurable in this backend, so the sandbox has no host network access.
- Further user-namespace creation is disabled, all capabilities are dropped, and the command receives a new terminal session.
- `/proc`, a minimal `/dev`, and an in-memory `/tmp` are created inside the sandbox.
- The daemon passes only standard input, output, and error as null streams. Bubblewrap closes other file descriptors unless they are explicitly preserved; AI OS preserves none.
- The child receives only explicitly configured environment values. Shell execution and `PATH` lookup remain prohibited.
- Bubblewrap, the prepared executable, mount paths, and sandbox executable path are validated before spawn. Executable identity is checked again immediately before execution.
- Unsupported platforms, missing Bubblewrap, invalid mount layouts, namespace setup failure, and changed executable identity fail closed.

The existing `ProcessToolBuilder` remains available for trusted direct child processes and backward compatibility. Isolation requires the separate `BubblewrapProcessToolBuilder`; direct execution must not be described as sandboxed.

This decision is an isolation foundation, not completion of DOD-002, DOD-003, or DOD-004. The principal-separated approval API remains blocked until every untrusted model and Tool runner uses this boundary or a stronger one, the approval listener is separate from the Task API, Linux peer credentials authenticate the approver, and adversarial tests prove that runners cannot reach or inherit the listener.

## Consequences

- A runner cannot see host paths that are absent from the prepared root and scratch mounts. In particular, `/run`, the daemon socket, the approval socket, the event database, and host credentials must not be included in either mount.
- Network-denied Tools receive a kernel network namespace rather than a policy-only denial.
- A prepared root filesystem becomes a versioned runtime artifact with its own update and provenance requirements.
- Linux hosts must install a compatible unprivileged Bubblewrap and permit the required user namespaces. The runtime does not silently fall back to direct execution.
- The root filesystem and scratch directory remain host-path inputs selected by trusted startup code. They are not yet derived from a Task's Filesystem Capability.
- Bind-mount and executable checks still have host-side time-of-check/time-of-use windows. Descriptor-bound opening and immutable image verification remain required.
- CPU and memory limits, cgroup placement, seccomp, Landlock or descriptor-bound filesystem access, task-scoped scratch lifecycle, asynchronous cancellation, and Linux end-to-end evidence remain future work.
- The backend must not expose a network-enabled flag until the Network Adapter can bind an approved logical destination to an actual connection without giving the runner ambient network access.

## Verification for this increment

- Unit tests pin the exact namespace, mount, capability, and command argument sequence.
- Tests verify that no network-sharing option or `/run` mount enters the launch plan.
- Sandbox executable paths must be absolute, bounded, and traversal-free.
- Non-Linux builds reject the isolated builder with a stable unsupported-platform error.

Linux integration tests must later execute escape probes for filesystem visibility, inherited descriptors, network access, descendant cleanup, and control-socket reachability. Unit command-plan tests are not release evidence.

## Alternatives considered

### Invoke `unshare` and mount utilities directly

Rejected for the initial backend. Correct ordering, privilege dropping, PID 1 behavior, descriptor closure, and failure cleanup would duplicate security-sensitive launcher code across several programs.

### Implement namespace setup inside Rust immediately

Deferred. The workspace forbids unsafe Rust, and a direct implementation would require a new low-level dependency plus a larger security review. It would still need a complete mount and seccomp policy.

### Start with an OCI runtime

Deferred. OCI is a viable later backend but introduces a larger configuration and lifecycle surface than the first deny-network Tool runner requires.

### Reuse the host root read-only

Rejected. A read-only host root still exposes credentials, sockets, configuration, process metadata, and unrelated user data. The initial backend requires a prepared minimal root filesystem.

### Fall back to direct execution when Bubblewrap is unavailable

Rejected. Silent fallback would turn a host configuration problem into a security-boundary bypass.
