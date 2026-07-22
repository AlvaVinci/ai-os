# Architecture

## Status

Draft. The architecture will evolve from MVP implementation and measurement.

The repository currently contains seven crates:

- `aios-agent`: bounded model-session contracts and synchronous approval-aware Agent execution
- `aios-adapter-process`: bounded child-process Tool handler with explicit executable configuration
- `aios-adapter-tool`: bounded catalog and in-process handler execution behind `ExecutionGate`
- `aios-core`: Task contracts, validation, stable error codes, and lifecycle states
- `aios-local-api`: bounded Unix-socket protocol and the experimental `aiosd` daemon
- `aios-runtime`: synchronous task supervision and a bounded in-memory event store
- `aios-storage-sqlite`: persistent audit events and event-derived Task state recovery

The Tool Adapter can invoke explicitly registered in-process handlers, including the bounded child-process handler. The Process Adapter starts only a trusted, fixed executable with validated arguments and a cleared environment. It is not an operating-system sandbox, and no crate provides complete process isolation yet. The remaining crates define the trust boundary that future execution components must satisfy.

## System overview

```text
+---------------- CLI / Local API ----------------+
| Submit, inspect, and cancel tasks                |
+-------------------------+------------------------+
                          |
+---------------- Task Supervisor ----------------+
| State machine, agents, retry, and dependencies   |
+-------------------------+------------------------+
                          |
+------------ Policy & Capability Engine ----------+
| Authorization, approvals, secrets, and audit     |
+-------------------------+------------------------+
                          |
+--------- Model Router & Resource Scheduler -------+
| Model choice, placement, CPU/GPU/RAM/time budget |
+-------------------------+------------------------+
                          |
+-------------- Runtime Adapters ------------------+
| Models, tools, context, and events               |
+-------------------------+------------------------+
                          |
+--------------------- Linux ----------------------+
| Processes, cgroups, namespaces, files, devices   |
+--------------------------------------------------+
```

## Responsibilities

### Task Supervisor

- Validate tasks and assign unique identifiers.
- Manage task state through a deterministic state machine.
- Prevent duplicate execution through idempotency keys.
- Coordinate future agent startup, cancellation, timeout, and retry behavior.
- Record an event before applying each accepted state change.

The current implementation is synchronous and process-local. It limits the number of retained Tasks, batches submission events atomically, and leaves state unchanged when event storage fails. Existing idempotent submissions remain retrievable when capacity is full.

### Local API

- Listens only on an owner-only Unix domain socket.
- Uses a four-byte length prefix and bounded JSON payloads.
- Handles one request per connection with read and write timeouts.
- Requires an explicit protocol version and rejects unsupported versions.
- Establishes the configured socket-path exclusion, then recovers audit-safe public Task state before accepting requests.
- Processes connections sequentially to keep concurrency bounded in the MVP.
- Refuses to replace an existing socket path and removes only the exact socket inode it created.
- Returns stable error categories without internal I/O or storage details.

`aiosctl` uses the same Protocol Version 4 types as the daemon for submission, inspection, event retrieval, and lifecycle transitions. Version 4 adds the audit-safe `task_failed` Event carrying a stable failure code. Version 3 replaced host-only network allowlists with exact TCP destinations and retained Version 2's removal of the unsafe Task-ID-only approval methods from Version 1.

Version 4 is the first stable Local API schema contract. The daemon publishes its inclusive supported-version window through `health`; the current window is `4..=4`. Requests remain strict, while clients ignore additive fields on existing response objects. Incompatible request, tagged variant, error-code, limit, framing, or security-semantic changes require a new protocol version and the overlap policy in [API compatibility](api-compatibility.md).

After binding the configured control socket but before accepting requests, `aiosd` reconstructs public Task snapshots from SQLite. Every previously non-terminal Task receives an atomic `RUNTIME_RESTARTED` failure Event and transition to `failed`; terminal Tasks remain unchanged, and repeated restarts do not append duplicate failure Events. Any recovery or audit write failure aborts startup and drops the exact socket it created. Goals, Capabilities, idempotency keys, model sessions, Tool operations, and approval authority are not reconstructed. A post-restart submission is always a new explicit Task, even if its idempotency key was used by a previous daemon process. Sharing one database across different socket paths is not supported or prevented yet.

### Policy & Capability Engine

- Evaluate access to files, networks, tools, and secrets.
- Return allow, deny, or human-approval-required decisions.
- Enforce constraints independently from prompts and model output.
- Record denial and approval reason codes without sensitive input values.

The current `CapabilityPolicy` is a pure, deterministic pre-execution evaluator. It can only be created from a valid Task. It returns `allow`, `deny`, or `approval_required` without including requested resource values in the decision. Capability checks always run before approval checks, so an approval rule cannot grant a missing capability.

This policy layer does not yet enforce operating-system access. Runtime adapters must call it before every operation and separately prevent path races, symlink escapes, DNS rebinding, inherited file descriptors, and subprocess bypasses. See [Capability policy](capability-policy.md) for the exact MVP semantics and enforcement boundary.

### Approval Authority

- Bounds pending approval count and lifetime.
- Binds each request to an exact Task ID, opaque Operation ID, and action identifier.
- Rejects duplicate pending requests for the same Task and Operation.
- Removes a request on approval, denial, or expiration.
- Returns a linear grant that cannot be cloned, debugged, or serialized.
- Consumes the grant on every authorization attempt, including scope mismatch and expiration.

The current supervisor integrates the process-local `ApprovalAuthority` with capability evaluation, exact in-memory capability binding, Task state, and resource-free audit events. `ExecutionGate` additionally retains the complete typed operation and invokes its private adapter only after allow or successful grant consumption. Approval IDs are public identifiers, not bearer secrets. Cancellation and terminal transitions invalidate unused authority, while audit persistence failures leave state unchanged or fail closed. Restart recovery deliberately discards all live grants and operations; restoring them is outside the v0.1 non-resumable contract. Local API exposure for a principal-separated approval workflow and concrete operating-system adapters remain future work. See [Approval grants](approval-grants.md).

### Execution Gate

- Owns a raw adapter without exposing a bypass reference.
- Accepts complete typed operations from trusted adapter integration code.
- Retains complete operation arguments privately while approval is pending.
- Drops denied, cancelled, expired, mismatched, and already-attempted operations.
- Invokes the adapter only after the runtime records allow or consumes an approval grant.
- Redacts adapter errors at the shared execution boundary.

The gate is an in-process authorization boundary, not an operating-system sandbox. Concrete adapters must prevent direct subprocess, file-descriptor, network, and device access outside the gate. Model and tool processes must not receive the daemon control socket or raw adapter handles.

### Agent Runtime

- Starts one Task-scoped model session from a validated goal and only the Tool routes granted to that Task.
- Accepts only bounded final-output or Tool-call decisions.
- Reconstructs every Tool operation through the trusted catalog before Capability evaluation.
- Bounds model turns and retains at most one active model session per runtime instance.
- Retains a waiting session across approval and resumes it only after the exact retained operation executes.
- Fails the Task on invalid decisions, unknown routes, model failures, Tool failures, and step exhaustion.

The current `aios-agent` crate supplies a deterministic scripted Model Adapter for conformance tests. It does not perform inference and does not satisfy the real local model requirement. Model requests, decisions, outputs, and Task execution input omit debug and serialization implementations because they may contain sensitive values. See [Agent runtime and Model Adapter contract](agent-runtime.md).

### Tool Adapter

- Maps model-visible routes to capability tool and action identifiers fixed by trusted registration.
- Encapsulates raw handlers and `ExecutionGate` behind the public `ToolExecutionGate` facade.
- Limits route count, identifiers, argument count, argument bytes, and output bytes.
- Passes arguments as a vector without shell interpolation, `PATH` lookup, or process creation.
- Revalidates catalog scope immediately before invoking a handler.
- Returns stable errors without route, argument, handler, or output values.

The current adapter executes trusted in-process handlers only. Handler-specific validation, timeouts, partial-side-effect idempotency, and sensitive output handling remain handler responsibilities. See [Tool adapter](tool-adapter.md).

### Process Adapter

- Executes one canonical absolute executable configured by trusted startup code.
- Requires a trusted policy to approve every dynamic argument vector.
- Bounds fixed and dynamic arguments and fixed environment values.
- Clears ambient environment variables and nulls standard input, output, and error.
- Uses an explicit canonical working directory and never invokes a shell or searches `PATH`.
- Applies a bounded timeout to the direct child and returns redacted failure categories.
- Optionally launches through an explicit Linux Bubblewrap backend with a prepared read-only root filesystem, one writable scratch mount, namespace isolation, no host network, dropped capabilities, and no preserved extra descriptors.

Direct mode remains a constrained `ToolHandler`, not a sandbox. Bubblewrap mode is an experimental deny-network isolation foundation, but its mounts are not yet derived from Task Capabilities and it does not apply cgroup budgets, seccomp, descriptor-bound filesystem access, or Linux-tested asynchronous cancellation. Process output is deliberately discarded until bounded streaming and descendant cleanup can be enforced together. See [Process adapter](process-adapter.md) and [ADR-0006](adr/0006-bubblewrap-process-isolation.md).

### Model Router

- Select models based on task requirements, privacy, latency, and available resources.
- Prefer local execution by default.
- Use external models only after explicit permission and data-scope evaluation.
- Adapt model-specific input and output to common contracts.

The initial Task-scoped `ModelAdapter` and `ModelSession` contracts live in `aios-agent`. A future local inference adapter must parse backend output into bounded `ModelDecision` values, apply inference deadlines, identify model artifacts, and prevent context from crossing Task sessions.

### Resource Scheduler

- Track per-task CPU, GPU, RAM, VRAM, and time limits.
- Manage priority and concurrency.
- Stop new work when a limit is reached, then fail or await approval as defined by policy.
- Later incorporate power, temperature, and external API cost.

### Context Store

- Preserve context provenance, ownership, creation time, and expiration.
- Control reads by sensitivity and task boundary.
- Never share long-term memory implicitly between tasks.
- Keep deletion and retention semantics separate from immutable audit metadata.

### Event Store

- Append task lifecycle, policy, approval, tool, and resource events.
- Exclude goals, capability values, secrets, and private model reasoning from default audit payloads.
- Avoid automatic debug formatting for input types that may contain sensitive values.
- Use structured reason codes and minimal explanations.
- Bound resource usage and fail atomically when a batch cannot be stored.

The `InMemoryEventStore` assigns a monotonically increasing sequence per Task and enforces a configurable event limit. `SqliteEventStore` adds transactional batches, schema versioning, owner-only file creation on Unix, corrupt or incomplete failure-sequence detection, and restart-safe recovery of public Task state. The Supervisor records the stable `RUNTIME_RESTARTED` category and terminal transition before a recovered Task becomes visible through the daemon.

The SQLite store deliberately does not persist Task goals or capability values. Resuming execution after a restart requires a separate encrypted Task-input design. Tamper evidence is also future work.

## Trust boundaries

The normative security objectives, actors, data flows, threat register, residual risks, and release gates are maintained in the [Threat model](threat-model.md). Capability authority, lifecycle, resource semantics, and enforcement status are defined in the [Capability model](capability-model.md).

Model output, retrieved documents, external tool output, and instructions written by anyone other than the active user are untrusted input. Instructions inside that data cannot modify task capabilities or policy.

High-impact operations include:

- transmitting data outside the approved boundary
- deleting files or history
- committing, publishing, or deploying changes
- using credentials or personal information
- changing system settings or persistent permissions

## Implementation direction

- Core domain and policy code: Rust
- Long-running daemon: a Rust service that accepts only validated `aios-core` types
- Low-level integration: C ABI or standard OS interfaces where required
- Model execution: existing local inference runtimes behind adapters
- Experimental model integrations: Python is allowed, but never as the capability enforcement layer
- Audit persistence: bundled SQLite with versioned schemas
- Sensitive Task persistence: deferred until encryption, retention, and deletion semantics are defined

Technology choices that affect public contracts are documented as [Architecture Decision Records](adr/README.md).

## Compatibility

The first target is Linux. Existing applications integrate through processes, standard streams, files, local sockets, and containers rather than a proprietary application format. macOS and Windows ports may follow after the core API stabilizes.

Protocol Version 4 is stable as a JSON schema contract, independently from Rust source compatibility and SQLite schema evolution. See [Local API Compatibility Contract](api-compatibility.md) and [ADR-0005](adr/0005-stable-local-api.md).
