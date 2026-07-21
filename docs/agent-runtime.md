# Agent Runtime and Model Adapter Contract

## Status

Experimental synchronous execution contract. The current implementation provides a deterministic scripted Model Adapter for conformance tests; it does not perform local or remote inference.

## Trust model

Model output is untrusted input. A model session can propose only one of two validated decisions:

- finish with a bounded final text result;
- call one model-visible Tool route with a bounded argument vector.

The model never selects a Capability Tool identifier, approval action, raw handler, executable, filesystem path authority, or network authority. Trusted startup code fixes the route-to-Capability mapping in `ToolCatalog`. `AgentRuntime` owns both the catalog and `ToolExecutionGate`, so it exposes no raw handler or adapter reference to a model session.

Sensitive model requests, final output, decisions, and Task execution input intentionally omit `Debug` and serialization implementations. Public Agent errors use stable categories and discard adapter-specific error values.

## Session contract

`ModelAdapter::start_session` creates one Task-scoped `ModelSession` from:

- the validated Task goal;
- the model-visible Tool route names whose fixed Capability Tool is granted to the Task.

Each model turn receives:

- a monotonic step number bounded by trusted configuration;
- at most the immediately preceding bounded Tool output.

The session boundary prevents conversation state from being reused implicitly between Tasks. A concrete inference adapter remains responsible for bounding its prompt construction, parsing untrusted model bytes through `ModelDecision` constructors, enforcing inference deadlines, and clearing backend state when the session is dropped.

## Execution flow

1. `AgentRuntime` rejects a second Task while one model session is active.
2. `TaskSupervisor::start_execution` records the `queued` to `running` transition before releasing the Task goal to trusted execution code.
3. The Model Adapter creates one isolated session.
4. A final decision records Task success before returning its bounded output.
5. A Tool decision is reconstructed through the trusted catalog and submitted to `ToolExecutionGate`. Unmatched or ungranted routes are not advertised to the model.
6. Capability denial fails the Task without invoking the handler.
7. An approval-required operation and the model session remain retained in memory. Only the exact Approval ID can execute the retained operation and resume the same session; denial, cancellation, or expiration drops it.
8. Invalid decisions, unknown routes, model failures, Tool failures, and step exhaustion fail closed.

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
| Concurrent model sessions per `AgentRuntime` | 1 |
| Approval lifetime requested by Agent runtime | 5 minutes by default, 15 minutes maximum |

Final output must be non-empty UTF-8 without NUL. Tool route identifiers and arguments use the same limits as the Tool Adapter and are validated again by the catalog and private adapter.

## Deterministic adapter

`ScriptedModelAdapter` consumes one pre-validated sequence of `ModelDecision` values and creates at most one session. It is intended only for deterministic contract, lifecycle, Capability, approval, and fail-closed tests. It is not a fallback inference implementation and must not be presented as satisfying the real local model requirement in [DOD-001](definition-of-done.md#dod-001-end-to-end-local-execution).

## Current limitations

- No real local model runtime is integrated.
- No model protocol parser, tokenizer, context-window manager, streaming output, inference timeout, or model artifact identity exists.
- Tool output is retained in memory and only the immediately preceding output is supplied to the next turn.
- Agent execution is synchronous and supports one active Task per runtime instance.
- CPU, RAM, and wall-time Task Budgets are not yet enforced by this layer.
- Tool handlers remain subject to the isolation limits documented in [Tool Adapter](tool-adapter.md) and [Process Adapter](process-adapter.md).
- Agent execution is not exposed through the local API daemon yet.

These limitations keep DOD-001 and the OS-enforcement release gates incomplete.
