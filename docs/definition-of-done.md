# v0.1 Definition of Done

## Status

This document is the normative release-readiness contract for AI OS v0.1. The release target is a safe local AI runtime for one Linux x86_64 device. It is not a bootable operating system, a desktop replacement, or a claim that model output can enforce security policy.

AI OS may be described as a **v0.1 safe local runtime** only when every mandatory gate in this document is verified on the same release commit. Until then, it must be described as an early implementation with policy and control-plane components that are not an operating-system security boundary.

## User outcome

A developer can submit a bounded Task through the local API, have one supported local model execute it through isolated Tools, approve an exact high-impact operation through a separate trusted control path, and inspect the terminal result and resource-free audit Events. Filesystem, network, process, approval, cancellation, and resource boundaries are enforced independently from model output.

The release must execute all three versioned [developer workloads](../benchmarks/README.md) without bypassing their Capability, approval, privacy, or Budget gates.

## Scope

### Included

- one Linux x86_64 device;
- one supported local model runtime plus a deterministic test adapter;
- Task submission, inspection, cancellation, and terminal results;
- OS-bound Filesystem, Network, and out-of-process Tool enforcement;
- CPU, RAM, and wall-time Budget enforcement;
- principal-separated human approval for exact operations;
- resource-free audit Events and documented restart behavior;
- the CLI and a versioned local API with a published compatibility policy.

### Not included

- a custom kernel, bootable image, or replacement desktop;
- Windows or macOS support;
- a GUI or consumer installer;
- distributed execution across devices;
- multi-agent scheduling beyond the declared Task contract;
- portable GPU, VRAM, NPU, power, or thermal enforcement;
- resumable execution after daemon restart;
- long-term memory shared between Tasks.

These exclusions do not relax a security boundary. An unavailable feature must fail closed and must not be simulated with broader ambient authority.

## Mandatory release gates

### DOD-001: End-to-end local execution

- One real local model adapter and one deterministic test adapter implement the same model contract.
- A submitted Task advances from `queued` to `running` and then to a terminal state based on actual model and Tool execution.
- Model replacement does not change the Task or local API schema.
- Empty, oversized, null, malformed, duplicate, and concurrent inputs receive deterministic bounded outcomes.

### DOD-002: OS-bound Capability enforcement

- Filesystem authorization is bound to the opened object and prevents traversal, symlink, mount, and time-of-check/time-of-use escapes.
- Network authorization is bound to the actual TCP destination and controls DNS rebinding, redirects, proxies, alternate addresses, and TLS behavior.
- Out-of-process Tools cannot obtain filesystem, network, device, IPC, or credential access that the Task did not grant.
- A Tool grant never implies Filesystem or Network authority.

### DOD-003: Process isolation and cancellation

- Untrusted runners execute without the daemon control socket, approval channel, credentials, or unrelated inherited descriptors.
- The runtime applies an explicit process, namespace, and descriptor boundary before untrusted code begins.
- Cancellation and timeout terminate the complete process tree and prevent further operations.
- Partial side effects and non-idempotent retry behavior are documented and tested.

### DOD-004: Principal-separated approval

- The AI runner cannot reach or authenticate to the approval control path.
- The approver is authenticated independently from public Task, Operation, and Approval identifiers.
- The user sees a trustworthy summary of the exact operation before deciding.
- Approval remains exact-operation, Task-, action-, and lifetime-scoped, single-use, and fail-closed.
- Approval cannot add or widen a Capability.

### DOD-005: Resource Budget enforcement

- CPU, resident memory, and wall-time limits are measured and enforced against the Task's execution boundary.
- Reaching a hard limit stops new work, terminates the process tree, records a resource-free Event, and returns `BUDGET_EXCEEDED`.
- Concurrent Tasks cannot evade a per-Task limit or destabilize the daemon.
- GPU and VRAM use, when not enforceable, is reported as unsupported rather than silently accepted as bounded.

### DOD-006: Persistence and restart contract

- Events and their derived public Task state survive daemon restart and detect corrupt or incomplete sequences.
- v0.1 does not resume execution after restart. Before accepting new work, every previously non-terminal Task transitions to `failed` with the stable `RUNTIME_RESTARTED` category so it cannot be mistaken for running work.
- Pending approvals, grants, complete operations, and authority are never reconstructed from audit Events.
- Task goals, Capability values, Tool arguments, model responses, and secrets are not persisted in plaintext.
- Idempotency behavior across restart is explicit and cannot cause an old Task to execute again silently.

### DOD-007: API and operational reliability

- The local API publishes its supported-version window, incompatible-change policy, and migration procedure.
- The CLI supports health, submit, inspect, Events, cancel, and the trusted approval workflow without exposing sensitive values by default.
- Request, collection, connection, output, deadline, and storage limits have automated boundary tests.
- Installation, startup, shutdown, stale-socket recovery, upgrade, and rollback procedures are documented and tested on the supported Linux target.

### DOD-008: Audit, privacy, and release integrity

- Every security-relevant state change and operation decision is durably ordered before the associated effect.
- Default Events, errors, logs, and metric labels exclude goals, paths, destinations, arguments, secrets, complete model output, and private reasoning.
- The local audit limitation or a tamper-evident export design is documented and tested.
- Locked dependencies pass the project's vulnerability policy; release artifacts have provenance and signatures; the build procedure is reproducible or its remaining variance is documented.
- Every release blocker in the [Threat Model](threat-model.md) has verification evidence on the release commit.

### DOD-009: Workload and acceptance evidence

- All acceptance criteria in the [MVP specification](mvp-spec.md) are automated and pass on Linux x86_64.
- DEV-001, DEV-002, and DEV-003 complete with every benchmark security gate passing.
- At least one cold run and five measured warm runs per workload use the published reproducible-run protocol.
- The release report records source, workload, dataset, model, Tool, and environment identities and includes every failure or outlier.
- Unit, integration, adversarial boundary, restart, cancellation, and end-to-end test suites pass on the release commit.

## Release evidence

The release candidate must publish one machine-readable or reviewable evidence bundle containing:

```json
{
  "release": "v0.1.0",
  "commit": "<full Git commit>",
  "target": "linux-x86_64",
  "gates": {
    "DOD-001": "verified",
    "DOD-002": "verified",
    "DOD-003": "verified",
    "DOD-004": "verified",
    "DOD-005": "verified",
    "DOD-006": "verified",
    "DOD-007": "verified",
    "DOD-008": "verified",
    "DOD-009": "verified"
  },
  "workloads_passed": 3,
  "workloads_total": 3
}
```

Each `verified` value must link to automated test output, benchmark evidence, or an explicit reviewed operational test. Missing evidence, a partial implementation, an unsupported mandatory control, or an unresolved release blocker means the gate is not verified.

## Progress reporting

Progress is reported with three objective ratios:

1. verified Definition-of-Done gates out of 9;
2. passing MVP acceptance criteria out of 9;
3. passing end-to-end benchmark workloads out of 3.

Roadmap checkboxes and estimates of engineering effort may provide planning context, but they are not release-readiness percentages. A gate moves to `verified` only when its complete evidence is available on one candidate commit; partial work remains `in progress`.

### Baseline when this contract was adopted

- Definition-of-Done gates: **0 of 9 release-verified**. Existing schema, policy, API, audit, approval, and adapter components are partial evidence, but no complete gate has a release evidence bundle yet.
- MVP acceptance criteria: **not yet audited as a release set**. Existing unit and integration tests must be mapped to each criterion and supplemented by missing OS-bound and end-to-end tests.
- End-to-end benchmark workloads: **0 of 3 passing**. The fixtures are schema-tested, but no workload executes through a local model and OS-enforced adapters yet.

This baseline must be updated when a gate receives complete evidence; partial implementation does not change the verified count.

## Relationship to later phases

Phase 2 resource work supplies DOD-005. The minimum Phase 3 developer workflow and API work supplies parts of DOD-007 and DOD-009. Phase numbers describe implementation order; they are not release boundaries by themselves.

Phase 4 evaluates Linux facilities and custom-kernel work only after v0.1 produces measured security or performance evidence that user-space Linux cannot satisfy. A custom kernel is not required for v0.1.
