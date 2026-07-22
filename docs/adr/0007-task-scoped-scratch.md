# ADR-0007: Allocate fresh Task-ID-scoped sandbox scratch

- Status: Accepted
- Date: 2026-07-23
- Related threats: TM-002, TM-008, TM-009, TM-018
- Related objectives: SEC-003, SEC-008, SEC-009

## Context

ADR-0006 requires one writable host directory mounted at `/workspace`, but its first implementation accepts an arbitrary trusted host path. Reusing a directory across Tasks can expose earlier Tool output, create confused-deputy behavior, and make a sandbox appear Task-bound when only its mount path is configured.

AI OS needs a small allocation boundary before adding immutable root images and general Filesystem Capability mounts. The boundary must not recursively delete attacker-controlled trees during object destruction, and it must not claim to solve descriptor-relative filesystem authorization.

## Decision

Trusted startup code configures `TaskScratchManager` with an existing absolute scratch root.

- The configured root must be a real directory rather than a symlink, must not be `/`, and must grant no group or other permissions.
- The manager records the canonical root device and inode and rejects allocation if that identity or its permissions change.
- Each allocation uses the canonical UUID representation of the exact `TaskId` as one direct child name.
- Allocation uses create-new directory semantics. An existing child, including a symlink, is never opened, emptied, or reused.
- The new directory is created and normalized to mode `0700`, checked to be a direct child of the configured root, and represented by a non-cloneable, non-serializable `TaskScratch` value.
- `BubblewrapProcessToolBuilder::new_for_task` records the scratch device and inode. The builder and handler revalidate the identity and owner-only permissions before use.
- The original path-taking Bubblewrap constructor remains available for backward compatibility with trusted startup code. Only `new_for_task` establishes the Task scratch checks in this decision.
- Dropping a manager, scratch value, builder, or handler does not delete files. Cleanup requires a later explicit lifecycle that first proves the Task process tree is stopped and then safely handles Tool-controlled content.

## Consequences

- Different Tasks receive different empty writable directories, and duplicate allocation for one Task fails closed.
- Scratch paths are derived only from `TaskId`, not from model text or a user-supplied path component.
- Owner-only permissions reduce accidental same-host exposure, while identity checks detect ordinary path replacement before allocation and spawn.
- The manager does not remove the host-side check-to-use race. A future Linux filesystem adapter must use descriptor-relative operations such as `openat2` and bind authorized descriptors to execution.
- Task-ID derivation is not Filesystem Capability derivation. The sandbox still receives no general Capability-selected host mounts.
- Explicit cleanup, quotas, disk accounting, retention, and crash recovery remain future runtime responsibilities.

## Verification

- Unit tests verify Task-derived names, empty `0700` directories, separation between Tasks, and duplicate-allocation rejection.
- Unit tests reject public permissions, a symlink root, configured-root replacement, and Task-directory permission or identity changes.
- The Linux Bubblewrap boundary fixture allocates its writable mount through `TaskScratchManager` and constructs the handler with `new_for_task`.

## Alternatives considered

### Reuse a configured scratch directory

Rejected for Task execution because stale files and cross-Task state would remain ambient authority.

### Delete scratch recursively when `TaskScratch` is dropped

Rejected. Drop can occur before all descendants are reaped, and recursive deletion of Tool-controlled trees requires explicit symlink, mount, race, retention, and recovery rules.

### Derive a directory name from the Task goal

Rejected. Goals are untrusted, sensitive, variable-length model input and must not become host path components or operational metadata.

### Implement descriptor-bound allocation immediately

Deferred. It is the required stronger boundary, but it needs Linux-specific descriptor ownership and mount integration beyond this narrow Task-separation increment.
