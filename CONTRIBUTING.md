# Contributing

Thank you for your interest in AI OS. The project is in an early design and implementation phase. Concrete use cases, constraints, failure scenarios, tests, and focused code changes are welcome.

## Project language

English is the canonical language for source code, documentation, issues, and pull requests. Clear contributions from non-native English speakers are welcome; perfect wording is not required.

Translations may be added under language-specific directories when there is enough demand and a maintenance owner. Canonical technical decisions remain in English to avoid specification drift.

## Before opening an issue

- Search existing issues and documentation.
- Describe the affected user and the problem before proposing an implementation.
- Include externally observable acceptance criteria when possible.
- Do not post security vulnerabilities publicly; follow [SECURITY.md](SECURITY.md).

## Change guidelines

- Keep each change small and focused on one purpose.
- Preserve established terminology and trust boundaries.
- Avoid unrelated refactoring and unnecessary dependencies.
- Include tests or reproducible verification for behavior changes.
- Describe security, compatibility, operational, and rollback impact.

## Development workflow

1. Find or create an issue for the change when design discussion is useful.
2. Work on a focused branch.
3. Run formatting, tests, and static analysis.
4. Open a pull request that explains the reason, verification, and impact.

Use Conventional Commits:

```text
docs(vision): define the local-first principle
feat(runtime): add the task state machine
fix(policy): reject paths before normalization
```

## Design decisions

Discuss alternatives in an issue or architecture decision record before implementing changes to:

- public APIs or persistent formats
- trust boundaries, capabilities, or approval flows
- long-running services or external dependencies
- supported operating systems, CPU architectures, or model runtimes
- kernel features or privileged processes

## License

By submitting a contribution, you agree that it is provided under the [Apache License 2.0](LICENSE).
