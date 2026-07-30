# ADR-0016: Bind resource adapters to the original Task wall-time

- Status: Accepted
- Date: 2026-07-31
- Related threats: TM-011, TM-014, TM-015, TM-016
- Related objectives: SEC-003, SEC-005, SEC-007, SEC-009

## Context

The Filesystem and Network Adapters already retain complete operations across approval, but their first implementations did not retain the Task execution context. An approval wait could therefore consume the Task wall-time Budget while the approved operation still created a file or opened a connection. The Network Adapter also used only its trusted configured timeout, which could outlive the Task deadline.

A new timeout starting after approval would widen execution authority beyond the Budget accepted with the Task. Resource adapters must instead use the same immutable `started_at` and validated Budget as the rest of that Task.

## Decision

- `FilesystemExecutionGate` and `NetworkExecutionGate` obtain `TaskExecutionContext` from the running Task before authorization and retain it privately inside the complete operation.
- The retained context crosses approval unchanged. Approval cannot restart, extend, or replace the Task wall-time.
- The Filesystem Adapter checks the remaining Task wall-time before opening or creating a destination, before writing, and after data synchronization.
- The Network Adapter checks the remaining Task wall-time before each blocking stage. Connect, write, and read use the smaller of the trusted adapter timeout and the remaining Task wall-time.
- Expiration returns a stable typed `BudgetExceeded` adapter error. The orchestration caller remains responsible for atomically recording the Task's existing `BUDGET_EXCEEDED` terminal failure.
- The retained context remains non-debuggable and non-serializable and is never supplied by model output or an untrusted caller.

## Consequences

- Approval latency consumes the original Task Budget. An operation approved after expiration cannot create a file or initiate a TCP connection.
- A stalled TCP peer cannot extend a synchronous exchange beyond the remaining Task wall-time except for operating-system scheduling granularity.
- Local filesystem write and synchronization calls are synchronous and cannot be preempted safely by this adapter. Expiration during one of those calls is detected afterward, and a partial or complete new file may remain.
- A remote peer can receive a partial or complete request before a later deadline or I/O failure. The adapter cannot roll back remote effects.
- Resource adapters return a typed budget category but do not independently transition Task state. Agent or daemon integration must preserve the existing audit-first terminal transition.
- This does not add disk quotas, connection-rate limits, asynchronous cancellation, Agent routing, or a general resource scheduler.

## Verification

- Filesystem Linux tests hold an operation for approval beyond the Task deadline and verify that no destination is created.
- Network tests hold an operation for approval beyond the Task deadline and verify that no connection is opened.
- Network tests verify that the Task wall-time, rather than a longer adapter timeout, bounds a stalled in-flight response.
- Existing adapter authorization, operation-retention, size-bound, and destination-bound tests continue to pass.

## Alternatives considered

### Start a fresh timeout after approval

Rejected. This would make approval a Budget reset and allow execution after the Task's accepted wall-time.

### Use only the adapter-configured socket timeout

Rejected. A trusted per-operation bound is still unsafe when it exceeds the Task's remaining wall-time.

### Move synchronous filesystem I/O to a detached timeout thread

Rejected for this increment. Returning on timeout would not stop the detached write and could hide continuing side effects. Preemptible filesystem execution requires a cancellable worker boundary with explicit cleanup and recovery semantics.
