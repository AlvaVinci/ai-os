# Architecture Decision Records

Architecture Decision Records capture security or compatibility choices that constrain future implementation.

Statuses:

- **Proposed**: under review and not binding;
- **Accepted**: current project direction;
- **Superseded**: replaced by a later ADR;
- **Rejected**: considered but not selected.

Index:

- [ADR-0001: Separate approval authority from untrusted execution principals](0001-separate-approval-principal.md)
- [ADR-0002: Prohibit shell interpretation in Tool execution](0002-no-shell-tool-execution.md)
- [ADR-0003: Authorize explicit TCP network destinations](0003-explicit-network-destinations.md)
- [ADR-0004: Fail interrupted Tasks instead of restoring execution authority](0004-non-resumable-restart.md)
- [ADR-0005: Stabilize the Local API from Protocol Version 4](0005-stable-local-api.md)

New ADRs should state context, decision, consequences, alternatives, and related security requirements. Existing ADRs are immutable except for status and supersession links; changed decisions receive a new record.
