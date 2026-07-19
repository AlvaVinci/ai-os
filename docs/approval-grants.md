# Approval Grants

## Status

Experimental process-local workflow for one-time human approval. It is connected to capability policy evaluation, Task lifecycle, the Event Store, and the generic `ExecutionGate`. It is not yet exposed through the local API or connected to a concrete operating-system adapter.

## Lifecycle

1. A trusted runtime caller submits one structured capability request for a running Task.
2. The supervisor evaluates the request and creates a unique Operation ID only when approval is required.
3. The supervisor stores the exact operation in process memory and records `approval_requested` with the transition to `waiting_approval` in one event batch.
4. A trusted user-facing component approves or denies the pending request.
5. Approval records `approval_granted`, resumes the Task, and stores the linear grant inside the supervisor. Denial records `approval_denied` and fails the Task.
6. `ExecutionGate` retains the complete operation object while approval is pending. Immediately before execution, it presents the retained capability request, consumes the grant, records `approval_consumed`, and only then invokes the adapter with the unchanged operation.

Approval and Operation IDs are public identifiers. They are not bearer secrets and do not authorize an operation by themselves.

## Scope and one-time use

Each pending request is bound to:

- one Task ID;
- one opaque Operation ID;
- one validated action identifier;
- one monotonic deadline.

`ApprovalGrant` does not implement `Clone`, `Debug`, or serialization. Its `authorize` method takes ownership of the grant, so every attempt consumes it. A wrong Task, Operation, or action fails with `ScopeMismatch`; an expired grant fails with `Expired`. Neither failure returns the grant.

The supervisor binds the Operation ID to the capability request, while `ExecutionGate` privately retains the complete adapter operation, including arguments that are intentionally absent from audit events. Model output must not select or reuse an Operation ID. The gate does not accept a replacement operation after approval, and every execution attempt removes the retained value.

## Bounds and expiration

The default authority permits at most 1,024 pending approvals and a maximum lifetime of 15 minutes. Configuration cannot exceed 65,536 pending approvals or a 24-hour maximum lifetime. Individual request lifetimes must be between 1 millisecond and the configured maximum.

The supervisor detects expired requests before request, approval, and denial operations, records `approval_expired`, and fails the waiting Task. Approval and denial both remove the request. Only one pending approval may exist for a Task at a time because the current Task state machine has one `waiting_approval` state.

Deadlines use `std::time::Instant`, so they are monotonic but process-local. Pending requests and grants do not survive a restart.

## Data exposure

The user-facing request snapshot contains only Approval ID, Task ID, Operation ID, action identifier, and requested lifetime. It does not contain paths, hosts, tool arguments, secrets, or model reasoning.

Grant internals and deadlines are private. Errors use stable non-sensitive categories and never echo action or resource values.

Cancellation and every terminal transition record `approval_revoked` before invalidating pending requests and unused grants. If an approval event batch cannot be persisted, Task state and approval ownership remain unchanged. If consumption-event persistence fails, the grant is consumed but no execution receipt is returned, so callers fail closed.

## Execution gate

`GuardedOperation` lets trusted adapter code derive a capability request from a complete typed operation. `ExecutionGate` owns the raw adapter and does not expose an adapter reference. It implements three paths:

- `allow`: record the policy decision, then execute immediately;
- `deny`: drop the complete operation without invoking the adapter;
- `approval_required`: retain the operation until `approve_and_execute`, then consume the grant before invoking the adapter.

Denial, cancellation, and expiration remove retained operations. Audit failure does not retain or execute a newly requested operation. Adapter failure is returned with a redacted message, and an approved operation cannot be replayed through the gate. Concrete adapters still own argument validation, idempotency for partial side effects, operating-system isolation, and resource-limit enforcement.

## Remaining integration work

- define a principal-separated Version 3 local API for request, approval, and denial;
- restore approval state safely after daemon restart;
- implement concrete filesystem, network, tool, and model adapters behind `ExecutionGate`.
