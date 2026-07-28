# ADR-0011: Bind the stable Task Budget to isolated process execution

- Status: Accepted
- Date: 2026-07-29
- Related threats: TM-011, TM-015, TM-016
- Related objectives: SEC-005, SEC-006, SEC-007, SEC-009

## Context

The stable Version 4 `TaskSpec` already carries wall-time, resident-memory, and parallel-agent limits. The Process Adapter separately supports a trusted wall timeout and a Task-ID-scoped cgroup with cumulative CPU-time and resident-memory limits. Those values were not connected: trusted startup code could configure limits unrelated to the accepted Task, and every Tool handler failure was reduced to one generic category before the Agent or Supervisor could record `BUDGET_EXCEEDED`.

Adding a CPU-time field or a new Event variant would change the stable Local API contract and require a new protocol version. The current synchronous Agent also supports only one active model session, so the accepted `max_parallel_agents` value cannot currently create concurrent work inside one `AgentRuntime`.

## Decision

AI OS binds the existing Version 4 Task Budget to trusted Tool execution without changing the wire schema.

- `TaskSupervisor` records a monotonic start instant after the audit-first transition to `running` and releases a non-serializable `TaskExecutionContext` only for that running Task. The context contains the immutable Task ID, validated Budget, and the start instant needed to calculate remaining wall time.
- `ToolExecutionGate` binds that context to the private complete `ToolOperation` before capability and approval processing. A model cannot construct or replace it.
- `ToolHandler::execute_for_task` receives the context through the private adapter path. Existing trusted handlers retain their context-free implementation by default.
- The Tool Adapter preserves only one handler failure distinction: `BudgetExceeded`. All other handler details remain reduced to the existing redacted failure category.
- `CgroupResourceBudget::from_task_budget` maps `memory_bytes` exactly to `memory.max`.
- Until a separately versioned CPU-time field exists, `wall_time_seconds` is also used as a conservative cumulative CPU-time ceiling. The same value is the elapsed wall-time ceiling for Task-bound process execution.
- A Task-bound Bubblewrap handler requires a cgroup whose Task ID and derived limits exactly match the execution context. A missing or mismatched cgroup fails before spawn and is not reported as budget exhaustion.
- Direct trusted process handlers apply the shorter of their configured timeout and the Task wall-time ceiling. Direct mode still does not enforce Task memory and remains outside the untrusted-process release boundary.
- Remaining wall time is calculated from the original Task start. Approval waits, repeated Tool calls, and model turns do not reset it. The Tool Adapter rejects an expired retained operation before invoking its handler, the Agent checks the same deadline before and after every synchronous model turn, and the existing Agent expiry poll records budget failure while approval remains idle.
- A real cgroup or Task wall-time limit returns `BudgetExceeded` through the Tool Adapter. `AgentRuntime` stops the active session and calls `TaskSupervisor::fail_budget_exceeded`.
- The Supervisor atomically appends the existing resource-free `TaskFailed { code: BUDGET_EXCEEDED }` Event and the transition to `failed` before changing in-memory state. Audit failure leaves the Task running and returns a supervision error; execution is never replayed.
- The Version 4 request schema, Event tag set, Task states, and error-code set do not change. This decision uses the existing `TaskFailed` Event and existing `BUDGET_EXCEEDED` code.

The Process Adapter currently accepts at most one hour for a Task-bound cgroup CPU ceiling. A larger otherwise-valid Task Budget cannot create this backend and fails closed before untrusted process execution. Raising that implementation bound is separate from changing the wire schema.

## Consequences

- The accepted Task, cgroup, Process Tool, Agent failure, and terminal audit record now share one Task identity and one memory/wall-time Budget.
- Repeated Process Tool calls cannot reset cumulative cgroup CPU accounting.
- Approval waits, model calls, and repeated Tool calls consume one shared monotonic Task wall-time allowance.
- New Tool calls stop after the Agent records budget exhaustion because the Task is terminal and the active model session is dropped.
- Trusted handlers can return the budget category, so handler registration and implementation remain trusted configuration.
- Using wall time as the current CPU allowance is conservative for multi-process workloads: cumulative CPU use can reach the limit before elapsed wall time. A future explicit CPU field requires a versioned API decision.
- This increment does not enforce model inference memory, disk, process count, GPU, VRAM, power, thermal use, or multi-runtime concurrency.
- The local daemon still does not execute the Agent path, so `BUDGET_EXCEEDED` is observable through Agent errors and Task Events but not yet returned by a daemon run method.
- The synchronous runtime has no background timer. Its owner must call the existing expiry poll to observe a deadline while the Agent is idle waiting for approval; Tool or model activity checks the deadline directly.

## Verification

- Supervisor tests prove that execution context is released only after the running transition and that budget failure Events are audit-first and atomic.
- Tool Adapter tests prove that only the budget category survives handler redaction.
- Agent and Tool Adapter tests cover immediate exhaustion, a slow Tool crossing the Task deadline, approval execution and expiry polling beyond the deadline, terminal state, session removal, and `BUDGET_EXCEEDED`.
- Process Adapter tests derive cgroup limits from the stable Budget and enforce Task wall time through the Tool gate.
- The Linux x86_64 boundary suite runs a CPU-bound Bubblewrap Tool through the Agent, verifies terminal `BUDGET_EXCEEDED`, and removes the complete Task cgroup.

## Alternatives considered

### Add CPU time to Version 4

Rejected. Adding a required request field changes the stable wire schema and requires a new protocol version with an overlapping support window and migration fixtures.

### Treat every Tool failure as budget exhaustion

Rejected. It would produce false audit records for argument denial, spawn failure, policy mismatch, and internal adapter errors.

### Let trusted configuration silently override the Task Budget

Rejected. A looser value would violate the accepted Task, while a tighter value reported as Task exhaustion would misstate the user's Budget.

### Record a new resource Event type

Rejected for Version 4. The existing `TaskFailed` Event already carries the stable resource-free error code, while adding a tagged Event variant is an incompatible protocol change.
