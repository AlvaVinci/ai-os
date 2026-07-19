# Local API

## Status

Experimental Protocol Version 3. The version number is explicit, but the framing and schema may still change before the first stable release.

Version 3 replaces Version 2 host-only network allowlists with exact TCP destinations containing a host and non-zero port. Version 3 retains Version 2's removal of the Version 1 `wait_for_approval` and `approve` methods. Those methods changed Task state using only a Task ID and were not bound to a policy-evaluated operation. Approval remains available through the trusted runtime API until a resource-safe local API schema is defined.

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

Every request uses an envelope with `protocol_version` and a tagged `request` object. Protocol Version 3 is the only supported version. Missing, unknown, or unsupported versions are rejected; unknown fields are also rejected.

The server reads the bounded envelope version before deserializing the method or Task body. A Version 2 request therefore receives `UNSUPPORTED_PROTOCOL_VERSION` even when its legacy Task shape is invalid under Version 3; old Capability data is never reinterpreted under new semantics.

### Health

```json
{"protocol_version":3,"request":{"method":"health"}}
```

### Submit

```json
{
  "protocol_version": 3,
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

Protocol Version 3 network allowlists use complete destinations:

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
  "protocol_version": 3,
  "status": "error",
  "error": {
    "code": "INVALID_STATE_TRANSITION",
    "message": "requested task transition is not allowed"
  }
}
```

Internal I/O errors, database details, Task goals, and capability values are not included in API errors.

## Command-line client

`aiosctl` speaks Protocol Version 3 and prints the complete JSON response. A Task file is read with a strict 65,536-byte limit before JSON parsing.

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

- Protocol Version 3 is experimental and does not yet carry a long-term compatibility guarantee.
- Policy-bound approval requests are not exposed through the local API yet.
- Task input and idempotency state are not restored after daemon restart.
- Malformed clients are isolated, but one local user can still consume time by repeatedly opening connections.
- Graceful signal handling and stale-socket recovery are not implemented.
