# Process Adapter

## Status

Experimental bounded child-process Tool handler. It reduces ambient process behavior but is not an operating-system sandbox or complete Capability enforcement.

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

## Execution behavior

1. The Tool Catalog maps a model-visible route to fixed Capability Tool and action identifiers.
2. `ExecutionGate` authorizes and retains the complete Tool operation.
3. The Process Adapter revalidates total argument bounds.
4. The trusted argument policy evaluates the dynamic argument vector.
5. The adapter verifies the configured executable identity.
6. The adapter starts the executable directly with fixed and dynamic argument arrays.
7. The child receives a canonical working directory, an otherwise empty environment, and null standard streams.
8. The adapter waits for successful exit or kills and reaps the direct child after the configured timeout.

No step invokes a shell, interprets argument text, or searches `PATH` for the executable. Dynamic arguments such as shell metacharacters remain literal strings.

The handler timeout is fixed trusted configuration. It does not yet derive from or enforce the Task wall-time budget.

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

The current adapter does not:

- run under a separate OS principal;
- guarantee that non-standard inherited descriptors are closed;
- restrict filesystem, network, IPC, device, or credential access;
- create user, mount, PID, or network namespaces;
- enforce CPU or memory budgets through cgroups;
- terminate descendants that leave the direct child's process lifecycle;
- bind execution to an already-open executable descriptor;
- provide resumable or asynchronous cancellation.

Executables registered with this adapter remain trusted. Argument policies must allow only the exact semantic operations intended for the Tool route. The adapter must not be described as isolated, sandboxed, or safe for untrusted executables until the corresponding [Threat Model](threat-model.md) release gates are implemented.

## Next enforcement milestone

The Linux isolation adapter should combine principal separation, a descriptor allowlist, namespace or equivalent isolation, cgroup resource limits, descendant cleanup, and descriptor-bound filesystem access. That work must preserve the fixed executable and structured argument rules from [ADR-0002](adr/0002-no-shell-tool-execution.md).
