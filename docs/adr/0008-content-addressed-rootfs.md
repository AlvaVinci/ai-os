# ADR-0008: Verify a content-addressed sealed minimal rootfs

- Status: Accepted
- Date: 2026-07-24
- Related threats: TM-002, TM-003, TM-008, TM-009, TM-020
- Related objectives: SEC-006, SEC-008, SEC-009

## Context

ADR-0006 requires a prepared minimal root filesystem, but a read-only Bubblewrap bind mount only removes writes from the sandbox view. The same host principal can still replace or modify the source tree before the mount. A mutable, unmeasured directory therefore cannot provide a stable Tool runtime identity.

The first rootfs must be easy to construct without root privileges, contain no package manager or ambient host data, and be reproducible across hosts. It must also fail closed on malformed trees and boundedly recheck content before execution. Linux mechanisms such as fs-verity, EROFS, or dm-verity are stronger but require a larger provisioning and mount lifecycle.

## Decision

AI OS introduces a content-addressed sealed staging tree as the first minimal rootfs artifact.

- `aios-rootfs-build` accepts one trusted absolute static BusyBox executable and one new absolute output path.
- The builder uses create-new semantics and never overwrites, removes, or reuses an existing output path.
- The output contains exactly the supplied bytes at `/bin/busybox` plus empty `/proc`, `/dev`, `/tmp`, and `/workspace` mount points. Every output entry has all write bits removed.
- The builder prints a canonical lowercase SHA-256 tree digest. The hash input is domain separated and versioned and includes sorted relative path bytes, entry kind, permission mode, file length, and file contents. It excludes timestamps and host user/group ownership.
- A failed build may leave a partial directory. Provisioning must inspect and explicitly discard it rather than relying on automatic recursive cleanup.
- The expected digest is stored in reviewed trusted deployment configuration outside the rootfs. Measuring and trusting the current tree during each startup is not an integrity check.
- `VerifiedRootFilesystem` requires the expected digest and rejects writable entries, symlinks, hard-linked files, sockets, devices, other special entries, missing mount points, identity replacement, digest mismatch, and configured size limits.
- Rootfs traversal is bounded to 4,096 entries, 256 MiB per file, and 512 MiB total file content.
- `BubblewrapProcessToolBuilder::new_for_verified_task` binds the verified root state and Task scratch state. It rechecks the root device/inode and complete digest during build and before every spawn.
- Existing path-taking constructors remain for backward compatibility with trusted code. They must not be described as verified-root execution.

RustCrypto `sha2` is the only new third-party dependency for this decision. It provides the incremental SHA-256 implementation; no shell command or host digest utility enters the verification path.

## Consequences

- The minimal Tool runtime has a reproducible content identity independent of file timestamps and host ownership.
- The rootfs contains no separate package manager, dynamic loader, host configuration, credential path, daemon socket, or unrelated binary beyond the selected static BusyBox implementation and its compiled applets.
- Any content or permission change detected during verification fails with the stable redacted `InvalidSandbox` category.
- Full-tree hashing before every spawn adds I/O proportional to rootfs size. The strict minimal image and byte bounds make this acceptable for the initial backend; later verified immutable storage can replace repeated hashing.
- A digest proves content identity, not source provenance. Release artifact signing, builder provenance, dependency audit, and trusted BusyBox acquisition remain separate requirements.
- Sealing permission bits and repeated hashing narrow accidental or ordinary replacement but do not eliminate the host-side check-to-use race. This decision does not claim OS-backed immutability.

## Verification

- Unit tests prove deterministic digesting, canonical digest parsing, content-change detection, create-new output, sealed permissions, and reproducible builds.
- Unit tests reject writable or replaced roots, root and entry symlinks, missing mount points, and oversized files.
- The Linux Bubblewrap boundary fixture builds the minimal rootfs, opens it through `VerifiedRootFilesystem`, and constructs the sandbox through `new_for_verified_task`.
- Workspace formatting, tests, and Clippy remain mandatory in the Linux workflow.

## Alternatives considered

### Trust a read-only bind mount without a digest

Rejected. The mount is read-only only inside the sandbox and does not identify or protect the host source tree.

### Store a generated manifest inside the rootfs

Rejected as a trust anchor. An attacker able to replace the tree could replace both content and manifest. The expected digest must remain outside the measured tree in trusted configuration.

### Package a prebuilt BusyBox binary in this repository

Rejected. Committing an opaque platform binary would add repository size, update, license-notice, and provenance concerns. Trusted provisioning supplies the static executable and pins the resulting digest.

### Require fs-verity or a read-only filesystem image immediately

Deferred. These are preferred stronger backends, but they require Linux-specific artifact signing, mount, lifecycle, and host setup that exceed this incremental boundary.

### Reuse a general Linux distribution rootfs

Rejected for the initial backend. A distribution tree expands the executable and configuration attack surface, makes per-spawn hashing expensive, and is unnecessary for the current BusyBox boundary probes.
