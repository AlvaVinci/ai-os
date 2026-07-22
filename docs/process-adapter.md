# Process Adapter

## Status

Experimental child-process Tool handlers with two explicit execution modes:

- `ProcessToolBuilder` starts a bounded direct child for trusted executables;
- `BubblewrapProcessToolBuilder` starts a deny-network Linux sandbox with a prepared read-only root filesystem and one writable scratch directory.

The Bubblewrap path is an isolation foundation, not complete operating-system Capability enforcement.

## Trust model

Trusted startup code configures one `ProcessToolHandler` with:

- one absolute executable path;
- one absolute working directory;
- optional fixed arguments;
- optional fixed environment entries;
- a bounded timeout;
- a mandatory policy for dynamic arguments.

The builder canonicalizes the executable and working directory. On Unix, it records the executable device and inode and verifies them again immediately before spawn. This narrows accidental executable replacement but does not detect in-place modification or eliminate the check-to-execution race. Descriptor-bound execution remains future work.

The handler is intended to be consumed by `ToolAdapterBuilder`. The resulting `ToolExecutionGate` applies Task Capability and approval checks before invoking it. Direct possession of a handler is trusted in-process authority and must not be exposed to a model or runner.

The Linux isolated builder additionally requires:

- an absolute Bubblewrap executable path;
- a prepared root filesystem that is not the host root;
- an absolute executable path inside that root filesystem;
- a separate scratch directory mounted at `/workspace`.

The prepared root must contain directory mount points for `/proc`, `/dev`, `/tmp`, and `/workspace`. The root and scratch trees must not overlap. Trusted startup code is responsible for creating a minimal, versioned root filesystem and an empty Task-scoped scratch directory. Neither tree may contain daemon or approval sockets, event databases, host credentials, or unrelated user data.

## Execution behavior

1. The Tool Catalog maps a model-visible route to fixed Capability Tool and action identifiers.
2. `ExecutionGate` authorizes and retains the complete Tool operation.
3. The Process Adapter revalidates total argument bounds.
4. The trusted argument policy evaluates the dynamic argument vector.
5. The adapter verifies the configured executable identity.
6. The direct mode starts the executable with fixed and dynamic argument arrays. The isolated mode starts the fixed Bubblewrap executable with a deterministic sandbox plan, followed by the exact executable and argument array.
7. The child receives an otherwise empty environment and null standard streams.
8. The adapter waits for successful exit or kills and reaps its direct child after the configured timeout.

No step invokes a shell, interprets argument text, or searches `PATH` for the executable. Dynamic arguments such as shell metacharacters remain literal strings.

The handler timeout is fixed trusted configuration. It does not yet derive from or enforce the Task wall-time budget.

## Linux Bubblewrap boundary

The isolated launch plan always:

- requires a user namespace through `--unshare-user`, then creates the remaining mount, PID, IPC, network, UTS, and cgroup namespaces through `--unshare-all`;
- disables further user namespace creation and drops all capabilities;
- mounts the prepared root read-only at `/`;
- mounts only the declared scratch directory read-write at `/workspace`;
- creates private `/proc`, `/dev`, and in-memory `/tmp` mounts;
- creates a new terminal session and requests child termination when the launcher or its parent dies;
- preserves no additional file descriptors and never adds `--share-net`.

Namespace or mount setup failure is an execution failure. The adapter never falls back to direct execution. The builder returns `UnsupportedPlatform` outside Linux.

## Linux boundary verification

The `linux_bubblewrap` integration suite starts the real Bubblewrap executable with a static BusyBox probe. It verifies that:

- `/workspace` writes reach only the declared scratch directory;
- writes through the read-only root filesystem fail;
- a host-only approval socket path is absent inside the sandbox;
- a non-standard host file descriptor is closed before Tool execution;
- a host TCP listener reachable by the same BusyBox executable in direct mode is unreachable from the sandbox network namespace.

These tests are ignored by the default test command because they require Linux, Bubblewrap 0.8.0 or newer with `--disable-userns`, static BusyBox, and enabled unprivileged user namespaces. The pinned Ubuntu 24.04 workflow verifies the required Bubblewrap option before running them explicitly:

```bash
AIOS_BWRAP_PATH=/usr/bin/bwrap \
AIOS_BUSYBOX_PATH=/usr/bin/busybox \
cargo test -p aios-adapter-process --test linux_bubblewrap --locked -- --ignored
```

Passing this suite is evidence for the current deny-network launch boundary only. It does not verify Task-derived mounts, cgroup budgets, seccomp, destination-scoped networking, or the future approval API.

## Bounds

| Resource | Default or maximum |
| --- | ---: |
| Timeout | 30 seconds by default, 1 hour maximum |
| Fixed and dynamic arguments | 64 total |
| Bytes per argument | 4,096 |
| Total argument bytes | 65,536 |
| Fixed environment entries | 64 |
| Environment name | 128 bytes |
| Environment value | 4,096 bytes |
| Total environment bytes | 65,536 |

Argument and environment values containing NUL are rejected. Environment names use portable ASCII identifier syntax. Duplicate names are rejected. Errors expose stable categories without executable, directory, argument, environment, or exit details.

Fixed environment entries are not a secret-delivery mechanism. Credentials remain prohibited until a scoped, revocable Secret Capability exists.

## Output policy

Standard input, standard output, and standard error are connected to null. A successful handler returns an empty bounded `ToolOutput`.

Output capture is deliberately deferred. A bounded pipe alone is insufficient because a descendant can retain the pipe after the direct child exits or is killed. Bounded streaming must be introduced together with reliable descendant-process containment and cleanup.

## Residual risks and prohibited claims

The direct mode does not:

- run under a separate OS principal;
- guarantee that non-standard inherited descriptors are closed;
- restrict filesystem, network, IPC, device, or credential access;
- create user, mount, PID, or network namespaces;
- enforce CPU or memory budgets through cgroups;
- terminate descendants that leave the direct child's process lifecycle;
- bind execution to an already-open executable descriptor;
- provide resumable or asynchronous cancellation.

The Bubblewrap mode narrows filesystem visibility, denies host network access, closes unpreserved descriptors in the launcher, and supplies namespace process containment. It still does not:

- derive root or scratch mounts from the current Task's Filesystem Capability;
- provide approved destination-scoped network access;
- enforce CPU or memory budgets through cgroups;
- install a seccomp policy or descriptor-bound file access;
- verify an immutable root filesystem image;
- eliminate host-side executable and mount time-of-check/time-of-use races;
- prove complete descendant cleanup with adversarial Linux integration tests;
- expose a principal-separated approval API.

Executables registered in direct mode remain trusted. Argument policies in both modes must allow only the exact semantic operations intended for the Tool route. Bubblewrap mode may be described as experimental deny-network process isolation, but not as complete Capability enforcement or a release-ready sandbox until the corresponding [Threat Model](threat-model.md) gates have Linux evidence.

## Next enforcement milestone

Next, extend the Linux suite with adversarial descendant cleanup, then integrate this backend with Task-derived scratch creation and a minimal immutable root image. After that, cgroup budgets, seccomp, and destination-scoped network brokering can extend the same boundary. See [ADR-0006](adr/0006-bubblewrap-process-isolation.md).
