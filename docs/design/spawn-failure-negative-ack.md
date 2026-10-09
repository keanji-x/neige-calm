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

Each task launch requires an owner-declared typed terminal role:
`BusinessProcess` or `OptionalViewer`. Terminal and Claude workers declare the
business role; Codex workers declare the optional viewer role. Gate/verify
launches declare business processes. The role travels with `TaskLaunch` to the
renderer; it is neither a provider identity check nor a persisted business
protocol. New callers must choose their role explicitly, without a default.

Only a fresh business-process exchange may persist `rejected` through
terminal_launch's transactional CAS. It checks operation identity, nonempty lease, spawn_started,
requested state/version, prepared terminal identity, recorded absolute socket,
card ownership and absent PID. Failure to persist cannot grant cleanup. Rejected
is terminal and never resets or issues EnsureProc again. The generic driver
continues to preserve the database checkpoint in set_compensating.

Business-process recovery consumes rejection before exit preservation. Compensation checks the
same owning classification before treating an exit (including boot's synthetic
-1) as proof of a real run. Evidence-read failures propagate to the compensation
driver: the step records its error and remains incomplete, retaining the cleanup
obligation and resources.
Business-session vetoes remain authoritative. A Codex optional viewer never
writes Rejected: a negative reply leaves Requested consumed. Recovery keeps the
existing AttachOnly/NoOp and exited-terminal CardAdded path, without a new
business-evidence query or rejection recovery branch. Viewer failure cannot
start another turn, resend EnsureProc, or authorize business compensation.
Generic disposal stays conservative until worker compensation removes the
owned rows.

Verification uses real claimed tasks, provider adapters and OperationRuntime,
with a fake shared Codex appserver at its production seam. Crash recovery reopens
SQLite and runs boot reconciliation before operation recovery; a separate case
exercises AttachOnly recovery before boot reconciliation. Tests cover
negative cleanup, unknown retention, fast exit, business veto, and optional
viewer failure after durable business startup. Viewer restart tests cover both
acknowledgement before CardAdded and CardAdded before phase persistence, checking
persisted and published recovery events. A projection-decoder fault demonstrates
that the existing exited-terminal path needs no preliminary business-evidence
lookup and never enters business startup compensation. The fixture repairs its
injected transient fault at CardAdded insert, before ordinary VCS projection;
that existing projection remains part of event persistence. Shared cleanup
evidence errors remain Result failures.
The cleanup evidence-read crash test blocks the transition to Stuck, leaving an
incomplete Compensating step for boot retry after evidence is repaired. Stuck is
terminal; these tests do not claim that Stuck automatically retries.
Kernel gates own text/contract/quick checks and focused replay; CI owns broad
suites. No new test-name pins or recurring checks are introduced. The existing
worker-kind ownership queries remain a maintenance cost: adding a worker kind
requires updating initialization and ownership query selections in
`terminal_launch.rs`; this change does not assert that list is permanently closed.

PTY spawn classification relies on portable-pty 0.9.0's Unix spawn_command
returning std::process::Command::spawn's error directly, with no fallible stage
after successful spawn; std's Unix error handshake reaps a child that failed
before exec. Thus this classification proves no executable child, not that fork
never happened. Tokio pipe spawn remains Unknown because constructing its async
handle can fail after exec. Review these assumptions when upgrading portable-pty,
Tokio or the Rust toolchain; do not broaden negative evidence to readiness or
post-spawn failures.

Per-run commands, red/green results and mutation history belong in PR evidence,
not this durable design contract.
