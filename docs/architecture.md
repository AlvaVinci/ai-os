# Architecture

## Status

Draft. The architecture will evolve from MVP implementation and measurement.

The repository currently contains four crates:

- `aios-core`: Task contracts, validation, stable error codes, and lifecycle states
- `aios-local-api`: bounded Unix-socket protocol and the experimental `aiosd` daemon
- `aios-runtime`: synchronous task supervision and a bounded in-memory event store
- `aios-storage-sqlite`: persistent audit events and event-derived Task state recovery

None of these crates executes models, tools, or operating-system operations. They define the trust boundary that future execution components must satisfy.

## System overview

```text
+---------------- CLI / Local API ----------------+
| Submit, inspect, approve, and cancel tasks       |
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
- Processes connections sequentially to keep concurrency bounded in the MVP.
- Refuses to replace an existing socket path and removes only the exact socket inode it created.
- Returns stable error categories without internal I/O or storage details.

`aiosctl` uses the same Protocol Version 1 types as the daemon for submission, inspection, event retrieval, and lifecycle transitions. The daemon currently loses Task input and in-memory idempotency state on restart. Persisted audit events remain available through the SQLite layer.

### Policy & Capability Engine

- Evaluate access to files, networks, tools, and secrets.
- Return allow, deny, or human-approval-required decisions.
- Enforce constraints independently from prompts and model output.
- Record denial and approval reason codes without sensitive input values.

### Model Router

- Select models based on task requirements, privacy, latency, and available resources.
- Prefer local execution by default.
- Use external models only after explicit permission and data-scope evaluation.
- Adapt model-specific input and output to common contracts.

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

The `InMemoryEventStore` assigns a monotonically increasing sequence per Task and enforces a configurable event limit. `SqliteEventStore` adds transactional batches, schema versioning, owner-only file creation on Unix, corrupt-sequence detection, and restart-safe recovery of public Task state.

The SQLite store deliberately does not persist Task goals or capability values. Resuming execution after a restart requires a separate encrypted Task-input design. Tamper evidence is also future work.

## Trust boundaries

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

Technology choices that affect public contracts will be documented as architecture decision records.

## Compatibility

The first target is Linux. Existing applications integrate through processes, standard streams, files, local sockets, and containers rather than a proprietary application format. macOS and Windows ports may follow after the core API stabilizes.
