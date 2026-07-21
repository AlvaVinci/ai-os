# Security Policy

## Scope

AI OS is currently in an early implementation phase. Security issues include flaws in published designs, code, or defaults that could enable authorization bypass, data disclosure, arbitrary code execution, audit bypass, or denial of service.

AI OS is not production-ready and must not yet be used as a security boundary for sensitive workloads. See the [Threat model](docs/threat-model.md), [Capability model](docs/capability-model.md), [v0.1 Definition of Done](docs/definition-of-done.md), and [Architecture Decision Records](docs/adr/README.md) for current objectives, residual risks, and release blockers. Missing operating-system enforcement must not be inferred from policy-only behavior.

## Reporting a vulnerability

Do not report vulnerabilities in public issues.

Open this repository's **Security** tab, select **Advisories**, and choose **Report a vulnerability** to submit a private report.

If private reporting is unavailable, do not publish details or reproduction steps. Ask a repository maintainer to establish a private communication channel.

Include as much of the following as possible:

- affected version or commit
- prerequisites and reproduction steps
- expected impact
- known mitigations
- planned disclosure date or existing third-party disclosure

After receiving a report, maintainers will evaluate impact and reproducibility, then coordinate remediation and disclosure with the reporter. Response-time targets will be defined when the maintenance team is established.
