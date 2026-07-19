# Capability Policy

## Status

Experimental pre-execution policy for the Phase 1 runtime. It makes deterministic authorization decisions but does not perform operating-system operations.

## Decision model

`CapabilityPolicy` can only be created from a `TaskSpec` that passes complete validation. Each operation produces one resource-free decision:

- `allow`: the capability is granted and no approval is required.
- `deny`: the request is invalid or the capability is not granted.
- `approval_required`: the capability is granted, but the operation must wait for fresh task-scoped approval.

Capability checks always run before approval checks. An approval requirement never grants a capability that the Task does not already have.

Denials use stable reason codes:

- `INVALID_REQUEST`
- `CAPABILITY_NOT_GRANTED`

Requested paths, hosts, tool names, and action names are not copied into decisions. Operation request types intentionally do not implement debug formatting or serialization.

## Filesystem semantics

- A requested path must be a normalized absolute path below `/`.
- Empty components, `.` components, `..` components, NUL bytes, relative paths, and the filesystem root are rejected.
- A capability path grants access to that exact path and descendants separated by `/`.
- String-prefix siblings do not match. `/workspace/project` does not grant `/workspace/project-private`.
- Read and write are independent. Neither access mode implies the other.
- `filesystem.read` and `filesystem.write` are the corresponding approval action identifiers.

These rules are lexical. Before performing an operation, a filesystem adapter must also enforce the resolved path and prevent symlink escapes, time-of-check/time-of-use races, inherited file-descriptor access, mount changes, and subprocess bypasses.

## Network semantics

- Network access is denied by default.
- Requested hosts must use the same validated lowercase host-name or IP-address format as Task capabilities.
- Matching is exact. Subdomains, schemes, paths, and ports are not inferred.
- Granted egress uses `network.egress` as its approval action identifier.

A network adapter must additionally bind authorization to the actual connection destination and defend against DNS rebinding, redirects, proxies, alternate IP representations, and subprocess bypasses.

## Tool semantics

- Tool names use exact identifier matching against the Task allowlist.
- Every tool request includes an adapter-supplied action identifier.
- The action identifier is matched exactly against `approval.required_for`.
- The tool adapter, not model output, is responsible for assigning the action identifier to an operation.

A tool adapter must validate its structured arguments, avoid shell interpolation, restrict inherited capabilities, and call the policy before every operation, including retries.

## Enforcement boundary

The policy engine answers whether an operation may proceed. It does not make the operation safe by itself. An execution adapter must:

1. construct the request from trusted adapter code;
2. stop immediately on `deny`;
3. pause without side effects on `approval_required`;
4. re-evaluate the capability and consume a fresh scoped approval grant before execution;
5. apply operating-system isolation and resource limits;
6. record a resource-free audit event for the decision.

Linear approval grants now exist in `aios-runtime`, but API, audit-event, lifecycle, and execution-adapter integration remain future work. Until those adapters exist, this module must not be described as complete operating-system capability enforcement.
