# Network Adapter

## Status

Experimental bounded TCP exchange adapter. It binds one narrow Network Capability subset to an operating-system socket but is not a general or release-complete network stack.

## Contract

`aios-adapter-network` performs one direct TCP exchange:

1. connect to one explicit IP address and port;
2. verify the connected peer is the same `SocketAddr`;
3. write at most 64 KiB;
4. close the socket's write half;
5. read at most 1 MiB;
6. close the socket without exposing it.

Trusted integration code constructs `NetworkExecutionGate` with a nonzero timeout of at most 30 seconds. `NetworkCatalog` validates the complete destination and request bytes before authorization. The gate binds the operation to the running Task's original execution context, then requires the exact Task TCP destination Capability and any configured `network.egress` approval.

The raw adapter and socket remain private. Approval retains the exact destination, request bytes, and Task context without connecting early or resetting the Task wall-time. Returned response bytes are bounded, untrusted, non-debuggable, and non-serializable.

## Destination binding

This increment accepts only explicit IPv4 and IPv6 address strings. It constructs one `SocketAddr`, calls [`TcpStream::connect_timeout`](https://doc.rust-lang.org/stable/std/net/struct.TcpStream.html#method.connect_timeout) for that single address, and verifies `peer_addr` before sending bytes.

The adapter rejects:

- hostnames;
- port `0`;
- unspecified, multicast, and IPv4 broadcast addresses;
- oversized host or request values;
- a connected peer different from the authorized address and port.

Hostnames remain valid in the Task schema for future adapters, but this adapter fails them closed. It performs no DNS lookup, so it cannot silently re-resolve or reinterpret a hostname.

## Protocol boundary

The adapter is raw TCP and deliberately does not:

- interpret HTTP or another application protocol;
- use environment or system proxies;
- follow redirects;
- negotiate or validate TLS;
- infer port `443`, a hostname, or another destination;
- listen for inbound connections;
- return or delegate a live socket;
- grant network authority to a Tool or subprocess.

A future hostname or HTTPS adapter must define DNS pinning, address policy, TLS identity, proxy and redirect behavior, and response semantics before use.

## Bounds and failure semantics

- Destination host strings are limited to 253 bytes.
- Request bytes are limited to 64 KiB.
- Response bytes are limited to 1 MiB.
- Before each blocking stage, the adapter checks the remaining Task wall-time.
- Connect, read, and write use the smaller of the configured timeout and the remaining Task wall-time.
- Errors expose stable categories without destination, request, response, or operating-system details.

Approval time consumes the original Task Budget. An operation approved after its Task deadline returns `BudgetExceeded` without connecting. A stalled in-flight socket operation is bounded by the remaining Task wall-time, subject to operating-system scheduling granularity.

A write or later deadline or I/O failure can occur after the remote peer received a partial or complete request. The adapter cannot roll back remote side effects, so exchanges are non-idempotent unless the application protocol makes them idempotent.

## Verification

Tests cover:

- IPv4 and IPv6 operation validation;
- hostname, unsafe-address, port, size, and timeout rejection;
- exact IP-and-port Capability execution;
- no connection before approval;
- no connection when approval outlives the original Task wall-time;
- Task wall-time enforcement for a stalled in-flight response;
- no connection after Capability denial;
- exact request and bounded response transfer;
- oversized response rejection.

See [ADR-0015](adr/0015-ip-bound-tcp-network-adapter.md), [ADR-0016](adr/0016-bind-resource-adapters-to-task-wall-time.md), the [Capability model](capability-model.md), and the [Threat model](threat-model.md).
