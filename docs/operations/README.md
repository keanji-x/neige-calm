# Operations index

Use this guide to identify the affected Neige instance and choose the next read-only check.

Start with the effective configuration, data directory, service addresses, running version, and observation time. Find the configuration file from the service start command and check for command-line overrides. Keep host-specific values in the incident record and omit credentials.

The instance manager owns service lifecycle and upgrades. The runtime owns Tracks, attempts, sessions, and their persisted state. Today and reports present the evidence. Follow the running version and deployment history to identify the instance, then trace the affected Track to its attempt, session, and execution or lease.

| Question | Read |
| --- | --- |
| Where is the database and what is its current schema? | [Locate and inspect SQLite](database.md) |
| Why is a service, Track, or execution stalled? | [Investigate stalled work](stalled-work.md) |
| What is the effective instance configuration? | [neige-app configuration](../neige-app-config.md) |
| What was deployed? How do I upgrade or recover? | [Upgrade history](../deploy-and-upgrade.md#51-get-upgradehistory) and [deployment runbook](../deploy-and-upgrade.md) |
| What do SQLite wait times and retries measure? | [Transaction observations](../db-transaction-observation.md) |
| Why are retired workspaces still on disk? | [Workspace retirement](../workspace-retirement.md) |
| How do I investigate event retention and disk growth? | [Event retention](../events-retention.md) |

Deployment owners can open **Today → Daily Supervisor** for the scheduler boot scan, Worker cleanup candidates, retired workspaces, and SQLite write wait. Use the stall guide to investigate individual executions.

Daily Planner can read `guide/operations.md`, `guide/database.md`, and `guide/stalled-work.md` with `neige_cat`. Links to the deployment runbooks are for operators. In reports, distinguish a merged PR, an available release, an applied upgrade, and a verified running version. Cite the observation for each claimed stage and record missing evidence as unknown.
