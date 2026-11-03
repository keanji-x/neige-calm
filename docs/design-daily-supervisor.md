# Daily Supervisor (#3500)

Daily Supervisor uses the existing Today route, daily Track identity and history.
It adds instance health to Today and gives the conversation read access to the
same observations. Existing daily reports and their date URLs remain readable.
Provisioning a daily Track still does not start a model turn.

## Read path

Bind one deployment and one Neige checkout explicitly in configuration. The
deployment reports its running release and health. Resolve the deployed commit
from release provenance. Show checkout HEAD, its dirty state and the last
observed remote main separately. Surface missing provenance and stale remote
observations explicitly. Never infer the deployed revision from the checkout.

Use bounded, read-only probes from the component that owns each source.
Record observation time, source, result and last successful observation.
Keep recent samples under a retention limit. Compute the daily report for the
Asia/Shanghai day from those samples. Current findings and dated reports share
rule definitions and evidence. Historical dates show their own observations.
Distinguish healthy, degraded, unavailable and stale sources.

| Rule | Evidence | Classification |
| --- | --- | --- |
| SQLite writes | Existing managed transaction observations, aggregated into twelve five-minute buckets per open database handle | Report mean acquisition, BEGIN and retry-backoff time, lifecycle error count and built-in retry attempts. Statement errors, outer caller retries, cancellations and successful rollbacks are outside these error and retry counts. Buckets are process-local and empty buckets have no observation. |
| Scheduler recovery (#3432) | HTTP readiness plus recovery phase, remaining work and oldest item | HTTP readiness alone does not establish scheduler health. |
| Track progress | Current task or attempt state, last progress event, provider state | Waiting for user input is expected. Flag only states with an execution progress expectation. |
| Stop settlement (#3374) | Stop request and its original driver identity, observed exit or unsettled outcome | Flag unresolved execution past its existing cancellation window. Keep unknown distinct from alive. |
| Lease cleanup (#3390, #3337, #3302) | Lease owner and exact attempt or worker session, durable terminal state and cleanup outcome | Flag ended owners retaining leases past the settlement window. Check a consistent snapshot so ownership transfer cannot produce a false finding. |
| Workspace retirement (#3495) | Existing workspace resource ledger, retirement timestamp and reason | Show intentional retention separately from admitted cleanup failure or unknown outcome. Inventory does not traverse or delete retired paths. |
| Data integrity | Bounded checks of explicit owner-layer invariants | Report counts and stable record identities. Missing evidence stays unknown. |

Link each finding to the Track and then to the attempt, lease or resource
identity where available. List new, continuing and resolved findings in the
daily report. A failed collection does not resolve a finding. Avoid exposing
database rows, paths or diagnostics to sessions without an instance-operator
grant. Keep the generic Track and report layers independent of deployment policy.

## Actions and failure handling

Deploy an explicit release through the existing `neige-app` upgrade flow.
Bind operator confirmation to the target instance, running version and requested
release. Use the existing upgrade classification and apply checks, and display
the durable outcome after reconnect. The first action supports preserving
upgrades. Breaking upgrades and physical workspace deletion require their
separate maintenance protocols.

Keep liveness checks outside calm-server so a stopped or restarting server
remains observable. Conversation input can trigger a fresh bounded diagnostic
and attach an incident note. It cannot turn feedback text into an authorized
operation.

## Delivery and acceptance

1. Read-only inventory and current Today status. Verify against fixtures with
   deployed and checkout revisions that differ, unavailable sources, an
   intentionally retired workspace and a normal Track waiting for user input.
2. Collect timed observations and derive daily findings. Exercise the production
   entry points with delayed recovery, failed Stop, an ended lease owner, and a
   concurrent successor attempt. Verify restart, stale data, source failures
   and day rollover.
3. Add the operator-confirmed deploy action. Verify confirmation replay,
   wrong-instance refusal, concurrent upgrade, reconnect, health failure and
   the existing preserving rollback path in a test deployment.

Implementation review tier is L2 for operator visibility, durable observation
history and deployment authorization. Verify each slice against the relevant
Rust contracts, frontend tests and a real browser preview. Verify load-bearing
authorization and ownership assertions with production-code mutations.
