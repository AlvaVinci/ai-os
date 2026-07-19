# ADR-0003: Authorize explicit TCP network destinations

- Status: Accepted
- Date: 2026-07-19
- Related threats: TM-008, TM-010, TM-011, TM-017
- Related objectives: SEC-002, SEC-003, SEC-007, SEC-009

## Context

The initial Network Capability allowed exact hostnames or IP addresses but did not identify a transport or port. A future Network Adapter cannot safely translate that grant into an operating-system connection because the same host may expose unrelated services on many ports. Inferring HTTPS or a default port from a Tool name, URL, model output, or approval would broaden authority outside the Task contract.

Network authorization must remain distinct from Tool authorization. A `dependency_scanner` Tool grant, for example, does not grant the connection it may attempt.

The Task schema is carried inside the experimental local API. Changing the Network Capability therefore requires an explicit protocol compatibility boundary.

## Decision

An allowed network resource is one explicit destination with:

- a validated lowercase hostname or IP address;
- the `tcp` transport;
- a non-zero TCP port.

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

Authorization matches host, transport, and port exactly. It does not infer subdomains, resolved IP addresses, redirects, proxies, TLS, application protocol, or additional ports. Duplicate destinations, empty allowlists, invalid hosts, port `0`, unknown transports, and legacy host-only allowlists are rejected.

Only TCP is specified in this version. UDP, listening sockets, Unix sockets, and raw sockets remain denied until their resource and lifecycle semantics receive separate design review.

The approval action remains `network.egress`. Approval is evaluated only after an exact destination Capability matches and cannot add or alter a destination.

The local API advances from Protocol Version 2 to Version 3. Version 3 is the only supported protocol. Version 2 Task submission is rejected explicitly rather than interpreted under new semantics.

## Acceptance criteria

- Given an exact granted host, TCP transport, and port, policy returns allow or approval required according to the Task approval rule.
- Given a different host, subdomain, transport, or port, policy denies with `CAPABILITY_NOT_GRANTED`.
- Given an invalid host, port `0`, or malformed request, policy denies with `INVALID_REQUEST`.
- Given a Tool Capability without the destination Capability, no network authorization is inferred.
- Given Protocol Version 2, the local API returns its stable unsupported-version error and performs no Task operation.
- Public errors and Events do not contain destination values.

## Consequences

- A Network Adapter can receive a deterministic logical destination without guessing a port.
- HTTPS still requires trusted Tool or adapter code to enforce TLS and bind DNS resolution, redirects, proxies, and the actual socket destination.
- Existing host-only Task JSON must migrate to `destinations` and specify `transport` and `port`.
- Example Tasks, benchmark fixtures, tests, CLI documentation, and API examples move to Protocol Version 3 together.

## Alternatives considered

### Infer port 443 from a hostname or Tool

Rejected. Tool identity is not network authority, and inference can silently broaden a grant or misrepresent non-HTTPS traffic.

### Maintain separate host and port lists

Rejected. Their Cartesian product grants combinations the user may not intend.

### Allow arbitrary URL strings

Rejected. URLs combine application protocol, credentials, path, query, fragments, redirects, and hostname parsing into one ambiguous policy input.

### Preserve host-only grants as any-port access

Rejected. This is incompatible with least privilege and cannot be safely enforced by a destination-bound adapter.
