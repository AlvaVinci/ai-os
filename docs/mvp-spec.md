# MVP Specification

## Status

Draft 0.2. Numeric limits and implementation technologies may change through validation.

## 1. Background

Local AI agent applications often implement model access, tools, permissions, budgets, approvals, and audit behavior independently. The MVP provides a common runtime for those concerns on one Linux device.

## 2. Goals and non-goals

### Goals

- Accept structured tasks through a local API.
- Restrict file, network, and tool capabilities per task.
- Inspect agent, model, lifecycle, and resource state.
- Pause high-impact operations for human approval.
- Audit important state transitions and operations.

### Non-goals

- GUI, voice interface, or consumer installer
- distributed execution across devices
- a custom kernel or device drivers
- support for every model runtime
- autonomous capability escalation

## 3. Terms

- **Task**: an execution unit containing a goal, constraints, capabilities, and budgets
- **Agent**: an actor that plans work and requests permitted tools
- **Capability**: permission for a specific operation on a resource
- **Budget**: the maximum resources available to a task
- **Approval**: a time-bound user decision for one high-impact operation
- **Event**: an append-only task state or audit record

## 4. Requirements

### Functional requirements

- **FR-001**: The runtime accepts Tasks encoded as JSON.
- **FR-002**: The runtime detects duplicate Task IDs and idempotency keys.
- **FR-003**: Unspecified capabilities are denied.
- **FR-004**: Filesystem access is evaluated using normalized paths and operation types.
- **FR-005**: Network access is denied by default.
- **FR-006**: High-impact operations enter an approval-waiting state.
- **FR-007**: Task cancellation terminates child processes.
- **FR-008**: Wall-time and memory limits are enforced.
- **FR-009**: State transitions and policy decisions are stored as Events.
- **FR-010**: Model runtimes can be replaced through a common adapter.
- **FR-011**: The local API accepts bounded requests only through an owner-only Unix socket.
- **FR-012**: Every local API request declares its protocol version, and unsupported versions are rejected explicitly.
- **FR-013**: Capability decisions are deterministic, fail closed, and evaluate granted capability before approval requirements.
- **FR-014**: Approval grants are task-, operation-, and action-scoped, time-limited, and consumable only once.

### Task input example

```json
{
  "idempotency_key": "repo-analysis-2026-07-19-001",
  "goal": "Analyze the repository and report the cause of failing tests",
  "capabilities": {
    "filesystem": [
      { "path": "/workspace/project", "access": "read" }
    ],
    "network": {
      "mode": "deny"
    },
    "tools": ["test_runner"]
  },
  "budget": {
    "wall_time_seconds": 1800,
    "memory_bytes": 8589934592,
    "max_parallel_agents": 2
  },
  "approval": {
    "required_for": ["filesystem.write", "git.commit", "network.egress"]
  }
}
```

### Minimum output fields

```json
{
  "task_id": "019...",
  "state": "succeeded",
  "result": {
    "summary": "Identified the cause and documented reproduction steps",
    "artifacts": []
  },
  "usage": {
    "wall_time_seconds": 124,
    "peak_memory_bytes": 1207959552
  },
  "event_cursor": 42
}
```

## 5. States and transitions

```text
submitted -> validating -> queued -> running -> succeeded
                |            |         |  |
                v            |         |  +-> waiting_approval -> running
              rejected       |         +----> failed
                             +--------------> cancelled
```

- Terminal states are `succeeded`, `failed`, `cancelled`, and `rejected`.
- Terminal tasks never return to an execution state.
- A retry creates a new Task ID and references the original Task ID.
- An approval identifies the operation, resource, and expiration.
- Approval and Operation IDs are identifiers, not bearer secrets.
- A scope mismatch, expiration, approval, or denial consumes or removes the corresponding authorization object.

## 6. Boundaries and errors

- A blank goal or a goal longer than 8,192 Unicode scalar values is rejected.
- `null` for a required field is a validation error and is not silently defaulted.
- Repeating an idempotency key with identical input returns the existing Task.
- Reusing an idempotency key with different input returns a conflict.
- Duplicate tools, approval actions, and network hosts are validation errors.
- `max_parallel_agents` is between 1 and 8 for the MVP.
- Zero budgets, integer overflow, and unknown units are rejected.
- When a budget is reached, new tool calls stop and the Task fails.
- Partial artifacts may be referenced by a failed result but are never published automatically.
- Cancellation is idempotent; cancelling a terminal Task returns its current state.

### Error codes

- `INVALID_TASK`: invalid structure or boundary value
- `IDEMPOTENCY_CONFLICT`: different input reused an idempotency key
- `CAPABILITY_DENIED`: a required capability was not granted
- `APPROVAL_EXPIRED`: a requested approval expired
- `BUDGET_EXCEEDED`: a resource limit was reached
- `RUNTIME_UNAVAILABLE`: a requested model or tool is unavailable
- `INTERNAL_ERROR`: an internal failure with a non-sensitive diagnostic ID

## 7. Non-functional requirements

### Security and privacy

- External network access is denied by default.
- Allowed network destinations are explicit lowercase host names or IP addresses without schemes, paths, or ports.
- Filesystem scopes match only the normalized path itself or a descendant separated by `/`; string-prefix siblings do not match.
- Read and write capabilities are independent and do not imply each other.
- Tool names, network hosts, and approval action identifiers use exact matching.
- Invalid operation requests and missing capabilities are denied with stable, resource-free reason codes.
- Pending approvals are bounded, expire against a monotonic process-local clock, and reject duplicate Task and Operation pairs.
- Linear approval grants cannot be cloned, debugged, or serialized through safe Rust APIs.
- Model output cannot modify capabilities.
- Secrets are not stored in prompts, events, or user-facing errors in plaintext.
- Context is not shared implicitly between Tasks.
- Policy evaluation fails closed.
- A failed Event append does not apply the associated state change.

### Reliability

- Events and their derived public Task state survive a runtime restart.
- Resumable Task input remains separate from audit persistence and requires an encrypted storage design.
- Event sequence numbers increase monotonically per Task.
- Tool retries are limited to operations declared idempotent.
- Multi-event submission records are appended atomically.

### Performance and availability

- Policy evaluation does not require model inference.
- Keeping idle models loaded is configurable.
- In-memory collections have explicit per-Task or per-runtime upper bounds.
- Local API frames are bounded and each connection has read and write timeouts.
- MVP request handling is sequential, preventing unbounded connection concurrency.
- Performance targets will be set after baseline measurement.

### Compatibility

- The first supported target is Linux x86_64.
- A slower degraded mode works without a hardware accelerator.

## 8. Acceptance criteria

1. Given a Task without network permission, when an agent requests external access, then the connection is denied and an Event is recorded.
2. Given a read-only directory, when an agent requests a write, then the operation is not executed and returns `CAPABILITY_DENIED`.
3. Given a memory-limited Task, when usage reaches the limit, then child processes stop and the Task returns `BUDGET_EXCEEDED`.
4. Given a commit that requires approval, when an agent requests it, then the Task enters `waiting_approval` and no commit occurs first.
5. Given identical input and an idempotency key, when submitted concurrently twice, then only one Task executes.
6. Given a running Task with persisted Events, when the runtime restarts, then its recovery state and existing Events can be inspected without loading goal or capability values.
7. Given two model adapters, when configuration switches between them, then the Task API format does not change.
8. Given Event storage failure, when a state change is requested, then the Task state remains unchanged.

## 9. Metrics, logs, and monitoring

- Task counts by state
- queue, execution, and approval wait time
- CPU time, peak RAM, and GPU/VRAM usage when available
- policy denial counts by reason code
- model and tool failures
- budget exhaustion and forced termination

Logs correlate through Task and Event IDs. Goal text, secrets, complete model responses, and private reasoning are excluded from default operational logs.

## 10. Compatibility and migration

MVP persistence formats are not stable APIs. A format change increments the schema version and provides either migration from the immediately previous version or an explicit reset procedure.

## 11. Alternatives and trade-offs

- **Start with a custom kernel**: offers control but makes drivers, isolation, and distribution dominate MVP work.
- **Use containers only**: helps isolation but does not define shared Task, Capability, Approval, and Model semantics.
- **Implement everything in Python**: accelerates experiments but is not the preferred boundary for long-running authorization enforcement.
- **Assume cloud execution**: provides larger models but conflicts with local-first privacy goals.
- **Write Task IDs without a standard identifier dependency**: reduces dependencies but increases collision and interoperability risk; UUIDv7 is used instead.

## 12. Open questions

- first supported Linux distribution
- first local model runtime
- policy language and capability granularity
- Event and Context encryption
- consistent GPU and VRAM limits across vendors
- approval user experience for destructive operations
- persistent Event Store technology and tamper-evidence model
