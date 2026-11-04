# Investigate stalled work

Record the target instance, affected object, last successful action, expected next step, and time. Start with the broadest symptom, then follow the exact execution identity.

| Symptom | Read first | Confirm |
| --- | --- | --- |
| Service or page unreachable | Service manager logs, neige-app `/health` and authenticated `/status` | Whether the supervisor is alive and the child is starting or repeatedly exiting |
| Service reachable with unexpected behavior | `/api/version`, Today → Daily Supervisor, [upgrade history](../deploy-and-upgrade.md#51-get-upgradehistory) | Running release, boot scan state, and freshness of observations |
| Track has not progressed | Track task and session state, `neige_track_status` if available | Waiting for input, dependencies, resources, execution, or a confirmed failure |
| Stop requested and execution still visible | Stop request and subsequent events for the exact attempt and session | Whether the request was accepted and whether exit or settlement has been observed |
| Resource remains after a task ended | Cleanup candidates, lease ownership and retirement ledger | Current owner, grace period, successor attempt, and last cleanup result |

Get the actual addresses and authentication from the target instance. See the [deployment runbook](../deploy-and-upgrade.md) for endpoint examples.

Trace Track → attempt → session → execution or lease. Retries can change identities. Compare the timestamps of last progress, stop or timeout request, settlement, and cleanup. Use the scheduler's wait reason when available. If reads fail or event pagination is incomplete, record the gap. See [read-only database inspection](database.md) when the state interface cannot answer the question.

Keep three resource categories distinct. A cleanup candidate still needs an outcome. A held lease needs its exact owner and release state. A retired workspace may remain on disk by policy. See the [retirement guide](../workspace-retirement.md).

For handoff, include instance, version, affected IDs, timeline, current wait or error, evidence links, and actions already attempted. Follow the applicable runbook and authorization for restart, rollback, or offline maintenance, then verify the instance and affected objects again.
