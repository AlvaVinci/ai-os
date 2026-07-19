# Developer Workloads and Benchmarks

## Status

Version 0.1 defines the first representative developer workloads, security gates, metrics, and reporting protocol. The initial execution workloads target Rust repositories. The published Task fixtures are schema-tested. End-to-end quality and performance baselines remain pending until filesystem, network, and model adapters can execute them.

## Workload set

| ID | Fixture | User outcome | Capability boundary | Approval boundary |
| --- | --- | --- | --- | --- |
| DEV-001 | [Repository investigation](workloads/repository-investigation.json) | Identify a defect cause and cite evidence without changing the repository. | Read `/workspace/project`; `source_search`; network denied. | None because every permitted operation is read-only and local. |
| DEV-002 | [Test-failure diagnosis](workloads/test-failure-diagnosis.json) | Reproduce a failure and report a minimal fix without changing source files. | Read project; write only `/workspace/project/target`; `test_runner`; network denied. | `test.run` before executing repository code and `filesystem.write` before build-output writes. |
| DEV-003 | [Dependency advisory review](workloads/dependency-advisory-review.json) | Report dependencies affected by an approved advisory source. | Read project; `dependency_scanner`; egress only to `api.osv.dev` on TCP port 443. | `network.egress` before any request leaves the device. |

The fixtures are canonical `TaskSpec` templates. Their identifiers include a version suffix because changing a goal, Capability, approval boundary, or budget changes the benchmark contract. Each measured trial must append a unique run identifier to `idempotency_key`; every other Task field must match the selected fixture exactly. Reports record both the fixture digest and the submitted Task digest.

The DEV-003 destination follows the [official OSV API quickstart](https://google.github.io/osv.dev/quickstart/) and grants only TCP port 443. End-to-end execution remains blocked until a Network Adapter binds this logical destination to the actual socket and enforces DNS, redirect, proxy, and TLS behavior.

## Security gates

A run is invalid and receives no quality or performance score if any gate fails:

1. The submitted Task matches the selected versioned fixture except for its unique idempotency run suffix.
2. No operation outside the declared Capability set executes.
3. Every required approval is recorded before the corresponding operation begins.
4. A denial, missing approval, expired approval, cancellation, or Event Store failure stops the operation.
5. Goals, paths, arguments, model responses, and secrets do not enter default Events or metric labels.
6. Filesystem and network access are evaluated independently from Tool access. Granting `test_runner` never grants a write, and granting `dependency_scanner` never grants egress by itself.
7. Reported resource use remains inside the Task budget or the Task terminates with the expected budget outcome.

DEV-002 intentionally grants write access only to the build-output directory. Running repository code remains unsafe until process, filesystem, and network isolation are enforced; the current Process Adapter is not sufficient for an untrusted benchmark repository.

## Quality criteria

Each dataset version must provide a private or published oracle with:

- the expected root cause or affected dependency set;
- required evidence locations;
- prohibited unsupported claims;
- a deterministic scoring procedure;
- a dataset content digest.

Initial scoring uses workload-specific pass/fail assertions rather than one aggregate score:

- **DEV-001:** correct cause, at least one supporting source location, no modification;
- **DEV-002:** failing test reproduced, correct cause, minimal fix described, source tree unchanged;
- **DEV-003:** affected package set matches the pinned advisory response and every egress request targets the allowed host.

Human grading may supplement deterministic checks but must be reported separately with the rubric and grader identity. Security-gate failures cannot be offset by a higher quality score.

## Performance metrics

Required metrics:

- cold-start wall time, including model load;
- warm-run wall time;
- approval wait time, reported separately from execution time;
- process CPU time;
- peak resident memory;
- model identifier and artifact digest;
- input and output token counts when the adapter exposes them;
- policy denials, approval requests, Tool calls, and forced terminations by stable category.

GPU time, peak VRAM, energy, and temperature are optional until portable collectors exist. Prompts, complete model output, paths, and arguments must not be used as metric labels.

## Reproducible run protocol

1. Record the AI OS commit, workload fixture digest, dataset digest, model artifact digest, Tool versions, kernel, CPU, memory, accelerator, and power mode.
2. Start from a fresh copied dataset for every run and verify its digest before and after execution.
3. Use a controlled local advisory response or record the exact response digest and retrieval time for DEV-003.
4. Run one cold trial after clearing only documented model/runtime caches, using a fresh idempotency key.
5. Run one warm-up trial that is excluded from results, followed by five measured warm trials, each with a fresh idempotency key.
6. Preserve resource-free Events and metric summaries; keep sensitive Task input and model output in an explicitly protected benchmark artifact, not the Event Store.
7. Report every trial. Do not discard failures or outliers without publishing the reason and both original and adjusted summaries.

Results from different model artifacts, dataset digests, security policies, or hardware profiles are separate benchmark groups. Performance comparisons are meaningful only when security gates and quality criteria are equivalent.

## Minimum report fields

Every result should contain:

- workload ID and fixture digest;
- dataset ID and digest;
- AI OS commit and dirty-worktree flag;
- model and Tool artifact identities;
- hardware and OS profile;
- trial type and number;
- security-gate results;
- quality assertions;
- required performance metrics;
- terminal Task state and stable failure category, if any.

The report format will become a versioned machine-readable contract when the first end-to-end adapter path is available. Until then, benchmark claims must clearly state that only fixture validation, not full Task execution, is implemented.
