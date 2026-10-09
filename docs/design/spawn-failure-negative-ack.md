# Terminal launch negative acknowledgement (#2353)

Review tier: L2 because this changes the durable authorization for deleting
prepared worker resources. The Planner arranges two independent reviews.

The supervisor must distinguish `NoChildCreated` from `Unknown` using a required
`SpawnFailedDisposition`. Pre-spawn failures prove that this EnsureProc created
no executable child; failures after a returned child handle and all readiness
failures remain Unknown. Reaped status does not authorize cleanup. Missing or
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
-1) as proof of a real run. Business-session vetoes remain authoritative. Generic
disposal stays conservative until worker compensation removes the owned rows.

Verification: first run the real claimed task -> TerminalWorkerAdapter ->
OperationRuntime -> real supervisor invalid-cwd regression red. Run only the
requested focused targets, protocol tests, and single-factor production mutation
of the durable rejection authorization. Kernel gate owns text/contract/quick
checks and focused replay; CI owns broad suites. No extra test-name pins or
recurring checks are introduced; ordinary new tests need no additional lists.
