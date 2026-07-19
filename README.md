# AI OS

A local-first, AI-native operating-system runtime for individuals and developers.

> [!IMPORTANT]
> AI OS is in active design and early implementation. It is not yet a bootable operating system or a production-ready runtime.

## Vision

Traditional operating systems treat processes, files, and windows as primary abstractions. AI OS adds first-class concepts for running AI safely and efficiently:

- **Task**: a goal with constraints, deadlines, and budgets
- **Agent**: an actor that plans and performs work for a task
- **Model**: a local or remote inference resource
- **Context**: short- and long-term information with provenance and expiration
- **Capability**: explicit permission to access files, networks, or tools
- **Budget**: limits for CPU, GPU, RAM, VRAM, power, time, and external API cost
- **Event**: an audit record that makes important decisions and operations traceable

AI decisions pass through deterministic policy and capability checks. Model output is never treated as a privileged instruction by itself.

## Initial scope

The first deliverable is a user-space runtime for Linux:

- structured task submission and lifecycle management
- agent startup, monitoring, cancellation, and retry control
- capability-based authorization and human approval gates
- local-first model routing
- CPU, GPU, memory, and time-aware resource management
- append-only events for auditability
- a CLI and stable local API

A custom kernel is outside the initial scope. AI OS will first measure real workloads in user space and evaluate kernel extensions only when Linux cannot satisfy a demonstrated requirement.

## Design principles

1. **Local first** — Data and inference stay on the device unless external access is explicitly allowed.
2. **Deterministic enforcement** — AI proposes actions; deterministic code enforces permissions and limits.
3. **Least privilege** — Each task receives only the capabilities it needs.
4. **Observable and replayable** — Important state changes, approvals, operations, and resource use are traceable.
5. **Model agnostic** — Core contracts do not depend on one model, vendor, or accelerator.
6. **Compatibility first** — Linux processes, files, containers, and existing developer tools remain usable.

## Architecture

```text
CLI / Local API / Future GUI
            |
Task & Agent Supervisor
            |
Policy / Capability / Approval
            |
Model Router & Resource Scheduler
            |
Model Runtime / Context Store / Event Store
            |
Linux Kernel / Containers / Hardware
```

Read more:

- [Vision](docs/vision.md)
- [Architecture](docs/architecture.md)
- [Threat model](docs/threat-model.md)
- [Capability model](docs/capability-model.md)
- [Capability policy](docs/capability-policy.md)
- [Approval grants](docs/approval-grants.md)
- [Tool adapter](docs/tool-adapter.md)
- [Process adapter](docs/process-adapter.md)
- [Architecture decisions](docs/adr/README.md)
- [MVP specification](docs/mvp-spec.md)
- [Roadmap](docs/roadmap.md)

## Project status

AI OS is in the early part of **Phase 1: Safe Local Runtime**.

Implemented:

- structured Threat Model, Capability Model, and Architecture Decision Records
- strict Task JSON contracts and validation in `aios-core`
- goal, idempotency, capability, budget, and approval boundary checks
- network deny-by-default with exact host allowlists
- normalized absolute-path validation and traversal rejection
- deterministic task states and idempotent cancellation
- UUIDv7 task identifiers
- bounded synchronous `TaskSupervisor` with idempotent submission
- bounded, append-only `InMemoryEventStore`
- SQLite-backed Event Store with atomic batches and schema versioning
- event-derived Task state recovery after a restart
- owner-only database creation and insecure-file rejection on Unix
- audit-first state changes that leave task state unchanged when event storage fails
- `aiosd` with a bounded, owner-only Unix-socket API
- one-request-per-connection framing, timeouts, and event pagination
- Protocol Version 2 with explicit incompatible-version rejection
- `aiosctl` for task submission, inspection, events, and lifecycle transitions
- deterministic filesystem, network, and tool capability policy decisions
- fail-closed authorization with resource-free denial and approval results
- bounded, expiring, task-operation-action-scoped approval requests
- linear one-time approval grants that cannot be cloned, debugged, or serialized
- policy-bound approval lifecycle with exact operation matching and audit events
- approval invalidation on denial, expiration, cancellation, and terminal Task states
- guarded adapter execution with complete-operation retention across approval
- bounded in-process Tool Catalog and Handler adapter without shell execution
- bounded child-process Tool handler with explicit executable identity, argument policy, clean environment, and direct-child timeout

Not implemented yet:

- operating-system enforcement of capabilities
- principal-separated approval API and operating-system isolation adapters
- persistent Task input and resumable execution recovery
- model execution and isolated out-of-process Tool execution
- resource usage enforcement and monitoring

## Development

The Rust toolchain is pinned in [rust-toolchain.toml](rust-toolchain.toml).

```bash
cargo fmt --all -- --check
cargo test --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

See [examples/task.json](examples/task.json) for a task input example. Tests load this file directly to detect schema drift.

Run the experimental daemon in a private temporary directory:

```bash
runtime_dir=/tmp/aios-demo
install -d -m 700 "$runtime_dir"
cargo run -p aios-local-api --bin aiosd -- \
  --socket "$runtime_dir/aiosd.sock" \
  --database "$runtime_dir/events.sqlite"
```

In another terminal, use the experimental client:

```bash
runtime_dir=/tmp/aios-demo
cargo run -p aios-local-api --bin aiosctl -- \
  --socket "$runtime_dir/aiosd.sock" health

cargo run -p aios-local-api --bin aiosctl -- \
  --socket "$runtime_dir/aiosd.sock" submit examples/task.json
```

The local API uses Protocol Version 2, but remains experimental and has no stable compatibility guarantee yet. See [Local API](docs/local-api.md) for the protocol and security boundaries.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Do not post security vulnerabilities in public issues; follow [SECURITY.md](SECURITY.md) instead.

## License

Licensed under the [Apache License 2.0](LICENSE).
