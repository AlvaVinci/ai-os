# ADR-0012: Reconcile stale Task cgroups during non-resumable startup

## Status

Accepted

## Context

A daemon failure or forced termination can leave processes and Task cgroups beneath the delegated cgroup v2 root. Reusing an existing `task-{TaskId}` child already fails closed, but without startup cleanup the process tree can continue consuming resources after the durable runtime has lost its model session, Tool operation, and approval authority.

Deleting every child beneath a configured cgroup root is unsafe. The root also contains the runtime's own cgroup and may contain host-provisioned children. Directory-name discovery alone does not establish that AI OS durably owns a Task identifier.

The v0.1 daemon already implements a non-resumable restart contract. It reconstructs audit-safe Task identifiers and states from SQLite, records `RUNTIME_RESTARTED` for every interrupted Task, and accepts no request until recovery succeeds.

## Decision

When `aiosd` is configured with `--cgroup-root`, startup will:

1. bind the configured control socket without accepting requests;
2. recover and validate Task state from SQLite;
3. durably fail every previously non-terminal Task with `RUNTIME_RESTARTED`;
4. pass only the recovered Task identifiers to `CgroupV2Manager`;
5. for each exact `task-{TaskId}` child that exists, reject a target containing the daemon's current cgroup, revalidate the delegated root and child identity, write `1` to `cgroup.kill`, wait for `populated 0`, revalidate again, and remove that one empty cgroup;
6. abort startup on any configured-root, identity, control-file, termination, timeout, or removal failure;
7. begin accepting local API requests only after reconciliation succeeds.

Reconciliation never enumerates or recursively deletes cgroup children. An absent exact child is already reconciled, making repeated startup idempotent. A child not represented by the selected Event Store is left untouched for operator investigation.

The delegated root is exclusive deployment state for one daemon and its Event Store. A deployment that uses Task cgroups must pass the same root on every startup and must run the daemon in a separate child cgroup beneath it. Omitting `--cgroup-root` makes no stale-process cleanup claim.

The stable Local API Version 4 schema does not change.

## Consequences

- A Process Tool tree left by a daemon crash cannot continue after successful configured startup.
- Durable failure audit precedes destructive process cleanup.
- Terminal Task cgroups left by an incomplete normal cleanup are also removed because terminal identifiers are recovered from the Event Store.
- Unrelated host or runtime cgroups are not selected.
- Lost, replaced, corrupt, or incomplete Event storage prevents cleanup rather than widening the deletion set.
- The complete reconciliation pass has a five-second deadline. Partial success is safe and a later startup retries the remaining exact identifiers.
- Database and cgroup-root ownership across differently configured daemon instances is documented but not locked across processes yet.
- Task scratch cleanup remains separate because it contains Tool-controlled filesystem content and requires its own reviewed lifecycle.

## Alternatives considered

### Enumerate and delete every `task-*` child

Rejected. A name prefix is not durable ownership evidence and could kill work belonging to another configuration.

### Delete the complete delegated root recursively

Rejected. It would include the runtime cgroup and unrelated host-provisioned children, and recursive deletion is unnecessary.

### Clean before recording `RUNTIME_RESTARTED`

Rejected. A cleanup failure could stop work without preserving the durable non-resumable outcome.

### Ignore cleanup failure and accept requests

Rejected. The daemon would present a recovered control plane while stale execution could remain active.

## Verification

- SQLite restart tests prove that the Supervisor exposes only identifiers reconstructed from durable Events.
- Linux cgroup tests leave a process in one recovered Task cgroup, verify that reconciliation kills and removes it, preserve an unselected Task cgroup, and verify idempotent retry.
- Daemon option tests cover explicit and omitted cgroup-root configuration.

## Related requirements

- DOD-006: persistence and restart contract
- MVP FR-019: explicit non-resumable outcome
- Threat Model TM-013: restart authority confusion
