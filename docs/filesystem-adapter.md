# Filesystem Adapter

## Status

Experimental Linux-only create-new writer. This is a narrow operating-system enforcement increment, not a general filesystem API.

## Contract

`aios-adapter-filesystem` creates one new regular file beneath a trusted source root and writes at most 1 MiB to it.

- Trusted integration code constructs `FilesystemExecutionGate` with an absolute source root; the raw adapter remains private.
- A normalized absolute Capability path is interpreted relative to that root.
- `FilesystemCatalog` validates the complete path and byte payload before authorization.
- `FilesystemExecutionGate` requires an exact `write` Capability and any configured `filesystem.write` approval.
- Approval retains the exact path and bytes without creating the file early.
- A successful operation returns only the number of bytes written. It never returns file contents or a descriptor.

The adapter is not exposed as a Tool handler. A Tool Capability does not substitute for a Filesystem Capability.

## Linux enforcement

The adapter opens and retains the trusted root directory when it is constructed. Each operation uses Linux `openat2` relative to that descriptor with:

- `O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC`;
- owner-only mode `0600`;
- `RESOLVE_BENEATH`;
- `RESOLVE_NO_MAGICLINKS`;
- `RESOLVE_NO_SYMLINKS`;
- `RESOLVE_NO_XDEV`.

These flags make the operation create-only and write-only. Existing destinations are never opened or overwritten, symlink and mount escapes fail closed, and replacing the configured root path after construction does not redirect the operation.

Parent directories must already exist. Trusted provisioning is responsible for creating the source-root layout and preventing untrusted modification of the root itself.

## Bounds and error handling

- Paths are limited to 4,096 bytes and must follow the same normalized absolute syntax as `TaskSpec`.
- Contents are limited to 1 MiB.
- Empty files are allowed.
- Errors expose only stable categories and omit paths, contents, and operating-system details.
- The adapter calls `sync_data` before reporting success.

If writing or syncing fails after creation, a partial new file can remain. The adapter deliberately does not unlink by path because another actor could replace that path before cleanup. Callers must treat the operation as non-idempotent and inspect or remove partial output through a separately authorized, descriptor-safe workflow.

## Non-goals

This increment does not implement:

- reading or returning file contents;
- overwriting, appending, renaming, deleting, linking, or metadata changes;
- directory creation;
- atomic publish by rename;
- per-operation wall-time or disk-quota enforcement;
- access for an untrusted subprocess;
- Agent or Local API routing to the adapter;
- non-Linux execution.

The Process Adapter still rejects write Capability mounts because a writable bind mount would also grant read access. A future subprocess write boundary needs a trusted post-mount launcher or an equivalent kernel-enforced design.

## Verification

Portable unit tests cover normalized path and byte bounds. Linux tests cover:

- exact write Capability execution and owner-only file mode;
- approval without early side effects;
- denial for prefix siblings and read-only Capabilities;
- no overwrite of an existing destination;
- root-path replacement without authority redirection;
- symlink escape rejection;
- Linux cross-target compilation.

See [ADR-0014](adr/0014-create-new-write-filesystem-adapter.md), the [Capability model](capability-model.md), and the [Threat model](threat-model.md).
