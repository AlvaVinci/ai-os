# Threat Model

## Status and scope

Version 0.1 for the Phase 1 local runtime. This document models the repository as implemented today and the planned Linux execution boundary. It is not a claim that operating-system isolation is complete.

In scope:

- Task submission and lifecycle;
- deterministic Capability Policy;
- Approval Authority and `ExecutionGate`;
- in-process Tool Catalog and handlers;
- Unix-socket local API and CLI;
- in-memory and SQLite Event Stores;
- future model, filesystem, network, and out-of-process Tool adapters.

Out of scope for this version:

- physical attacks on an unlocked device;
- a malicious host kernel, hypervisor, firmware, or root administrator;
- side-channel resistance against other privileged local workloads;
- distributed execution and remote multi-user administration;
- availability during hardware failure or full-disk exhaustion.

## Security objectives

| ID | Objective |
| --- | --- |
| SEC-001 | Model output, retrieved content, and Tool output never become authority by themselves. |
| SEC-002 | Missing, invalid, expired, or ambiguous permission fails closed. |
| SEC-003 | Approval can authorize only an already-granted Capability for one exact Task and operation. |
| SEC-004 | High-impact operations cannot execute before the corresponding audit and approval steps succeed. |
| SEC-005 | Cancellation and terminal Task states invalidate pending and unused authorization. |
| SEC-006 | Sensitive goals, resources, arguments, secrets, and model reasoning stay out of default Events and public errors. |
| SEC-007 | Every untrusted collection, payload, lifetime, and output has an explicit bound. |
| SEC-008 | Untrusted runners cannot access approval control paths, raw adapters, inherited privileged descriptors, or daemon credentials. |
| SEC-009 | Adapters bind authorization to the resource actually used by the operating system. |
| SEC-010 | Public identifiers such as Task, Operation, and Approval IDs are never treated as bearer credentials. |

The normative Capability rules behind these objectives are defined in [Capability model](capability-model.md).

## Assets

| Asset | Sensitivity | Required protection |
| --- | --- | --- |
| Task goal and inputs | confidential | explicit retention, encrypted persistence, no default audit copy |
| Capability resource values | confidential and integrity-critical | immutable per Task, exact matching, no untrusted modification |
| Approval decision and pending operation | integrity-critical | scoped, expiring, single-use, cancellation-aware |
| Tool and model arguments | potentially confidential | bounded, retained privately, no debug or audit serialization |
| Secrets and credentials | highly confidential | never in prompts or Events, scoped delivery, revocation |
| Event history | integrity-critical, partially confidential | append-only ordering, atomic batches, future tamper evidence |
| Adapter registry and executable configuration | integrity-critical | trusted startup ownership, no model-selected action or executable |
| Daemon socket and control credentials | integrity-critical | owner-only access, never inherited by untrusted runners |
| CPU, memory, GPU, file descriptors, and time | availability-critical | hard limits and termination outside model control |

## Actors

| Actor | Trust level | Capabilities and constraints |
| --- | --- | --- |
| Active local user | trusted authority | creates Tasks and makes approval decisions |
| Local API client | authenticated only as current OS user | may submit and inspect Tasks; is not automatically an approver |
| Runtime daemon | trusted enforcement | validates Tasks, owns policy, approval, gates, and audit writes |
| Model or agent runner | untrusted | proposes operations; must have no control socket or raw adapter access |
| Retrieved document or prompt author | untrusted data source | can influence model text but cannot change policy |
| Tool Catalog integration | trusted mapping code | maps routes to fixed tool and action identifiers |
| Tool handler | trusted in the current in-process adapter | validates semantic arguments and controls side effects |
| Future isolated adapter process | partially trusted | receives only scoped operation data and restricted OS resources |
| External service | untrusted remote party | may redirect, delay, replay, or return malicious content |
| Other same-user local process | untrusted for control-plane decisions | may reach owner-only files unless separated by OS sandboxing |
| Host kernel and root administrator | trusted computing base | enforce process, filesystem, network, and resource isolation |

## Trust boundaries and data flow

```text
[User / CLI]
      | B1: owner-only local API
      v
[Daemon: validation -> Supervisor -> Event Store]
      ^                         |
      | B2: untrusted requests | B5: persistent storage
[Model / Agent Runner]         v
      | B3: typed operation  [SQLite]
      v
[Catalog -> ExecutionGate -> Adapter]
      | B4: OS process/resource boundary
      v
[Linux kernel / files / network / devices]
      |
      | B6: external network trust boundary
      v
[Remote services]
```

- **B1 — Local control plane:** the current socket proves same-user filesystem access, not human presence or approver identity.
- **B2 — AI input boundary:** model output and retrieved text are always untrusted structured input.
- **B3 — Execution request boundary:** trusted Catalog or adapter integration constructs fixed Capability identifiers and complete operations.
- **B4 — OS enforcement boundary:** the experimental Bubblewrap process path supplies a deny-network namespace and restricted mount view; complete adapters must still bind policy to actual file descriptors, approved destinations, processes, and resource limits.
- **B5 — Persistence boundary:** Event data crosses into a local SQLite file; Task input persistence remains deferred until encryption and deletion semantics exist.
- **B6 — External boundary:** remote endpoints and their responses are untrusted even when egress is allowed.

## Security invariants

1. A Task's Capability set is validated before queuing and does not change during that Task instance.
2. Capability evaluation precedes approval evaluation; approval never creates a missing Capability.
3. A pending approval is scoped to Task ID, Operation ID, action, and monotonic deadline.
4. Complete operation arguments retained across approval cannot be replaced before execution.
5. An approval Grant is consumed on every authorization attempt and cannot be cloned or serialized through safe Rust APIs.
6. State changes and authorization decisions that require audit persistence do not apply when the Event batch fails.
7. Denied, expired, cancelled, terminal, mismatched, or already-attempted operations never reach an adapter through the guarded API.
8. Public API and Event payloads contain stable reason categories rather than sensitive resource values.
9. Model-visible Tool routes map to fixed Capability tool and action identifiers registered by trusted code.
10. A future runner process must not inherit the daemon socket, approval credentials, ambient network access, or unrelated file descriptors.

## Threat register

| ID | STRIDE | Threat and impact | Current mitigation | Residual risk / required control | Status |
| --- | --- | --- | --- | --- | --- |
| TM-001 | Elevation | Prompt injection attempts to add Capabilities or change approval rules. | Strict `TaskSpec`; opaque bounded model decisions; trusted Tool Catalog; deterministic policy; model output is not deserialized as policy. | Concrete inference adapters must parse backend output only through validated decision constructors. | Mitigated for Agent contract |
| TM-002 | Elevation | An AI runner calls an approval endpoint and approves its own operation. | Protocol Version 4 exposes no approval method. | Approval API requires a separate principal and control channel; see [ADR-0001](adr/0001-separate-approval-principal.md). | Release blocker |
| TM-003 | Spoofing | A client presents a Task, Operation, or Approval ID as proof of authority. | IDs are documented as public; approval requires internal pending state and a linear Grant. | Every future API lookup must authenticate actor and authorize Task ownership. | Partially mitigated |
| TM-004 | Tampering | A Tool request supplies a benign action but changes high-impact arguments after approval. | `ExecutionGate` retains the complete operation; `ToolOperation` fields are private. | Concrete adapters must cover every security-relevant argument in the retained value. | Mitigated for gate |
| TM-005 | Elevation | Model input selects a privileged Tool action directly. | Trusted Tool Catalog fixes route-to-tool/action mapping; raw Tool Adapter is private. | Registration configuration remains trusted and needs review and provenance. | Mitigated for in-process Tool |
| TM-006 | Elevation | Code bypasses `ExecutionGate` and calls a raw adapter. | `ToolExecutionGate` does not expose its raw adapter. | Future adapter crates must use the same facade pattern; language-level boundaries do not stop arbitrary unsafe native code. | Partially mitigated |
| TM-007 | Tampering | Filesystem path passes lexical policy but resolves through a symlink, mount change, or race. | Lexical normalization rejects traversal and prefix siblings. | Linux adapter must use descriptor-relative resolution such as `openat2`, reject unsafe links, and operate on the authorized descriptor. | Release blocker |
| TM-008 | Tampering | Allowed hostname resolves or redirects to a different destination. | Exact validated TCP host-and-port matching; network deny by default. | Network adapter must pin actual destination addresses, control TLS, redirects and proxies, and defend against DNS rebinding. | Release blocker |
| TM-009 | Elevation | Child process inherits daemon socket, credentials, network, or unrelated descriptors. | Direct mode clears environment and standard streams. Experimental Bubblewrap mode uses an isolated root view, no host network, and preserves no extra descriptors. | Make isolation mandatory for every untrusted runner; derive mounts from Task Capabilities; add Linux descriptor, socket, escape, and descendant tests. | Partially mitigated in Bubblewrap plan; release blocker |
| TM-010 | Information disclosure | Goals, paths, arguments, secrets, or model reasoning leak through Events or errors. | Resource-free Events; sensitive request types omit `Debug` and serialization; redacted adapter errors. | Crash dumps, handler logs, metrics labels, and external libraries still require review. | Partially mitigated |
| TM-011 | Denial of service | Oversized frames, Tasks, approvals, model turns, Tool arguments, outputs, or retained collections exhaust memory. | Explicit limits on frames, Tasks, Events, approvals, model turns, final output, routes, arguments, and Tool output. | CPU, disk, concrete model context, connection rate, and decompression limits remain. | Partially mitigated |
| TM-012 | Repudiation | A local actor alters or deletes SQLite Events and denies an action. | Owner-only file and append ordering checks. | Cryptographic tamper evidence, durable identity, clock semantics, and protected export are not implemented. | Open |
| TM-013 | Tampering | Runtime restart loses pending approvals or complete operations but persisted state implies resumability. | Socket-path exclusion is established before recovery; before accepting requests, recovery records `RUNTIME_RESTARTED` and `failed` for every interrupted Task. No input, operation, or approval authority is reconstructed; audit failure aborts startup, and repeat recovery is idempotent. | Database ownership across different socket paths is not enforced. Any future resumable design needs encrypted retention, key management, fresh authorization, and replay controls. | Partially mitigated for v0.1 non-resumable contract |
| TM-014 | Elevation | Approval is reused across Tasks, operations, actions, or after expiry. | Task/Operation/action/deadline scope; monotonic time; linear consumption; exact operation comparison. | Persistence and multi-process authority require equivalent atomic semantics. | Mitigated in process |
| TM-015 | Tampering | Cancellation races with approval or execution. | Synchronous Supervisor; audit-first transition; terminal states revoke pending and unused approvals. | Concurrent runtime must serialize per-Task authorization and cancellation. | Mitigated for synchronous runtime |
| TM-016 | Denial of service | A Tool handler blocks forever or returns after partial side effects. | Input/output bounds, one execution attempt through the gate, and a direct-child timeout in the Process Adapter. | General handler cancellation, idempotency declaration, descendant cleanup, and isolated termination are not implemented. | Partially mitigated |
| TM-017 | Elevation | Trusted in-process Tool handler performs undeclared filesystem or network access. | Handler registration is trusted and explicit. | Move high-risk handlers out of process and apply OS Capability enforcement. | Open |
| TM-018 | Information disclosure | Another same-user process reads the socket or future encrypted Task key material. | Socket and database use owner-only permissions. The experimental isolated root omits host runtime paths by default. | Enforce sandbox-specific socket invisibility for every runner, authenticate peer credentials, separate the approval listener, and add keychain or kernel-backed key handling. | Release blocker for approval API |
| TM-019 | Spoofing | Approval UI displays incomplete or misleading operation context. | Approval request exposes stable IDs, action, and lifetime without secrets. | Trusted UI needs protected resource summaries, actor identity, freshness, and anti-confusion presentation. | Open |
| TM-020 | Elevation | Dependency or build compromise changes policy or adapter behavior. | Small Rust dependency set, lockfile, forbidden unsafe code in workspace crates. | Add dependency audit, provenance, release signing, and reproducible build checks. | Open |

## Abuse cases

### Untrusted content requests network exfiltration

The model may propose egress after reading malicious content. Network deny-by-default blocks the request unless the Task already grants the exact TCP host and port. If egress also requires approval, the operation remains paused. Approval cannot add or alter a destination. The future network adapter must bind the logical destination to the actual socket address and enforce DNS, proxy, redirect, and TLS behavior.

### Model requests a destructive Tool route

The model can select only registered route names. The Catalog assigns the fixed action, and the Task must grant the corresponding Tool. If approval is required, `ExecutionGate` retains the complete arguments. The model cannot replace them after approval. High-risk handlers still require OS isolation and semantic argument validation.

### Same-user process attempts self-approval

Owner-only socket permissions do not prove human authority. The current API therefore exposes no approval operation. A future approval control plane remains blocked until the runner cannot access that control channel and the server can authenticate the approver principal.

### Event storage becomes unavailable

Submission, state transitions, and approval batches fail without applying the corresponding in-memory change. Grant consumption fails closed: no receipt is returned and the operation is not executed through the gate.

## Verification matrix

| Security objective | Existing evidence | Required next evidence |
| --- | --- | --- |
| SEC-001 / SEC-002 | Task validation and Capability Policy unit tests | adapter integration tests for every new resource type |
| SEC-003 / SEC-004 | approval scope, expiry, linearity, audit-failure, and execution-gate tests | principal-separated approval API tests |
| SEC-005 | cancellation, denial, expiry, and terminal invalidation tests | concurrent race tests when concurrency is added |
| SEC-006 | resource-free Event serialization and redacted error tests | log and crash-report review |
| SEC-007 | capacity tests across API, Event, approval, gate, and Tool Adapter | CPU, disk, model context, and connection-rate tests |
| SEC-008 / SEC-009 | private in-process adapters, content-addressed sealed rootfs and fresh Task-ID-scoped scratch tests, Bubblewrap launch-plan tests, and a Linux boundary suite for root/scratch visibility, descriptor closure, control-socket invisibility, host-network denial, and descendant cleanup after exit or timeout | OS-backed rootfs immutability, descriptor-bound filesystem race, Capability-derived mount, and approved network destination tests |
| SEC-010 | identifier-only approval tests and API removal | ownership authorization tests for future APIs |

## Release gates

The project must not describe Phase 1 as a safe OS-enforced runtime until all of the following are implemented and tested on Linux:

- runner/control-plane principal separation;
- filesystem resolution bound to authorized descriptors;
- network authorization bound to actual destinations;
- child-process descriptor, environment, namespace, and resource isolation;
- cancellation and timeout enforcement for child processes;
- encrypted resumable Task storage or an explicit non-resumable product contract;
- tamper-evident audit export or a documented local-audit limitation;
- dependency and release integrity checks.

These security gates are mandatory inputs to [DOD-008](definition-of-done.md#dod-008-audit-privacy-and-release-integrity). Phase or roadmap completion cannot waive them.

## Review cadence

Update this model whenever a new adapter, persisted sensitive field, approval surface, external service, OS privilege, or concurrency mechanism is introduced. Every security-relevant architecture decision should link to the affected Threat IDs and Security Objectives.
