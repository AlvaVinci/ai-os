# ADR-0017: Route Agent network proposals through the Network gate

- Status: Accepted
- Date: 2026-08-02
- Related threats: TM-001, TM-004, TM-006, TM-008, TM-010, TM-011, TM-014, TM-016
- Related objectives: SEC-001, SEC-002, SEC-003, SEC-006, SEC-007, SEC-009

## Context

The Agent runtime could execute only registered Tool routes. The IP-bound Network Adapter separately enforced exact destination Capabilities, approval, bounded I/O, and the original Task wall-time, but no model-directed execution path could reach it.

Treating a model-selected Tool as implicit Network authority would violate Capability separation. Exposing the raw adapter or socket to a model or Tool would also move connection authority outside the trusted catalog and `ExecutionGate` boundary.

## Decision

- Extend the validated model decision contract with one bounded direct TCP exchange proposal: explicit host, port, and at most 64 KiB of request bytes.
- Release validated Task Network destinations to trusted Agent startup code only after the audit-first transition to `running`.
- Advertise only destinations accepted by the configured IP-only `NetworkCatalog`. Hostname Capabilities and all destinations remain hidden when no Network gate is configured.
- Reconstruct every model proposal through the Agent-owned `NetworkCatalog`, then submit it to the private `NetworkExecutionGate` for exact Capability and approval evaluation.
- Retain the adapter kind with a pending Approval ID so approval, denial, expiration, and cancellation dispatch only to the gate that owns the complete operation.
- Return only the bounded response bytes to the next model turn. Never return or expose the socket, raw adapter, approval grant, or Capability authority.
- Map a typed Network `BudgetExceeded` failure to the existing audit-first Agent Task failure and stable `BUDGET_EXCEEDED` Event.
- Fail the Task without connecting when the Network gate is absent, the proposal is unsupported, or the exact destination Capability is denied.

## Consequences

- A model can propose data for an already granted destination, but it cannot add a destination, bypass approval, restart the Task deadline, or obtain reusable socket authority.
- Approval retains the exact destination and request bytes. A resumed model receives only the resulting bounded response.
- The model session input now contains sensitive Task Network destinations. It remains non-debuggable and non-serializable, and concrete model adapters must not log it.
- Hostnames remain valid in the Task schema but are not advertised or executable through this IP-only Agent path.
- A remote peer may receive a partial or complete request before a later failure. Agent routing cannot roll back remote side effects.
- This does not add DNS, HTTP, TLS, proxy, redirect, inbound networking, Process Tool networking, Filesystem Agent routing, local API Agent execution, or a real model adapter.

## Verification

- Agent tests verify exact Task destination exposure only when a compatible Network gate is configured.
- A loopback integration test verifies model proposal, trusted catalog reconstruction, exact connection, bounded response observation, and Task success.
- Approval tests verify that no connection occurs before the exact retained operation is resumed.
- Denial and missing-gate tests verify that no connection occurs and the Task fails closed.
- A stalled response test verifies atomic `BUDGET_EXCEEDED` Task failure through the Agent path.

## Alternatives considered

### Grant networking through a Tool route

Rejected. A Tool Capability is intentionally independent from Network authority and cannot imply an outbound destination.

### Give the model a connected socket

Rejected. A live socket is reusable authority and would bypass exact-operation retention, byte bounds, approval, and Task wall-time enforcement.

### Advertise every Task hostname and resolve it in the Agent

Rejected. DNS, rebinding, alternate addresses, TLS identity, proxies, and redirects require a separate reviewed adapter contract.
