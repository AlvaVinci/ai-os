# ADR-0013: Bind Task read Capabilities to opened filesystem objects

- Status: Accepted
- Date: 2026-07-29
- Related threats: TM-007, TM-009, TM-020
- Related objectives: SEC-002, SEC-008, SEC-009

## Context

The stable Task schema authorizes normalized absolute filesystem paths lexically, while the Bubblewrap backend previously accepted only trusted host paths for its rootfs and Task scratch. Adding a general host bind path after policy evaluation would reintroduce symlink and time-of-check/time-of-use races. Passing opened descriptors directly from the multi-threaded daemon by clearing close-on-exec would also create a window in which an unrelated concurrent child could inherit filesystem authority.

The policy deliberately treats `read` and `write` as independent. A conventional read-write bind mount cannot implement a write-only grant because it also permits reads. The first OS-backed increment therefore must not silently broaden write authority.

## Decision

- `TaskExecutionContext` carries a private clone of the immutable validated Filesystem Capability list. It remains non-debuggable and non-serializable.
- Trusted configuration supplies `BubblewrapProcessToolBuilder` with one host source root and the exact Task Capability list. Each Capability path is interpreted relative to that root and becomes the same absolute destination in the sandbox.
- This increment accepts only `read` Capabilities. Root, duplicate, malformed, excessive, missing, and `write` entries fail closed.
- The builder opens every source with Linux `openat2` using `O_PATH | O_CLOEXEC` and `RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS | RESOLVE_NO_SYMLINKS | RESOLVE_NO_XDEV`.
- A Task-bound Process call requires exact set equality between configured mounts and the current execution context before spawn.
- Opened Capability descriptors retain close-on-exec in the daemon. The parent creates a private Unix socket as launcher standard input and transfers the sealed seccomp descriptor plus the exact Capability descriptor count with `SCM_RIGHTS`.
- The single-threaded `aios-cgroup-launch` helper validates its fixed launch metadata, joins the Task cgroup, receives the descriptors with close-on-exec, clears that flag only immediately before replacing itself, and supplies Bubblewrap with `--ro-bind-fd`.
- The broker socket is replaced with null standard input during exec. Bubblewrap verifies each mounted object identity and closes the explicit descriptors before starting the Tool.
- Descriptor transfer, count, parsing, identity, or mount failure is fatal. There is no host-path or unfiltered fallback.
- Bubblewrap 0.11.2 is the minimum tested version. CI builds the official non-setuid release from its checksum-pinned archive.

## Consequences

- Renaming or replacing a Capability source path after handler construction does not redirect the mount to a different object.
- Symlink, magic-link, traversal, and mount crossing during initial Capability resolution fail closed.
- Privileged filesystem descriptors never become generally inheritable in the multi-threaded daemon.
- Read Capabilities can overlay narrower paths beneath the prepared root or writable Task scratch while remaining read-only.
- Trusted provisioning must create the source-root layout corresponding to Task Capability paths.
- `write` Capabilities remain policy-only and cannot be used by this Process mount backend. This is an explicit incomplete boundary, not an implicit read-write interpretation.
- Rootfs, Task scratch, and sandbox executable inputs remain path-plus-identity or digest based; this decision does not make them descriptor-bound.
- The helper and Bubblewrap briefly hold explicit descriptors as trusted launch authority, but the Tool receives none of them.

## Verification

- Unit tests cover normalized read entries and fail-closed write and duplicate entries.
- Supervisor tests prove the immutable Filesystem Capability list is released only in the running execution context.
- Linux cross-target compilation covers the broker, ancillary descriptor transfer, and `openat2` path.
- The Linux boundary suite opens one read Capability, replaces its host path, verifies that the original object is mounted, verifies that mutation fails, and removes the Task cgroup.
- CI verifies Bubblewrap exposes `--ro-bind-fd` before running the boundary suite.

## Alternatives considered

### Pass canonical host paths to Bubblewrap

Rejected. Canonicalization does not bind later use to the same object and leaves a replacement race.

### Clear close-on-exec in the daemon

Rejected for filesystem authority. A concurrent child could inherit the descriptor during the spawn window. The existing sealed seccomp descriptor contains no writable authority; Filesystem Capability descriptors do.

### Treat `write` as read-write

Rejected. The current Capability contract states that write does not imply read. Broadening it inside an adapter would violate the accepted Task.

### Use one standard descriptor per mount

Rejected. Standard input, output, and error have fixed null semantics and cannot represent the bounded maximum of 128 mounts.

### Add unsafe pre-exec descriptor remapping

Rejected. Workspace Rust forbids unsafe code, and the dedicated single-threaded broker provides an auditable safe-Rust boundary with the existing `rustix` dependency.
