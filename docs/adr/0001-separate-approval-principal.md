# ADR-0001: Separate approval authority from untrusted execution principals

- Status: Accepted
- Date: 2026-07-19
- Related threats: TM-002, TM-003, TM-009, TM-018, TM-019
- Related objectives: SEC-003, SEC-008, SEC-010

## Context

The current local API uses an owner-only Unix socket. This proves that a client can access files as the daemon's OS user, but it does not distinguish a human-controlled client from an AI runner or another process under the same account.

Protocol Version 1 exposed Task-ID-only approval transitions. Those methods were removed in Version 2 because an identifier did not bind approval to a policy-evaluated operation and the socket did not establish an approver identity.

Future model and Tool runners will process untrusted content. If they can access the approval control channel, they can attempt to approve their own requests.

## Decision

Approval authority must be separated from untrusted execution by an enforceable principal boundary.

- The current local API will not expose approval methods.
- A future approval API must authenticate an approver independently from Task, Operation, and Approval IDs.
- Runner processes must not inherit or access the approval socket, credential, or raw Supervisor handle.
- Approval must reference a currently pending, capability-granted, exact operation and a fresh lifetime.
- Human-facing clients must display trustworthy operation context and freshness without exposing secrets.
- Same-user file permissions alone are insufficient when runner processes use the same OS account.

Candidate implementations include separate OS users, sandbox-specific socket visibility, peer-credential policy combined with process separation, or a narrowly scoped authenticated broker. The concrete mechanism requires a later ADR after Linux prototyping.

## Consequences

- Approval API work remains blocked until runner isolation and approver authentication are designed together.
- Local development can use trusted in-process runtime calls, but these are not a public multi-principal API.
- IDs remain safe to expose for correlation because they do not authorize by possession.
- The daemon architecture needs an explicit control plane and execution plane.

## Alternatives considered

### Reuse one owner-only socket

Rejected. It cannot distinguish the active user from an untrusted same-user runner.

### Treat Approval ID as a secret token

Rejected. IDs appear in audit and UI flows, do not prove human presence, and complicate revocation without solving runner access.

### Allow approval only inside the daemon process

Useful for early tests but insufficient as a user-facing design. It postpones rather than solves principal separation.
