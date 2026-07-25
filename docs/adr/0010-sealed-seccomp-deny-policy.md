# ADR-0010: Apply a sealed seccomp deny policy to every Bubblewrap Tool process

- Status: Accepted
- Date: 2026-07-26
- Related threats: TM-009, TM-011, TM-016, TM-017, TM-020
- Related objectives: SEC-002, SEC-007, SEC-008, SEC-009

## Context

The Bubblewrap backend limits mounts, namespaces, capabilities, descriptors, network visibility, CPU time, and memory, but an untrusted Tool can still invoke every system call accepted by the host kernel. Namespaces and capability checks make many privileged calls fail, yet they do not reduce the kernel interfaces exposed to hostile code.

AI OS v0.1 targets Linux x86_64. Bubblewrap accepts classic BPF seccomp programs from an inherited file descriptor and applies them immediately before the sandbox command. The Process Adapter must supply that descriptor without a shell, without weakening its null standard-stream contract, and without adding unsafe Rust to the workspace.

## Decision

Every `BubblewrapProcessToolBuilder` execution applies one built-in Linux x86_64 seccomp policy. There is no opt-out or model-controlled policy selection.

- The policy first checks `AUDIT_ARCH_X86_64`. An unexpected audit architecture terminates the process.
- The policy rejects the x32 syscall range before evaluating individual syscall numbers.
- Syscall numbers newer than the reviewed Linux table terminate the process until the policy is explicitly updated.
- The reviewed deny set terminates calls that manage mounts, kernel modules, system time, swap, reboot, kernel logs, keyrings, BPF, performance events, cross-process memory, namespace reassociation, `io_uring`, userfault handling, and related privileged kernel surfaces.
- Other syscalls remain allowed. This is an explicit deny policy, not a general-purpose allowlist.
- The adapter serializes the policy as classic BPF into a fresh anonymous `memfd`, rewinds it, and seals writes, growth, shrinkage, and further seal changes.
- The descriptor starts close-on-exec while it is constructed. The adapter clears that flag only for the sealed descriptor needed by Bubblewrap and passes its exact numeric value through `--seccomp`.
- The descriptor remains alive until spawn completes. Bubblewrap reads and closes it before starting the Tool. Standard input, output, and error remain connected to null.
- Failure to create, write, seal, or expose the policy descriptor returns the stable `SeccompUnavailable` category. The adapter never retries without seccomp.
- Linux architectures other than x86_64 remain unsupported for the Bubblewrap backend until they receive their own reviewed syscall table and Linux boundary evidence.

The implementation uses the safe `rustix` file-descriptor and sealing APIs. The workspace continues to forbid unsafe Rust.

## Consequences

- Untrusted Tool code receives a smaller host-kernel attack surface even when a denied syscall would also have failed a later capability check.
- The policy is versioned with the Process Adapter, deterministic, bounded, and reviewable without reading an external profile at runtime.
- Programs that require a denied interface, including `io_uring`, mount manipulation, performance counters, or process tracing, fail inside this backend. Supporting one of those interfaces requires a new reviewed policy decision and adversarial tests.
- Clearing close-on-exec in the multi-threaded daemon creates a short possibility that another concurrently spawned trusted child inherits the sealed read-only policy descriptor. The descriptor contains no secret or writable authority, but a future dedicated launch broker should remove this inheritance window.
- A deny policy cannot prevent exploitation through allowed syscalls. Per-workload allowlists may be evaluated after the real model and Tool workloads are stable enough to produce reproducible syscall evidence.
- Seccomp does not replace descriptor-bound filesystem authorization, destination-bound networking, cgroups, immutable rootfs backing, or principal separation.

## Verification

- Unit tests pin the architecture check, x32 rejection, sorted unique syscall table, fail-closed return actions, and final allow action.
- Launch-plan tests require `--seccomp` with the exact generated descriptor and continue to reject network sharing and undeclared mounts.
- The Linux x86_64 boundary suite confirms that the Tool reports seccomp filter mode and that a host-permitted `personality` operation is terminated inside the sandbox.
- Existing filesystem, network, descriptor, descendant, rootfs-integrity, CPU, and memory boundary tests run with the seccomp policy installed.

## Alternatives considered

### Keep namespace and capability controls only

Rejected. Those controls constrain authority but still expose unnecessary kernel entry points to hostile code.

### Load an administrator-provided raw BPF file

Rejected for v0.1. It adds profile identity, ownership, replacement, compatibility, and deployment failure modes without a current need for multiple policies.

### Pass the filter through standard input

Rejected. Bubblewrap consumes and closes the supplied descriptor, which would violate the Process Adapter's null standard-input contract and could let a later open reuse descriptor zero.

### Use a strict per-Tool allowlist immediately

Deferred. The supported real model and Tool workload set is not stable enough to justify a complete allowlist. An incomplete allowlist would create compatibility pressure to add broad syscalls without evidence.

### Implement raw descriptor operations with unsafe Rust

Rejected. The workspace forbids unsafe Rust, and maintained safe wrappers provide the required anonymous-file, sealing, and descriptor-flag operations.
