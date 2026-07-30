# ADR-0014: Enforce write-only Capabilities with create-new filesystem operations

- Status: Accepted
- Date: 2026-07-31
- Related threats: TM-006, TM-007, TM-010, TM-016
- Related objectives: SEC-002, SEC-006, SEC-007, SEC-008, SEC-009

## Context

The Capability contract defines `read` and `write` independently. A writable bind mount inside the current Bubblewrap Process boundary would also allow the Tool to read existing content, so it cannot implement a write-only Capability.

[Linux Landlock](https://docs.kernel.org/userspace-api/landlock.html) can distinguish file read and write access, but applying it to the current sandbox requires a trusted launcher inside the completed mount namespace. The current minimal root filesystem contains no such launcher. Adding one and changing the Process launch protocol is larger than the smallest safe filesystem enforcement increment.

AI OS still needs evidence that a write Capability can be joined to an actual kernel operation without widening it to read authority or relying on a path check followed by an unrestricted open.

## Decision

- Add a dedicated Linux-only `aios-adapter-filesystem` crate behind `ExecutionGate`.
- Initially support exactly one operation: create one new regular file and write bounded bytes.
- Validate the complete normalized absolute path and at most 1 MiB of contents before authorization.
- Request `CapabilityRequest::File` with exact `write` access. A Tool Capability never authorizes this adapter.
- Retain the complete path and contents privately across approval. Sensitive operation types omit `Clone`, `Debug`, and serialization.
- Open and retain one trusted source-root descriptor during adapter construction.
- Resolve every destination from that descriptor with Linux `openat2`, `O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC`, owner-only mode, and `RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS | RESOLVE_NO_SYMLINKS | RESOLVE_NO_XDEV`.
- Never expose file contents, the opened descriptor, the raw adapter, or operating-system error details.
- Flush file data before reporting success.
- Do not remove a partially written file after a write or sync failure. Path-based cleanup could race with replacement and delete a different object.

## Consequences

- `write` does not imply `read`: the adapter never opens an existing destination and has no read operation.
- Existing files, symlink final components, symlink parents, traversal, magic links, and mount crossings fail closed.
- Replacing the configured source-root path after construction does not redirect later writes.
- Trusted provisioning must create parent directories and protect the source root.
- The operation is create-only and non-idempotent. Retrying the same destination fails rather than overwriting it.
- A write or sync error can leave a partial owner-only file for a separately authorized cleanup workflow.
- This does not grant write authority to an untrusted Process Tool, complete general filesystem semantics, enforce disk quotas, or integrate the adapter with Agent routing.
- Non-Linux construction fails as unsupported.

## Verification

- Portable tests cover normalized path syntax and payload bounds.
- Linux tests verify exact Capability and approval behavior, no early write, owner-only mode, no overwrite, prefix-sibling and read-only denial, symlink escape rejection, and root-path replacement.
- Linux cross-target compilation covers the descriptor-relative implementation.
- Workspace formatting, tests, and lint checks remain required in CI.

## Alternatives considered

### Mount a writable directory into Bubblewrap

Rejected. A read-write mount would silently add read authority and violate the accepted Capability.

### Apply Landlock in the daemon before Bubblewrap

Rejected. Landlock restrictions are inherited and would prevent the trusted launcher from completing the sandbox mount topology. A future post-mount launcher can revisit this design.

### Open by canonical host path

Rejected. Canonicalization followed by a later path open leaves replacement and symlink races. The operation must start from the retained root descriptor.

### Overwrite or atomically replace an existing file

Rejected for this increment. Replacement requires explicit semantics for reading metadata, rename authority, hard links, crash recovery, and cleanup.

### Delete a partial file on write failure

Rejected. Reopening or unlinking the logical path after failure could target a replacement created by another actor.
