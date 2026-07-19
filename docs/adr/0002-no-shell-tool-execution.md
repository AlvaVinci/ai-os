# ADR-0002: Prohibit shell interpretation in Tool execution

- Status: Accepted
- Date: 2026-07-19
- Related threats: TM-004, TM-005, TM-006, TM-009, TM-016, TM-017
- Related objectives: SEC-001, SEC-007, SEC-008, SEC-009

## Context

Tool arguments originate directly or indirectly from model output and retrieved content. Constructing a command string and passing it to a shell would combine data and control syntax, expand environment state, and make the actual executable and arguments difficult to bind to Capability approval.

The first Tool Adapter executes trusted in-process handlers. Future adapters may need to run existing developer tools as child processes.

## Decision

AI OS Tool adapters must not interpret model-controlled text as shell syntax.

- The in-process adapter passes a bounded `Vec<String>` directly to a registered handler.
- Trusted registration fixes the Capability Tool and action; model input cannot assign them.
- Future process adapters must use an explicit executable identity and argument vector.
- `PATH` search, command strings, shell startup, environment interpolation, and implicit redirection are prohibited at the enforcement layer.
- Child environments and inherited file descriptors must use explicit allowlists.
- Every retry must pass through Capability and approval checks and follow declared idempotency semantics.

## Consequences

- Shell features such as pipelines, globbing, redirection, and substitution require separately registered typed operations rather than free-form command text.
- Existing CLI tools can remain compatible through explicit executable configuration and structured arguments.
- Tool-specific handlers remain responsible for semantic argument validation.
- The process adapter can bind audit and approval to the actual executable and argument object.

## Alternatives considered

### Escape model output and run a shell

Rejected. Correct escaping is context-dependent and does not address environment expansion, redirection, shell configuration, or executable identity.

### Allow a shell only after human approval

Rejected. Approval does not create a missing Capability and cannot make ambiguous free-form execution safe.

### Maintain a denylist of unsafe command fragments

Rejected. Denylists are incomplete and conflict with deterministic structured enforcement.
