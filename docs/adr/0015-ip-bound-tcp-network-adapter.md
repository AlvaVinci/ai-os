# ADR-0015: Bind the first Network Adapter to explicit IP destinations

- Status: Accepted
- Date: 2026-07-31
- Related threats: TM-004, TM-006, TM-008, TM-010, TM-011, TM-016, TM-017
- Related objectives: SEC-002, SEC-003, SEC-006, SEC-007, SEC-008, SEC-009

## Context

The Task schema authorizes an exact host string, TCP transport, and port. Policy evaluation alone does not prove which operating-system peer receives bytes. Hostname support also requires explicit DNS rebinding, address filtering, TLS identity, proxy, redirect, and multi-address semantics.

Implementing those semantics implicitly through `ToSocketAddrs`, an HTTP client, environment proxy configuration, or a Tool process could broaden authority beyond the accepted Task. Exposing a connected socket would also delegate reusable network authority outside `ExecutionGate`.

The first enforcement increment needs to bind authorization to an actual socket without claiming that the complete hostname and HTTPS boundary is solved.

## Decision

- Add `aios-adapter-network` behind `ExecutionGate`.
- Initially accept only explicit IPv4 or IPv6 address strings. Hostname Capabilities fail closed as unsupported.
- Reject port `0`, unspecified addresses, multicast addresses, IPv4 broadcast, host strings over 253 bytes, and request bodies over 64 KiB.
- Support one operation: a direct one-shot TCP exchange with at most a 1 MiB response.
- Retain the exact IP string, port, and request bytes privately across approval.
- Request the exact TCP Network Capability and `network.egress` approval action.
- Construct one `SocketAddr` and call `TcpStream::connect_timeout` for that address rather than a multi-address resolver.
- Apply bounded read and write timeouts, verify `peer_addr` exactly, close the write half after the request, read a bounded response, and close the socket.
- Keep the raw adapter and socket private. Never return, clone, serialize, or delegate live socket authority.
- Do not interpret HTTP, DNS, redirects, proxies, TLS, or another application protocol.
- Return only stable redacted failure categories.

## Consequences

- Authorization and the actual connected IP address, TCP transport, and port are joined before bytes are sent.
- DNS rebinding and proxy or redirect substitution are absent from this narrow operation because no DNS, proxy, or application redirect is used.
- Task hostname Capabilities remain policy-valid but cannot execute through this adapter.
- Local, private, link-local, or otherwise sensitive IP ranges remain available only when the Task explicitly grants that exact address and port. This increment does not invent a separate address-class policy.
- Approval creates no connection and sends no bytes before the exact operation is approved.
- A remote peer can receive a partial or complete request before a later write, read, timeout, or size failure. Remote effects cannot be rolled back.
- Socket timeouts are bounded by trusted configuration but not yet by the Task's remaining wall-time Budget.
- This does not provide HTTPS, destination authority to a Process Tool, inbound networking, cancellation of an in-flight synchronous operation, or complete DOD-002 Network enforcement.

## Verification

- Validation tests cover IP versions, hostnames, special addresses, port, size, and timeout bounds.
- Loopback integration tests verify exact destination execution, approval without early connection, denial without connection, request/response transfer, and oversized response rejection.
- Workspace tests and Clippy run on the supported Linux CI target.

## Alternatives considered

### Resolve hostnames once and connect to a returned address

Deferred. A safe contract must define accepted address classes, multiple results, resolver trust, TTL and cache behavior, rebinding, approval presentation, and TLS hostname identity.

### Use an HTTP client

Deferred. HTTP adds URL parsing, credentials, redirects, proxies, decompression, response framing, and TLS configuration that require separate bounded semantics.

### Return the connected `TcpStream`

Rejected. A live socket is reusable authority and would let caller code perform unbounded or unapproved I/O outside the exact retained operation.

### Run networking inside the Process Adapter

Deferred. The current Bubblewrap backend deliberately has no network namespace connectivity. Destination-scoped Process networking requires a broker or equivalent kernel boundary that cannot be bypassed by the Tool.
