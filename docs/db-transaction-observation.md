# SQLite transaction observation

The existing SQLite writing-transaction entries emit one structured DEBUG event per
invocation on `calm_truth::db::transaction`. Enable this target through the server's
existing logging filter, for example:

```text
RUST_LOG=info,calm_truth::db::transaction=debug
```

The default logging filter leaves this target disabled. This adds no database
schema, telemetry backend, operation registry or per-query logging. Event fields
contain static source file/line provenance and lifecycle measurements, without SQL,
parameters, database errors, session identifiers or payloads. Existing tracing
context is captured when the helper future is first polled and retained even if
the future is later dropped outside that context. This supports the existing
operation tracing context, including instrumented futures.

## Coverage

`begin_immediate_tx` retains its existing bounded busy retries. The single-attempt
`begin_immediate_tx_once` serves callers that already own their retry policy,
including source-record capture and startup backfill. Both use the same observed
acquisition/BEGIN implementation. Domain `_tx` helpers still borrow SQLx's raw
transaction; authorization, projection and atomic publication contracts stay in
their existing owners.

Raw transactions constructed directly by test fixtures, SQLx's migration machinery
and nested savepoints are not additional observations. These events do not measure
every SQL statement or auto-commit pool operation.

## Measurements

All durations are caller-observed milliseconds with sub-millisecond precision.

| Field | Meaning |
| --- | --- |
| `source_file`, `source_line` | Compiled caller location captured when the helper is called. |
| `attempts` | Number of connection-acquisition attempts, including retries. |
| `begin_attempts` | Attempts that acquired a connection and reached BEGIN. |
| `acquire_ms` | Accumulated pool acquisition elapsed, including connection setup. |
| `begin_ms` | Accumulated BEGIN elapsed, including worker/admission and busy waiting. |
| `backoff_ms` | Retry backoff elapsed. |
| `active_ms` | Successful BEGIN acknowledgment to finalization start or caller drop. |
| `commit_ms`, `rollback_ms` | Time spent awaiting the corresponding finalization phase. |
| `total_ms` | Helper-future polling through terminal observation. |

`begin_ms` is not pure SQLite lock-wait time. `active_ms` includes SQL and any other
work awaited while the caller owns the transaction. A long maintenance operation
can have short transaction phases; a short operation can spend most of its time
waiting for another writer. Compare these fields with unrelated-writer latency in
mixed-load profiles rather than treating total operation time as writer occupancy.

## Outcomes and interrupted operations

Acknowledged explicit finalization reports `committed` or `rolled_back`. Failures
report `acquire_error`, `begin_error`, `commit_error` or `rollback_error`; timing
records do not replace the original returned SQLx/domain error.

A dropped admission future reports `acquire_interrupted`, `begin_interrupted` or
`backoff_interrupted`. An active transaction dropped by an error path, cancellation
or ordinary scope exit reports `active_dropped`; Drop alone cannot identify which
cause occurred. SQLx queues its normal rollback, and the existing pool-release
repair remains authoritative. The record does not assert rollback completion or
physical lock release.

`commit_interrupted` and `rollback_interrupted` mean the caller stopped awaiting
finalization. In particular, SQLite may have completed a COMMIT after its future
was cancelled: the outcome is unknown from this observation, not necessarily a
rollback. Always inspect durable state before deciding whether an operation can be
retried. Interrupted phase durations end at caller drop and cannot establish the
actual cleanup-completion timestamp.

## Maintenance

The observation owner mirrors the pinned SQLx 0.8.6 `Pool::begin_with` acquisition
seam using its public, doc-hidden `Transaction::begin` constructor. This preserves
SQLx's owned pooled-connection cleanup while separating acquisition from BEGIN.
When upgrading SQLx, verify that seam and replay the cancellation/pool-recovery
tests; do not replace SQLx's transaction manager or its Drop behavior.
