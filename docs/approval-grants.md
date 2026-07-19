# Approval Grants

## Status

Experimental process-local workflow for one-time human approval. It is connected to capability policy evaluation, Task lifecycle, and the Event Store through `TaskSupervisor`. It is not yet exposed through the local API or connected to an execution adapter.

## Lifecycle

1. A trusted runtime caller submits one structured capability request for a running Task.
2. The supervisor evaluates the request and creates a unique Operation ID only when approval is required.
3. The supervisor stores the exact operation in process memory and records `approval_requested` with the transition to `waiting_approval` in one event batch.
4. A trusted user-facing component approves or denies the pending request.
5. Approval records `approval_granted`, resumes the Task, and stores the linear grant inside the supervisor. Denial records `approval_denied` and fails the Task.
6. Immediately before execution, the adapter presents the same structured request. The supervisor compares every resource field, re-evaluates policy, consumes the grant, and records `approval_consumed` before returning the receipt.

Approval and Operation IDs are public identifiers. They are not bearer secrets and do not authorize an operation by themselves.

## Scope and one-time use

Each pending request is bound to:

- one Task ID;
- one opaque Operation ID;
- one validated action identifier;
- one monotonic deadline.

`ApprovalGrant` does not implement `Clone`, `Debug`, or serialization. Its `authorize` method takes ownership of the grant, so every attempt consumes it. A wrong Task, Operation, or action fails with `ScopeMismatch`; an expired grant fails with `Expired`. Neither failure returns the grant.

The supervisor binds the Operation ID to one exact structured operation. Model output must not select or reuse an Operation ID, and changing any operation argument consumes the grant with `ScopeMismatch`; a new operation and approval are required.

## Bounds and expiration

The default authority permits at most 1,024 pending approvals and a maximum lifetime of 15 minutes. Configuration cannot exceed 65,536 pending approvals or a 24-hour maximum lifetime. Individual request lifetimes must be between 1 millisecond and the configured maximum.

The supervisor detects expired requests before request, approval, and denial operations, records `approval_expired`, and fails the waiting Task. Approval and denial both remove the request. Only one pending approval may exist for a Task at a time because the current Task state machine has one `waiting_approval` state.

Deadlines use `std::time::Instant`, so they are monotonic but process-local. Pending requests and grants do not survive a restart.

## Data exposure

The user-facing request snapshot contains only Approval ID, Task ID, Operation ID, action identifier, and requested lifetime. It does not contain paths, hosts, tool arguments, secrets, or model reasoning.

Grant internals and deadlines are private. Errors use stable non-sensitive categories and never echo action or resource values.

Cancellation and every terminal transition record `approval_revoked` before invalidating pending requests and unused grants. If an approval event batch cannot be persisted, Task state and approval ownership remain unchanged. If consumption-event persistence fails, the grant is consumed but no execution receipt is returned, so callers fail closed.

## Remaining integration work

- define a resource-safe Version 3 local API for request, approval, and denial;
- restore approval state safely after daemon restart;
- require a consumed receipt before a high-impact adapter operation executes.
