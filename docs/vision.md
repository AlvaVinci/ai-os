# Vision

## Background

General-purpose operating systems evolved around people directly operating applications, files, and windows. When AI agents run on top of those systems, goals, permissions, context, models, resource budgets, approvals, and audit records are often fragmented across applications.

AI OS aims to manage those concerns through a shared local runtime so that AI workloads can run safely and efficiently on personal computers.

## Goals

- Run useful AI tasks while keeping personal data on the device by default.
- Manage multiple models and agents through common permission, resource, and audit contracts.
- Separate AI proposals from deterministic enforcement of permissions and limits.
- Choose execution strategies under CPU, GPU, RAM, VRAM, power, time, and cost constraints.
- Remain compatible with Linux tools and existing developer workflows.

## Non-goals

- Replacing Windows, macOS, or Linux as a general-purpose desktop OS in the initial phases.
- Delegating memory protection, access control, or filesystem integrity directly to AI models.
- Running irreversible or high-impact operations without human approval.
- Optimizing the core design for only one model, GPU, or cloud service.
- Making natural language the only user interface.

## Target users

### Individuals

People who want to use local documents, schedules, code, and other private data while controlling external transmission.

### Software developers

Developers who want to delegate repository analysis, implementation, tests, and documentation with explicit permissions and budgets.

### AI application developers

Developers who want common task, permission, audit, and resource APIs instead of rebuilding them for each model integration.

## Representative use case

```text
Investigate this repository and prepare a proposed fix for the failing tests.
Do not use the network. You may modify only src/ and tests/.
Use at most 8 GiB of memory and stop after 30 minutes.
Ask for approval before committing or deleting files.
```

AI OS converts this request into a task, capabilities, budgets, and approval requirements. The agent works within that boundary, while important decisions and operations are recorded as structured events.

## Definition of success

MVP success is not determined by maximum inference speed alone. The runtime must also demonstrate that:

- unauthorized file and network access is blocked
- task resource limits are observable and enforceable
- task APIs remain stable when the model backend changes
- failures and human approvals can be traced from events
- representative developer tasks can complete with local models only

## Core position

The model is not the kernel. A model is a component that reasons inside a trust boundary, and its output is always untrusted input. Deterministic software remains responsible for permissions, resource limits, auditability, and preventing irreversible operations.
