# Roadmap

The roadmap uses verifiable exit criteria rather than calendar promises. Each phase may change based on measurements from the previous phase.

## Phase 0: Foundation

Define the project boundary and security model.

- [x] Publish the vision, terminology, goals, and non-goals.
- [x] Define the MVP contract and acceptance criteria.
- [x] Separate AI decisions from deterministic enforcement.
- [ ] Publish a structured threat model and full Capability model.
- [ ] Establish architecture decision records.
- [ ] Define representative developer workloads and benchmarks.

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
- [ ] Encrypted Task-input storage and resumable recovery.
- [ ] Operating-system enforcement adapters for filesystem, network, and tool capabilities.
- [ ] Approval API and audit-event integration, cancellation, and timeout enforcement.
- [ ] One local model adapter.

Exit criteria:

- Permission, approval, and cancellation acceptance criteria pass.
- Tests block unauthorized network and filesystem operations.
- Task state can be recovered after a runtime restart.

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
