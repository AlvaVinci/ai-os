# ADR-0009: Enforce process CPU time and memory in a Task cgroup v2 boundary

- Status: Accepted
- Date: 2026-07-24
- Related threats: TM-009, TM-011, TM-016
- Related objectives: SEC-007, SEC-008, SEC-009

## Context

The Bubblewrap backend contains a Tool process tree in Linux namespaces and enforces a wall-clock timeout, but a busy or memory-hungry tree can still consume host CPU and RAM until that timeout. Killing only the launcher is also insufficient as a general resource response because every descendant must stop before the Task boundary is released.

The public `TaskSpec` currently declares wall time and memory but not a CPU-time field. Changing that stable Local API contract requires a versioned migration and runtime Event integration. The Process Adapter first needs a tested kernel boundary that later orchestration can bind to the versioned Task Budget without exposing untrusted execution before placement.

## Decision

AI OS introduces a cgroup v2 resource-control option for Task-scoped Bubblewrap execution.

- Host provisioning supplies an existing delegated cgroup v2 root beneath `/sys/fs/cgroup`.
- The delegated root must have the `cpu` and `memory` controllers enabled, contain no processes, and contain the runtime in a separate child cgroup. Invalid topology or unavailable controller files fails closed.
- `CgroupV2Manager` records the delegated root identity and creates exactly one new child named from the complete `TaskId`. An existing child is never reused or removed.
- `CgroupResourceBudget` carries a cumulative CPU-time ceiling and a resident-memory ceiling. Both must be nonzero, and CPU time is bounded by the Process Adapter's one-hour maximum.
- Task cgroup setup writes `memory.max`, disables swap with `memory.swap.max=0`, and enables group-wide OOM termination with `memory.oom.group=1`.
- CPU time is measured cumulatively from `cpu.stat`. Memory exhaustion is detected from `memory.events`. Reaching either ceiling terminates the complete cgroup through `cgroup.kill` and returns the redacted `ResourceLimitExceeded` category.
- A cgroup-controlled launch uses the fixed `aios-cgroup-launch` executable. The Process Adapter validates and rechecks its absolute executable identity.
- The trusted launcher opens the exact `cgroup.procs`, checks its parent against the Task cgroup device and inode recorded at creation, writes its own PID, clears its environment, and replaces itself with the validated Bubblewrap path through `exec`. It never invokes a shell or starts the Tool before cgroup placement.
- The existing trusted timeout remains a wall-clock ceiling. When a cgroup is configured, timeout termination also kills the complete cgroup rather than only the launcher.
- The Task scratch and Task cgroup identities must carry the same `TaskId`. Direct mode and the legacy path-only Bubblewrap constructors do not gain implicit resource control.
- `TaskCgroup::finish` is explicit. It kills remaining members, waits for the kernel to report an unpopulated cgroup, and removes only that dedicated cgroup. Dropping a cgroup authority does not perform destructive cleanup.

This increment does not change the Local API or claim DOD-005 completion. Runtime orchestration must still derive these limits from the versioned Task Budget, record resource-free Events before terminal state changes, return `BUDGET_EXCEEDED`, coordinate concurrent Task work, and publish measured peak usage.

## Consequences

- CPU use is a cumulative Task-scoped allowance rather than a per-process allowance, so repeated Tool launches cannot reset it.
- Resident memory and swap are kernel-enforced for the whole process tree. A process that handles an allocation failure still causes the adapter to stop the Task boundary after the first recorded hard-limit event.
- Only the small trusted cgroup launcher runs before placement. Bubblewrap and untrusted Tool code inherit the cgroup from their first instruction.
- Standard input, output, and error retain the existing null-stream contract; no synchronization descriptor reaches the Tool.
- CPU enforcement is polled at a bounded interval, so measured usage can exceed the configured threshold slightly before termination. All descendants contribute to the same counter.
- Task cgroup identity and configured memory controls are rechecked before launch and while the process runs. Identity mismatch or control mutation fails closed.
- Hosts must provision a writable delegated subtree and start the runtime inside it. AI OS does not modify the host cgroup root or silently run without requested resource control.
- A daemon crash can leave a Task cgroup behind. Reuse fails closed; startup reconciliation of stale cgroups remains a later operational lifecycle requirement.
- This boundary does not limit process count, disk use, GPU, VRAM, power, or thermal resources.

## Verification

- Unit tests validate resource-budget bounds.
- Linux integration tests run through the trusted cgroup launcher inside a delegated cgroup v2 subtree. They prove that Task identity mismatch and reuse fail closed and that CPU-time and resident-memory ceilings return `ResourceLimitExceeded`, stop the sandbox, and permit explicit cgroup removal.
- The existing descendant and timeout tests continue to exercise the namespace process boundary.
- Linux-target cross-compilation, workspace tests, formatting, and Clippy remain required.

## Alternatives considered

### Move Bubblewrap after it starts

Rejected. The Tool could execute or allocate memory before the kernel resource boundary applies.

### Block Bubblewrap on a standard file descriptor

Rejected. `--block-fd` closes the selected descriptor after synchronization. Using a standard descriptor would change the Tool stream contract, while preserving a separate inherited descriptor would add low-level descriptor manipulation to the otherwise safe Rust boundary.

### Use `setrlimit` for each direct child

Rejected for this boundary. Per-process limits do not aggregate descendants or repeated Tool launches, and they do not provide the same complete-tree termination primitive.

### Launch every Tool through `systemd-run`

Deferred. Systemd can manage delegated scopes, but making it mandatory would add a service-manager protocol and transient-unit lifecycle to the current boundary. Host provisioning may still use systemd to create the delegated root.

### Change the stable Task schema in the same increment

Rejected. Adding CPU semantics and `BUDGET_EXCEEDED` behavior requires a versioned Local API, Event, restart, and compatibility decision. The kernel adapter boundary is kept separately reviewable.
