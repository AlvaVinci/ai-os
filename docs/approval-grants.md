# Approval Grants

## Status

Experimental process-local primitive for one-time human approval. It is not yet connected to the local API, Task lifecycle, Event Store, or an execution adapter.

## Lifecycle

1. A trusted adapter creates a unique Operation ID for one exact operation and resource.
2. The adapter requests approval for a Task ID, Operation ID, action identifier, and lifetime.
3. A user-facing component approves or denies the pending request.
4. Approval removes the pending request and returns a linear `ApprovalGrant`.
5. Immediately before execution, the adapter re-evaluates the capability and consumes the grant with the same Task ID, Operation ID, and action.
6. The adapter records the resulting receipt without copying resource values into the audit payload.

Approval and Operation IDs are public identifiers. They are not bearer secrets and do not authorize an operation by themselves.

## Scope and one-time use

Each pending request is bound to:

- one Task ID;
- one opaque Operation ID;
- one validated action identifier;
- one monotonic deadline.

`ApprovalGrant` does not implement `Clone`, `Debug`, or serialization. Its `authorize` method takes ownership of the grant, so every attempt consumes it. A wrong Task, Operation, or action fails with `ScopeMismatch`; an expired grant fails with `Expired`. Neither failure returns the grant.

The Operation ID must be bound to one exact structured operation by trusted adapter code. Model output must not select or reuse an Operation ID, and changing any operation argument requires a new Operation ID and approval.

## Bounds and expiration

The default authority permits at most 1,024 pending approvals and a maximum lifetime of 15 minutes. Configuration cannot exceed 65,536 pending approvals or a 24-hour maximum lifetime. Individual request lifetimes must be between 1 millisecond and the configured maximum.

Expired requests are removed before admitting new work and when the pending count is queried. Approval and denial both remove the request. Only one pending request may exist for the same Task and Operation pair.

Deadlines use `std::time::Instant`, so they are monotonic but process-local. Pending requests and grants do not survive a restart.

## Data exposure

The user-facing request snapshot contains only Approval ID, Task ID, Operation ID, action identifier, and requested lifetime. It does not contain paths, hosts, tool arguments, secrets, or model reasoning.

Grant internals and deadlines are private. Errors use stable non-sensitive categories and never echo action or resource values.

## Remaining integration work

- store the exact pending operation in a protected adapter-owned registry;
- connect approval requests to Task `waiting_approval` transitions;
- expose safe request, approve, and deny methods through the local API;
- append resource-free requested, granted, denied, expired, and consumed Events;
- invalidate pending approvals and grants when a Task is cancelled or becomes terminal;
- require a consumed receipt before a high-impact adapter operation executes.
