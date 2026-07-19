# Capability Model

## Status

Version 0.1. This document defines the normative authorization model for the current Phase 1 runtime and distinguishes implemented policy from future operating-system enforcement.

## Authority chain

```text
Active user
  -> validated immutable TaskSpec
    -> CapabilityPolicy decision
      -> optional scoped Approval Grant
        -> ExecutionGate
          -> concrete adapter
            -> operating-system resource
```

Authority only narrows as it moves down this chain. Model output, Operation IDs, Approval IDs, Events, and adapter output are not authority sources.

## Entities

### Task Capability grant

A Task Capability grant is an immutable declaration inside a validated `TaskSpec`. It identifies a resource class and the allowed operation scope. Omitted grants deny access.

Current resource classes:

| Class | Resource scope | Operation scope | Approval action |
| --- | --- | --- | --- |
| Filesystem | normalized absolute path and descendants | `read` or `write`, independently | `filesystem.read` or `filesystem.write` |
| Network | exact validated lowercase hostname or IP address | outbound connection | `network.egress` |
| Tool | exact registered capability Tool name | adapter-supplied action | the fixed action identifier |

Planned classes such as secrets, models, devices, IPC, and process execution require a specification and adapter before use. They are denied by absence today.

### Capability Request

A Capability Request is constructed by trusted adapter integration from one complete operation. It contains only the fields needed for deterministic policy evaluation. Request types may reference sensitive resources and therefore intentionally omit `Debug` and serialization.

The complete operation can contain additional arguments. `ExecutionGate` retains that complete value across approval so policy-relevant fields and operation arguments cannot be replaced before execution.

### Policy Decision

Policy evaluation returns exactly one resource-free result:

- `allow`: a matching Capability exists and no approval rule applies;
- `deny`: the request is invalid or no matching Capability exists;
- `approval_required`: a matching Capability exists and the fixed action requires approval.

Evaluation order is mandatory:

1. validate the operation request;
2. match the granted Capability;
3. evaluate the approval rule;
4. never infer or broaden a resource.

### Approval Grant

Approval is an additional condition, not a Capability. A Grant is scoped to:

- Task ID;
- Operation ID;
- fixed action;
- monotonic deadline.

A Grant is linear and process-local. It cannot be cloned, debugged, or serialized through safe Rust APIs. Every authorization attempt consumes it, including mismatch or expiration.

### Execution Gate and adapter

`ExecutionGate` is the in-process point that joins Capability decision, approval, complete operation retention, and adapter invocation. The concrete adapter must then bind the approved logical resource to the actual operating-system resource.

The gate is necessary but insufficient for OS enforcement. An untrusted runner must not receive a raw adapter, daemon control socket, privileged descriptor, or alternate execution path.

## Grant properties

### Deny by default

Missing resource classes, empty allowlists, unknown Tool routes, invalid identifiers, and malformed resources deny the operation. Approval rules cannot turn denial into allow.

### Exact matching

- Filesystem access mode is exact; read does not imply write and write does not imply read.
- Filesystem descendants require a path-component boundary; string-prefix siblings are excluded.
- Network hostname or IP matching is exact; subdomains, ports, schemes, and paths are not inferred.
- Tool name and action matching are exact identifiers fixed by trusted registration.

### No ambient inheritance

A Task does not inherit Capabilities from another Task, a previous Task, the daemon user, the model process, a parent process, or an approval history. A retry creates a new Task and requires fresh authority.

### No delegation in Phase 1

Tasks and agents cannot delegate, mint, widen, or persist Capabilities. Sub-agent semantics are not defined yet. When introduced, delegated authority must be a strict subset with explicit lifetime and revocation.

### Revocation and terminal state

Cancellation, failure, success, rejection, denial, and expiration remove pending or unused approval authority as applicable. Terminal Tasks never return to an execution state. Existing immutable Task grants are no longer executable after termination.

## Resource enforcement requirements

### Filesystem

Current policy is lexical. A Linux filesystem adapter must additionally:

- resolve from a trusted directory descriptor;
- reject unsafe symlink, magic-link, traversal, and mount escape behavior;
- bind authorization and use to the same opened descriptor;
- prevent inherited descriptor and subprocess bypass;
- apply read/write semantics to the actual system call;
- define behavior for rename, hard links, deletion, metadata, and atomic replacement before exposing them.

### Network

Current policy matches the requested host. A network adapter must additionally:

- resolve and validate every actual destination address;
- define allowed ports and protocols explicitly;
- control redirects, proxies, DNS rebinding, and alternate address forms;
- bind approval to the final connection destination and transmitted data class;
- prevent subprocess and inherited-socket bypass;
- deny listening sockets unless a separate inbound Capability is defined.

### Tool

The in-process Tool Adapter currently:

- maps a model-visible route to fixed Tool and action identifiers;
- bounds route count, identifiers, argument count, argument bytes, and output bytes;
- passes arguments without shell interpretation;
- keeps the raw handler adapter behind `ToolExecutionGate`;
- revalidates scope immediately before handler invocation.

The Process Adapter can be registered as a constrained handler. It fixes a canonical executable and working directory, requires a trusted dynamic-argument policy, clears the environment, discards standard streams, and times out the direct child. These controls reduce ambient process authority but do not provide OS Capability enforcement.

Trusted handlers remain capable of ambient process access. High-risk or untrusted Tools require an out-of-process adapter with a separate principal, descriptor allowlist, clean environment, explicit executable identity, time and memory limits, and no daemon control socket.

### Secrets

No Secret Capability exists yet. Until one is specified:

- secrets must not be placed in Task goals, prompts, Events, Tool arguments, or public errors;
- adapters must not read ambient credential stores on behalf of a Task;
- approval does not authorize credential access;
- future secret delivery must be scoped, non-serializable, revocable, and excluded from child environments by default.

### Models and devices

Model and accelerator selection is not a Capability today. Future model/device authority must distinguish local inference, external transmission, model file access, GPU or NPU device access, memory limits, and data sensitivity. External model use requires both network authority and an explicit data-release policy.

## Capability lifecycle

1. The user or trusted client creates a complete `TaskSpec`.
2. Validation rejects malformed, duplicate, empty, or excessive grants.
3. Accepted grants remain immutable for the Task lifetime.
4. Trusted adapter code creates a complete operation and corresponding Capability Request.
5. Policy returns allow, deny, or approval required.
6. If required, approval creates one scoped linear Grant without widening the Capability.
7. `ExecutionGate` rechecks and consumes authority before invoking the adapter.
8. The adapter enforces the actual OS resource and records only resource-free audit metadata.
9. Cancellation or terminal state invalidates outstanding authorization.

## Identifier semantics

Task, Operation, Approval, Event sequence, and Tool route identifiers support correlation and lookup only. Possession does not prove identity, ownership, approval, or permission. Future APIs must authenticate the caller and authorize access independently of identifiers.

## Data handling

| Data | Default Event | Public error | Persistent storage |
| --- | --- | --- | --- |
| Task/Operation/Approval IDs | allowed | allowed when needed | Event Store |
| Stable decision and reason code | allowed | allowed | Event Store |
| Goal and private context | excluded | excluded | deferred encrypted store |
| Filesystem path or network host | excluded | excluded | Task storage only when encrypted |
| Tool arguments and output | excluded | excluded | not persisted by core runtime |
| Approval Grant internals | excluded | excluded | never persisted in current design |
| Secrets and credentials | excluded | excluded | no storage design yet |

## Security invariants

| ID | Invariant |
| --- | --- |
| CAP-001 | No operation executes without a matching immutable Task Capability. |
| CAP-002 | Approval never widens or creates a Capability. |
| CAP-003 | The complete approved operation cannot be replaced before execution. |
| CAP-004 | Authority is scoped to one Task and is invalid after terminal state. |
| CAP-005 | Identifiers and Events are not bearer authority. |
| CAP-006 | Untrusted model data cannot choose trusted action, executable, credential, or adapter identity. |
| CAP-007 | Adapter enforcement uses the actual OS resource, not only the requested string. |
| CAP-008 | Sensitive Capability values do not enter default audit or error channels. |
| CAP-009 | Every request, retained object, and output is bounded. |
| CAP-010 | New resource classes are denied until both policy semantics and enforcement adapters exist. |

Violations of these invariants are security defects. The associated threats and release blockers are tracked in [Threat model](threat-model.md).

## Current enforcement matrix

| Layer | Filesystem | Network | Tool | Approval |
| --- | --- | --- | --- | --- |
| Schema validation | implemented | implemented | implemented | implemented |
| Deterministic policy | lexical path/access | exact host, deny default | exact Tool/action | capability-first action match |
| Complete operation retention | runtime type available | runtime type available | implemented by Tool gate | implemented |
| OS resource binding | not implemented | not implemented | path/inode precheck only; OS binding not implemented | process-local only |
| Restart recovery | no sensitive Task input | no sensitive Task input | no pending operation | public Task state only |

The matrix must be read literally. A policy check without OS resource binding is not complete Capability enforcement.
