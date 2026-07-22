# Local API Compatibility Contract

## Status

Protocol Version 4 is the first stable Local API schema contract. The daemon and its security boundaries remain pre-release; schema stability does not mean that AI OS is production-ready or OS-enforced.

This contract applies from the release commit that adopts it. Protocol Versions 1 through 3 were experimental and are outside the support window.

## Goal

Clients and operators must be able to determine:

- which protocol versions a daemon supports;
- which changes preserve a protocol version;
- when a new version is required;
- how long an older stable version remains supported;
- how to upgrade or roll back without silently reinterpreting authority.

## Non-goals

- Stabilizing the Rust crate API or command-line text output.
- Stabilizing the SQLite schema or making Event storage portable between arbitrary releases.
- Allowing protocol negotiation to weaken Capability, approval, privacy, or validation rules.
- Supporting Protocol Versions 1 through 3.

## Terms

- **Protocol version**: the integer in every Local API request and response envelope.
- **Current version**: the version emitted by the current `aiosctl` and used by new requests.
- **Supported window**: the inclusive minimum and maximum versions accepted by a daemon release.
- **Compatible change**: a change that every conforming client and server for the same protocol version can process safely.
- **Incompatible change**: a change that can alter interpretation, reject previously conforming peers, or make a conforming peer unable to decode a message.

## Current support window

| Property | Value |
| --- | ---: |
| Current protocol | 4 |
| Minimum supported protocol | 4 |
| Maximum supported protocol | 4 |

The code constants `PROTOCOL_VERSION`, `MIN_SUPPORTED_PROTOCOL_VERSION`, and `MAX_SUPPORTED_PROTOCOL_VERSION` are normative and are covered by contract tests.

The Version 4 health response publishes the same window:

```json
{
  "protocol_version": 4,
  "status": "ok",
  "result": {
    "type": "healthy",
    "supported_protocol_versions": {
      "minimum": 4,
      "maximum": 4
    }
  }
}
```

An unsupported request receives `UNSUPPORTED_PROTOCOL_VERSION`. The response envelope uses the daemon's current protocol version so an operator can identify the installed server. Unsupported input is never reinterpreted as another version.

## Version 4 wire rules

- Every request and response contains `protocol_version`.
- Requests reject missing fields, duplicate fields, unknown fields, unknown methods, invalid tags, `null` for required values, empty frames, oversized frames, and values outside published bounds.
- Successful responses use the requested supported version. Version 4 is currently the only supported version.
- Version selection is per request and creates no mutable negotiation session; concurrent clients cannot change the daemon's published window.
- Clients must ignore unknown fields added to existing response objects.
- Clients must not assume that unknown tagged result, Event, state, disposition, or error-code variants are safe to reinterpret.
- Error messages are descriptive but not stable machine interfaces; clients branch only on stable codes.
- Goals, Capability values, Tool arguments, secrets, complete model output, and private reasoning remain excluded from default responses and Events.

## Change classification

The following changes may remain within Version 4:

- documentation clarification that does not change behavior;
- a bug fix that makes implementation match the published Version 4 contract;
- adding an optional field to an existing response object, because conforming clients ignore unknown response fields;
- increasing a numeric limit without changing field meaning;
- rejecting an input that is unsafe despite previously being accepted, under the security exception below.

The following changes require a new protocol version:

- adding, removing, or renaming a request method or request field;
- changing a field between required and optional, changing its type, tag, default, or meaning;
- adding or removing a tagged response result, Task state, Event type, disposition, or error code;
- removing or renaming a response field, or adding a required response field;
- reducing a published request, collection, output, or pagination limit;
- changing authorization, approval, idempotency, restart, privacy, or state-transition semantics;
- changing framing or encoding.

Rust source compatibility is separate. Internal types may change without a protocol bump only when the Version 4 JSON behavior remains identical.

## Security exception

A release may reject previously accepted input within the same protocol version when continued acceptance creates a concrete security, privacy, corruption, or availability risk. The release must:

1. preserve fail-closed behavior and stable error categories where possible;
2. document the affected input and migration in release notes or a security advisory;
3. add a regression test;
4. avoid broadening authority or silently changing input meaning.

If safe decoding or unambiguous interpretation requires a schema change, the protocol version must still increase.

## Future version lifecycle

When an incompatible Version `N` is introduced after Version 4:

1. the introducing minor release must support both `N - 1` and `N`;
2. its health response must publish a window containing both versions;
3. the bundled CLI must select the highest mutually supported version without retrying a request that may have side effects;
4. migration documentation and Version `N` contract fixtures must ship in the same release;
5. Version `N - 1` may be removed no earlier than the next minor release.

An urgent security release may shorten this overlap only when continuing support is unsafe. It must publish the reason, coordinated upgrade steps, and rollback constraints.

Version 4 has no supported predecessor because Versions 1 through 3 were explicitly experimental.

## Migration procedure

### Protocol Versions 1-3 to Version 4

There is no compatibility bridge. Stop the old daemon, install `aiosd` and `aiosctl` from the same release, then restart and run `aiosctl health`. Existing SQLite Events remain subject to the release's separate storage compatibility rules; never point two daemon versions at the same database concurrently.

### Future overlapping versions

1. Read the release notes and confirm the old and new supported windows overlap.
2. Stop Task submission and allow active work to finish or cancel it explicitly.
3. Stop the daemon and create an owner-only backup of the Event database.
4. Upgrade the CLI to a release that still supports the running daemon.
5. Upgrade and restart the daemon.
6. Run `aiosctl health` and verify the expected minimum and maximum versions.
7. Run one bounded read-only workload before restoring normal submissions.

Do not replay a failed non-idempotent request merely because a connection closed. Inspect the Task and Events first.

## Rollback procedure

1. Stop submissions and the daemon.
2. Confirm that the target older daemon supports a protocol still supported by the installed CLI.
3. Follow any SQLite schema rollback instructions from that release. Restore the owner-only backup when downgrade is unsupported.
4. Install matching daemon and CLI binaries.
5. Start the daemon, run `aiosctl health`, and inspect interrupted Tasks and Events.

Rollback must not restore pending approval grants, retained Tool operations, or model sessions from audit Events.

## Acceptance criteria

1. Given a Version 4 health request, the response matches the published golden fixture and reports support window `4..=4`.
2. Given a request below or above the window, the daemon returns `UNSUPPORTED_PROTOCOL_VERSION` without processing the method body.
3. Given an unknown Version 4 request field, decoding fails deterministically as `INVALID_REQUEST`.
4. Given additive unknown fields on an existing Version 4 response object, a conforming Version 4 client can decode the known fields.
5. Given a proposed incompatible schema or semantic change, the pull request adds a new protocol version, contract fixtures, migration steps, and an ADR.
6. Given a security exception, the release documents the narrowed behavior and includes a regression test.

## Observability and privacy

Health exposes only version integers and contains no Task data. Unsupported-version errors contain no request body, method, identifiers, paths, destinations, or secrets. Operators should record daemon and CLI release identities alongside protocol versions when collecting release evidence.

## Trade-offs

- A one-version window is simple today but requires coordinated Version 3-to-4 upgrades.
- Requiring one overlapping minor release adds implementation cost to future incompatible changes but prevents forced lockstep upgrades.
- Strict requests detect mistakes early; additive response fields provide limited evolution without weakening input validation.
- The security exception permits urgent tightening but cannot be used to broaden authority or avoid a necessary version bump.
