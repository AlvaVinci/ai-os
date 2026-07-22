# ADR-0004: Fail interrupted Tasks instead of restoring execution authority

## Status

Accepted

## Context

The SQLite Event Store persists resource-free audit Events and can reconstruct public Task state. It deliberately does not persist goals, Capabilities, model sessions, complete Tool operations, approval grants, or idempotency input. Reconstructing a `running` or `waiting_approval` state without those values would imply that work could continue even though its exact authority and execution context no longer exist.

AI OS v0.1 requires a deterministic non-resumable restart contract. A client must be able to distinguish an interrupted Task from a live Task, and the daemon must not accept new work if it cannot durably record that distinction.

## Decision

Before accepting requests, `aiosd` will:

1. bind the configured control socket to exclude another daemon using that socket path;
2. validate and reduce SQLite Events to public Task snapshots;
3. leave terminal Tasks unchanged;
4. append one atomic `task_failed` Event with code `RUNTIME_RESTARTED` and one transition to `failed` for every non-terminal Task;
5. abort startup and remove its exact socket if recovery, validation, capacity, or audit persistence fails.

Recovery reconstructs no Task input, model state, Tool operation, approval request, grant, or other execution authority. Repeating startup after a completed recovery is idempotent because all interrupted Tasks are already terminal.

The public Event schema changes in Protocol Version 4. Version 3 clients are rejected explicitly rather than receiving an Event variant they do not understand.

Process-local idempotency keys are not restored. A submission after restart is an explicit new Task with a new Task ID, even when the caller reuses a previous idempotency key. The old Task remains inspectable and is never executed again.

## Consequences

- Public Task state and Events remain inspectable across restart without persisting sensitive Task input.
- Interrupted work cannot silently resume or retain stale approval authority.
- A full Event partition or corrupt sequence can prevent daemon startup until the operator resolves the storage condition.
- Recovered Tasks count toward the configured Task capacity until a separate archival policy exists.
- Operators must not share one database between daemons configured with different socket paths; cross-socket database ownership is not enforced yet.
- Resumable execution remains unavailable in v0.1.

## Alternatives considered

### Restore running state without execution context

Rejected. It presents stale work as active and cannot reproduce the original Capability, operation, or model-session boundary.

### Resume from plaintext Task input and Tool arguments

Rejected. It violates the privacy boundary and still lacks encrypted retention, replay protection, approval freshness, and key-management semantics.

### Delete interrupted Tasks

Rejected. It removes audit evidence and prevents clients from learning the terminal outcome.

### Record only an in-memory failure

Rejected. The result would disappear on the next restart and would not be ordered durably before the daemon accepts work.

## Related requirements

- DOD-006: persistence and restart contract
- MVP FR-019: explicit non-resumable outcome
- Threat Model TM-013: restart authority confusion
