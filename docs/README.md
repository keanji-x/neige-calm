# Documentation

Start with the [English README](../README.md) or [中文 README](../README.zh-CN.md)
for the product overview and source-based quick start. User guides below describe
the implementation on `main`; a design record may also contain historical
diagnoses, proposed changes, or explicitly deferred work.

## Use and operate Neige Calm

| Need | Guide |
| --- | --- |
| Create reusable plans, supervise work, open files, add plugins | [Using Neige Calm](using-neige-calm.md) |
| Write task blocks in a Recipe | [Recipe body format](recipe-body-format.md) |
| Build or install a Linux Alpha archive | [Alpha release runbook](alpha-release.md) |
| Deploy, back up, upgrade, or recover an installation | [Deploy & Upgrade Guide](deploy-and-upgrade.md) |
| Configure the supervisor and child server | [neige-app configuration](neige-app-config.md) |
| Understand local plugin execution and credentials | [Plugin host security](plugin-security.md) |
| Manage event retention and disk usage | [Events retention runbook](events-retention.md) |
| Understand compatibility and migration policy | [Upgrade stability policy](upgrade-stability.md) |

## Develop and understand the system

| Topic | Entry point |
| --- | --- |
| Contribution and verification rules | [Contributing](../CONTRIBUTING.md), [repository agent guidance](../AGENTS.md) |
| Frontend setup and architecture | [Frontend README](../fe/README.md), [frontend guidance](../fe/AGENTS.md) |
| Stack E2E tiers | [E2E README](../e2e/README.md) |
| Executable UI contracts | [Oracle schema and conventions](oracle/SCHEMA.md) |
| Kernel and app responsibilities | [Kernel/app boundary](architecture/955-kernel-app-boundary.md) |
| Reports as executable plans | [Doc-as-plan design](architecture/985-doc-as-plan.md) |
| Worker evidence, verification directories, and deferred recovery work | [Long task reliability](architecture/long-task-reliability.md) |
| Track-create retry identity | [Idempotency design](design-1384-track-idempotency.md) |
| Permanent retry records and deleted-Track recovery | [Idempotency retention decision](design-1428-idempotency-retention.md) |

Numbered files in this directory and `architecture/` are design and investigation
records, not a release checklist. Check their delivery scope and the associated
merged implementation before treating a proposal as available behavior.
