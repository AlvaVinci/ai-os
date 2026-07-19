# Tool Adapter

## Status

Experimental bounded in-process adapter. It executes only handlers registered by trusted startup code. It never invokes a shell, searches `PATH`, or starts a process.

## Trust model

Registration creates two separate objects:

- `ToolCatalog`: a read-only mapping from a model-visible route to fixed capability tool and action identifiers;
- `ToolExecutionGate`: the only public execution facade, which owns `ExecutionGate` and the matching private handler adapter.

Operation preparation code can select a registered route and provide arguments. It cannot assign the capability tool or approval action. `ToolOperation` fields are private, and the type does not implement `Clone`, `Debug`, or serialization.

The adapter verifies the route-to-capability mapping again immediately before calling a handler. An operation prepared by a catalog with a different mapping fails with `ScopeMismatch` and does not reach the handler.

## Execution flow

1. Trusted startup code registers a route, capability tool, action, and in-process handler.
2. The builder produces one matching catalog and `ToolExecutionGate`; the raw adapter type is never exposed.
3. The execution facade keeps its `ExecutionGate` and handler registry private, so callers cannot invoke a handler without authorization.
4. The catalog prepares a bounded `ToolOperation` from the selected route and arguments.
5. `ExecutionGate` applies capability policy and approval before invoking the adapter.
6. The adapter revalidates the fixed scope and argument bounds, then passes the argument vector directly to the registered handler.

Arguments remain separate strings. The adapter performs no interpolation or parsing as shell syntax.

## Bounds

| Resource | Default or maximum |
| --- | ---: |
| Registered routes | 256 by default, 4,096 hard maximum |
| Route, tool, or action identifier | 64 bytes |
| Arguments per operation | 64 |
| Bytes per argument | 4,096 |
| Total argument bytes | 65,536 |
| Handler output | 1 MiB |

NUL-containing arguments are rejected. Identifier values use ASCII alphanumeric characters plus `.`, `_`, `:`, and `-`. Errors never include route names, arguments, handler details, or output.

## Security boundary

The Tool Adapter is not an operating-system sandbox. Registered handlers are trusted code and remain responsible for:

- validating argument meaning and combinations;
- avoiding shell invocation and string-built commands;
- applying timeouts and idempotency where side effects are possible;
- preventing direct access to capabilities not represented by the Task;
- keeping sensitive output out of logs and untrusted responses.

Future out-of-process handlers must run under a separate operating-system principal or sandbox, receive no daemon control socket, use explicit executable paths and argument arrays, and enforce CPU, memory, time, file-descriptor, and network limits.
