# Local API

## Status

Protocol Version 4 is the first stable Local API schema contract. The daemon remains pre-release and is not yet an operating-system security boundary.

Version 4 adds the audit-safe `task_failed` Event with a stable failure code, including `RUNTIME_RESTARTED`. Version 3 replaced Version 2 host-only network allowlists with exact TCP destinations containing a host and non-zero port. Version 3 retained Version 2's removal of the Version 1 `wait_for_approval` and `approve` methods. Those methods changed Task state using only a Task ID and were not bound to a policy-evaluated operation. Approval remains available through the trusted runtime API until a resource-safe local API schema is defined.

The current supported-version window is `4..=4`. The health response publishes this range. Incompatible changes require a new protocol version and future versions must overlap with their stable predecessor as defined by the [Local API Compatibility Contract](api-compatibility.md).

## Transport

`aiosd` listens on a Unix domain socket. It does not bind a TCP or UDP port.

Every connection carries exactly one request and one response:

```text
4-byte unsigned big-endian JSON length
N bytes of UTF-8 JSON
```

The default frame limit is 65,536 bytes. Configurations below 1,024 bytes or above 1 MiB are rejected. Empty frames are invalid.

Each connection has a five-second read and write timeout by default. The MVP handles connections sequentially, so concurrent work cannot grow without a fixed bound.

## Filesystem boundary

- The socket parent directory must be owner-only on Unix, normally mode `0700`.
- A missing immediate parent directory is created with mode `0700`.
- The socket is set to mode `0600`.
- Symlinked or group/world-accessible parent directories are rejected.
- An existing socket path is never removed or replaced automatically.
- On normal shutdown, the server removes the socket only if its device and inode still match the socket it created.

These checks restrict access to the local operating-system user. Explicit peer-credential policies and multi-user operation are future work.

## Requests

Every request uses an envelope with `protocol_version` and a tagged `request` object. Protocol Version 4 is the only supported version. Missing, unknown, or unsupported versions are rejected; unknown fields are also rejected.

The server reads the bounded envelope version before deserializing the method or Task body. A Version 3 request therefore receives `UNSUPPORTED_PROTOCOL_VERSION`; old response and Event schemas are never reinterpreted under new semantics.

### Health

```json
{"protocol_version":4,"request":{"method":"health"}}
```

```json
{
  "protocol_version": 4,
  "status": "ok",
  "result": {
    "type": "healthy",
    "supported_protocol_versions": {
      "minimum": 4,
      "maximum": 4
    }
  }
}
```

### Submit

```json
{
  "protocol_version": 4,
  "request": {
    "method": "submit",
    "task": {
      "idempotency_key": "repo-analysis-001",
      "goal": "Analyze the repository",
      "capabilities": {
        "filesystem": [
          {"path": "/workspace/project", "access": "read"}
        ],
        "network": {"mode": "deny"},
        "tools": ["test_runner"]
      },
      "budget": {
        "wall_time_seconds": 1800,
        "memory_bytes": 8589934592,
        "max_parallel_agents": 1
      },
      "approval": {
        "required_for": ["git.commit"]
      }
    }
  }
}
```

Protocol Version 4 retains complete network destinations introduced by Version 3:

```json
{
  "network": {
    "mode": "allow",
    "destinations": [
      {"host": "api.osv.dev", "transport": "tcp", "port": 443}
    ]
  }
}
```

Version 2 `hosts` arrays are rejected. Migration requires choosing each TCP port explicitly; the runtime does not infer port 443, TLS, or any-port access.

### Supported methods

| Method | Fields | Purpose |
| --- | --- | --- |
| `health` | none | Check daemon responsiveness |
| `submit` | `task` | Validate and queue a Task |
| `get_task` | `task_id` | Read public Task state |
| `events` | `task_id`, `after_sequence`, `limit` | Read up to 256 audit Events |
| `start` | `task_id` | Move a queued Task to running |
| `succeed` | `task_id` | Complete a running Task |
| `fail` | `task_id` | Fail a running or approval-waiting Task |
| `cancel` | `task_id` | Idempotently cancel a non-terminal Task |

## Responses

Every response declares the server's protocol version. Successful responses use `status: "ok"` and a tagged `result` object. Failures use `status: "error"` with a stable code and a non-sensitive message.

```json
{
  "protocol_version": 4,
  "status": "error",
  "error": {
    "code": "INVALID_STATE_TRANSITION",
    "message": "requested task transition is not allowed"
  }
}
```

Internal I/O errors, database details, Task goals, and capability values are not included in API errors.

## Restart behavior

`aiosd` completes recovery before accepting work:

1. The configured Unix socket is bound first, excluding another daemon using the same socket path without accepting requests yet.
2. SQLite Events are validated and reduced to public Task ID and state snapshots.
3. Every non-terminal snapshot receives one atomic `task_failed` Event with code `RUNTIME_RESTARTED` followed by its transition to `failed`.
4. Terminal Tasks remain unchanged. A later restart observes the terminal transition and does not append another failure.
5. Any corrupt sequence, capacity failure, or audit write failure aborts daemon startup and removes only the exact socket it created.

Recovered Task IDs remain available through `get_task` and `events`. Goals, Capabilities, model state, Tool arguments, pending operations, approval grants, and idempotency keys are never reconstructed from Events. Reusing a prior idempotency key after restart therefore creates a new Task only when the user explicitly submits it; the old Task is never executed again. Recovered Tasks count toward the configured Task capacity. One database must not be shared by daemons configured with different socket paths; database-level ownership enforcement remains future work.

## Command-line client

`aiosctl` speaks Protocol Version 4 and prints the complete JSON response. A Task file is read with a strict 65,536-byte limit before JSON parsing.

```bash
aiosctl --socket /path/to/aiosd.sock health
aiosctl --socket /path/to/aiosd.sock submit examples/task.json
aiosctl --socket /path/to/aiosd.sock get TASK_ID
aiosctl --socket /path/to/aiosd.sock events TASK_ID [AFTER_SEQUENCE] [LIMIT]
aiosctl --socket /path/to/aiosd.sock start TASK_ID
aiosctl --socket /path/to/aiosd.sock succeed TASK_ID
aiosctl --socket /path/to/aiosd.sock fail TASK_ID
aiosctl --socket /path/to/aiosd.sock cancel TASK_ID
```

Successful API responses exit with code `0`, API errors and invalid CLI input with code `2`, and transport failures with code `1`.

## Current limitations

- Protocol Version 4 has a stable schema contract; operational readiness remains blocked by the v0.1 Definition of Done.
- Policy-bound approval requests are not exposed through the local API yet.
- Task input and process-local idempotency state are not restored after daemon restart; resumable execution remains out of scope for v0.1.
- Malformed clients are isolated, but one local user can still consume time by repeatedly opening connections.
- Graceful signal handling and stale-socket recovery are not implemented.
