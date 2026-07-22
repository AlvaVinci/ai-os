# ADR-0005: Stabilize the Local API from Protocol Version 4

## Status

Accepted

## Context

The Local API has used explicit versions while its schema evolved, but Versions 1 through 4 supported only one exact version and carried no durable compatibility promise. DOD-007 requires a published support window, incompatible-change policy, and migration procedure. Without one, clients cannot distinguish safe additive evolution from a schema change that may reinterpret Task authority or fail to decode audit Events.

## Decision

Protocol Version 4 is the first stable Local API schema contract. The current supported window is `4..=4`, published in code and the health response.

Version 4 requests remain strict and reject unknown fields. Clients must ignore unknown fields added to existing response objects. New request fields or methods, tagged variants, error codes, required response fields, reduced limits, framing changes, and security-relevant semantic changes require a new protocol version.

A future incompatible Version `N` must overlap with `N - 1` for the minor release that introduces it. The bundled CLI must negotiate the highest common version without retrying side-effecting requests. The predecessor may be removed no earlier than the next minor release.

A same-version security fix may narrow unsafe input only when it preserves unambiguous decoding, fails closed, is documented, and adds regression evidence. It cannot broaden authority or hide a schema change.

The complete normative rules and procedures are in [Local API Compatibility Contract](../api-compatibility.md).

## Consequences

- Version 4 JSON behavior becomes review-protected by golden fixtures.
- Operators can inspect the supported window through `health`.
- Version 3-to-4 remains a coordinated upgrade because Version 3 predates this contract.
- Future incompatible versions require temporary multi-version server and CLI logic.
- Stable schema does not imply production readiness, OS enforcement, or Rust crate API stability.

## Alternatives considered

### Keep every protocol experimental until v1.0

Rejected. v0.1 requires a dependable local integration boundary and an auditable migration policy.

### Support only the newest version forever

Rejected. It forces lockstep upgrades and makes rollback unsafe once external clients exist.

### Accept unknown request fields for additive evolution

Rejected. Older daemons could silently ignore security-relevant client intent. Request evolution requires an explicit version.

### Use semantic-version strings in every request

Rejected. Integer protocol epochs are sufficient for wire compatibility; product and crate versions remain separate.

## Related requirements

- DOD-007: API and operational reliability
- ADR-0003: explicit network destinations and Version 3
- ADR-0004: restart failure Event and Version 4
