# Terminal launch negative acknowledgement (#2353)

Review tier: L2 because this changes the durable authorization for deleting
prepared worker resources. The Planner arranges two independent reviews.

The supervisor must distinguish `NoChildCreated` from `Unknown` using a required
`SpawnFailedDisposition`. Errors in stages before invoking process spawn prove
that this EnsureProc created no executable child. Pipe Tokio `Command::spawn` errors remain Unknown:
Tokio can exec successfully before failing to construct its asynchronous child
handle. Failures after a returned child handle and all readiness failures also
remain Unknown. Reaped status does not authorize cleanup. Missing or
unknown wire classifications fail deserialization; deploy server and supervisor
together with the bumped control version. Old durable launch records remain
conservative; no released migration changes.

Only a fresh task exchange may persist `rejected` through terminal_launch's
transactional CAS. It checks operation identity, nonempty lease, spawn_started,
requested state/version, prepared terminal identity, recorded absolute socket,
card ownership and absent PID. Failure to persist cannot grant cleanup. Rejected
is terminal and never resets or issues EnsureProc again. The generic driver
continues to preserve the database checkpoint in set_compensating.

Recovery consumes rejection before exit preservation. Compensation checks the
same owning classification before treating an exit (including boot's synthetic
-1) as proof of a real run. Evidence-read failures propagate to the compensation
driver: the step records its error and remains incomplete, retaining the cleanup
obligation and resources.
Business-session vetoes remain authoritative. A Codex rejection concerns only the
optional viewer. Codex recovery first checks the persisted nonempty business
thread and active turn; that contract authorizes success without interrupting,
starting another turn, or attempting the rejected viewer again. Rejection without
that business contract still refuses startup. This provider policy belongs in
the Codex adapter, not the generic driver. Generic
disposal stays conservative until worker compensation removes the owned rows.

Verification uses real claimed tasks, provider adapters and OperationRuntime,
with a fake shared Codex appserver at its production seam. Crash recovery reopens
SQLite and runs boot reconciliation before operation recovery. Tests cover
negative cleanup, unknown retention, fast exit, business veto, evidence-read
failure with retry, and viewer rejection after durable business startup.
Kernel gates own text/contract/quick checks and focused replay; CI owns broad
suites. No new test-name pins or recurring checks are introduced. The existing
worker-kind ownership queries remain a maintenance cost: adding a worker kind
requires updating initialization and ownership query selections in
`terminal_launch.rs`; this change does not assert that list is permanently closed.
Per-run commands, red/green results and mutation history belong in PR evidence,
not this durable design contract.
