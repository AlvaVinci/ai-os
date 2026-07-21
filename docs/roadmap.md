# Roadmap

The roadmap uses verifiable exit criteria rather than calendar promises. Each phase may change based on measurements from the previous phase.

Phases describe implementation order, not releases. AI OS v0.1 is complete only when every mandatory gate in the [v0.1 Definition of Done](definition-of-done.md) is verified on one release commit. Roadmap checkbox counts must not be reported as release-readiness percentages.

## Phase 0: Foundation

Define the project boundary and security model.

- [x] Publish the vision, terminology, goals, and non-goals.
- [x] Define the MVP contract and acceptance criteria.
- [x] Separate AI decisions from deterministic enforcement.
- [x] Publish a structured threat model and full Capability model.
- [x] Establish architecture decision records.
- [x] Define representative developer workloads and benchmarks.

Exit criteria:

- At least three representative use cases identify required capabilities and approval boundaries.
- MVP acceptance criteria can be converted into automated tests.
- Component responsibilities and trust boundaries are documented.

## Phase 1: Safe Local Runtime

Run Tasks safely on one Linux device.

- [x] Strict Task schema and validation.
- [x] Deterministic Task state machine.
- [x] Idempotent submission in the synchronous Supervisor.
- [x] Bounded append-only in-memory Event Store.
- [x] Persistent SQLite Event Store and event-derived state recovery.
- [x] Experimental long-running daemon and bounded Unix-socket API.
- [x] Experimental `aiosctl` client and versioned local API requests.
- [x] Deterministic filesystem, network, and tool capability policy evaluation.
- [x] Bounded pending approvals and linear one-time approval grants.
- [ ] Stable local API compatibility contract.
- [ ] Deterministic non-resumable restart handling for active Tasks; encrypted resumable execution is deferred beyond v0.1.
- [x] Approval audit events, cancellation invalidation, and timeout enforcement.
- [x] Approval-aware in-process execution gate with complete-operation retention.
- [x] Bounded in-process Tool Catalog and Handler adapter without shell execution.
- [x] Bounded child-process Tool handler with explicit executable, argument policy, clean environment, and direct-child timeout.
- [ ] Principal-separated approval API.
- [ ] Operating-system enforcement and isolation adapters for filesystem, network, and out-of-process Tools.
- [ ] One local model adapter.

Exit criteria:

- Permission, approval, and cancellation acceptance criteria pass.
- Tests block unauthorized network and filesystem operations.
- Task state can be recovered after a runtime restart.

Completing this phase alone does not complete v0.1. Resource enforcement and the minimum end-to-end developer workflow are supplied by Phases 2 and 3 and remain subject to the Definition of Done.

## Phase 2: Resource-Aware Execution

Manage resource contention between Tasks and models.

- enforce CPU, RAM, and wall-time limits
- observe GPU and VRAM usage
- manage priority and concurrency
- control model loading, sharing, and release
- benchmark resource use against task quality

Exit criteria:

- A Task exceeding its budget stops without destabilizing other Tasks or the host.
- Speed, memory, and quality comparisons are reproducible for one workload.
- Scheduler decisions can be explained from Events.

## Phase 3: Developer Experience

Support practical repository workflows.

- stabilize the CLI and local API
- support multiple agents and dependencies
- provide developer SDKs
- publish task templates and policy profiles
- support diagnostics, export, and retry

Exit criteria:

- A representative repository investigation completes with a local model.
- A new Tool can be added without changing the core runtime.
- A failed Task exposes enough structured information to identify retry conditions.

## Phase 4: OS-Level Optimization

Identify performance or isolation limits that user-space code cannot solve.

- evaluate cgroups, namespaces, eBPF, and other existing Linux facilities
- optimize model weights, KV caches, and shared memory
- analyze GPU and NPU scheduling constraints
- compare kernel extensions with a custom kernel

Exit criteria:

- Measured bottlenecks and user impact are documented.
- User-space, Linux-extension, and custom-kernel alternatives are compared.
- Security and rollback procedures exist for every OS-level change.

## v0.1 release boundary

v0.1 combines the safe execution boundary from Phase 1, mandatory CPU/RAM/wall-time enforcement from Phase 2, and the minimum local-model developer workflow from Phase 3. Phase 4 is not required for v0.1 unless measurement shows that an existing Linux facility cannot satisfy a mandatory release gate.
