# Agent Runtime and Model Adapter Contract

## Status

Experimental synchronous execution contract. The current implementation provides a deterministic scripted Model Adapter for conformance tests; it does not perform local or remote inference.

## Trust model

Model output is untrusted input. A model session can propose only one of three validated decisions:

- finish with a bounded final text result;
- call one model-visible Tool route with a bounded argument vector;
- perform one bounded TCP exchange with an explicit destination and request.

The model never selects a Capability Tool identifier, approval action, raw handler, executable, filesystem authority, or new Network authority. Trusted startup code fixes the route-to-Capability mapping in `ToolCatalog`. The Agent advertises only Task-granted destinations supported by its configured IP-only Network gate. `AgentRuntime` owns the catalogs and execution gates, so it exposes no raw handler, adapter, or socket reference to a model session.

Sensitive model requests, final output, decisions, and Task execution input intentionally omit `Debug` and serialization implementations. Public Agent errors use stable categories and discard adapter-specific error values.

## Session contract

`ModelAdapter::start_session` creates one Task-scoped `ModelSession` from:

- the validated Task goal;
- the model-visible Tool route names whose fixed Capability Tool is granted to the Task;
- exact Task Network destinations supported by the configured Network Adapter.

Each model turn receives:

- a monotonic step number bounded by trusted configuration;
- at most the immediately preceding bounded Tool or Network output.

The session boundary prevents conversation state from being reused implicitly between Tasks. A concrete inference adapter remains responsible for bounding its prompt construction, parsing untrusted model bytes through `ModelDecision` constructors, enforcing inference deadlines, and clearing backend state when the session is dropped.

## Execution flow

1. `AgentRuntime` rejects a second Task while one model session is active.
2. `TaskSupervisor::start_execution` records the `queued` to `running` transition before releasing the Task goal to trusted execution code.
3. The Model Adapter creates one isolated session.
4. A final decision records Task success before returning its bounded output.
5. Before and after every synchronous model turn, the Agent checks remaining wall time against the monotonic Task start. Model or approval waits do not reset the allowance.
6. A Tool decision is reconstructed through the trusted Tool catalog and submitted to `ToolExecutionGate`. Unmatched or ungranted routes are not advertised to the model.
7. A TCP decision is reconstructed through the trusted Network catalog and submitted to `NetworkExecutionGate`. Missing gates, unsupported destinations, and ungranted destinations fail without connecting.
8. Capability denial fails the Task without invoking a handler or opening a connection.
9. An approval-required operation, its owning adapter kind, and the model session remain retained in memory. Only the exact Approval ID can execute the retained operation and resume the same session; denial, cancellation, expiration, or wall-time exhaustion drops it. The existing expiry poll also checks the Task deadline while approval is idle.
10. A typed Tool or Network Budget failure drops the model session and atomically records `TaskFailed { code: BUDGET_EXCEEDED }` before the terminal state transition.
11. Invalid decisions, unknown routes, model failures, other adapter failures, and step exhaustion fail closed.

Audit persistence failure never authorizes a Tool operation. If a terminal state cannot be recorded, the Agent session is dropped and the caller receives a resource-free supervision failure rather than replaying a consumed model decision.

## Bounds

| Resource | Limit |
| --- | ---: |
| Model turns per Task | 16 by default, 64 maximum |
| Final output | 1 MiB |
| Tool route identifier | 64 bytes |
| Arguments per Tool operation | 64 |
| Bytes per argument | 4,096 |
| Total argument bytes | 65,536 |
| TCP request | 64 KiB |
| TCP response | 1 MiB |
| Concurrent model sessions per `AgentRuntime` | 1 |
| Approval lifetime requested by Agent runtime | 5 minutes by default, 15 minutes maximum |

Final output must be non-empty UTF-8 without NUL. Tool route identifiers and arguments use the same limits as the Tool Adapter. TCP host, port, request, and response values use the Network Adapter bounds. Every operation is validated again by its Agent-owned catalog and private adapter.

## Deterministic adapter

`ScriptedModelAdapter` consumes one pre-validated sequence of `ModelDecision` values and creates at most one session. It is intended only for deterministic contract, lifecycle, Capability, approval, and fail-closed tests. It is not a fallback inference implementation and must not be presented as satisfying the real local model requirement in [DOD-001](definition-of-done.md#dod-001-end-to-end-local-execution).

## Current limitations

- No real local model runtime is integrated.
- No model protocol parser, tokenizer, context-window manager, streaming output, inference timeout, or model artifact identity exists.
- Operation output is retained in memory and only the immediately preceding bounded output is supplied to the next turn.
- Agent execution is synchronous and supports one active Task per runtime instance.
- The runtime owner must poll `expire` to observe wall-time exhaustion while an Agent is idle waiting for approval; the local daemon does not schedule this monitor yet.
- The Agent propagates the validated Task Budget and terminates on a typed Tool or Network Budget failure. Current OS enforcement covers Task-bound Process Tools and bounded IP-only Agent TCP exchanges, not model inference.
- Tool handlers remain subject to the isolation limits documented in [Tool Adapter](tool-adapter.md) and [Process Adapter](process-adapter.md).
- Filesystem operations are not routed through the Agent yet.
- Agent execution is not exposed through the local API daemon yet.

These limitations keep DOD-001, complete DOD-002 and DOD-005 coverage, and the OS-enforcement release gates incomplete. See [ADR-0011](adr/0011-bind-task-budget-to-process-execution.md) and [ADR-0017](adr/0017-route-agent-network-proposals-through-network-gate.md).
