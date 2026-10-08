# Worker-flow capture atomicity (#2345)

Review tier: L2, because capture crosses a persistence boundary. The planner
arranges two independent review channels.

One source record must persist all normalized items and its checkpoint together.
The source owns parsing, identity, sequence/turn reconstruction, and rewrite/reset
rules. The exec contract requires an explicit expected durable checkpoint and a
next checkpoint, including source path. Truth serializes the entire batch before
SQLite opens one immediate transaction: compare expected checkpoint, insert all
items, upsert checkpoint, commit once. Empty batches advance metadata/malformed
records too. Existing schemas and public wire types stay unchanged.

A shared capture writer suppresses idle checkpoints, retries only safe writer
contention, and observes cancellation. Cancellation settles the in-flight batch
without dropping its sink future; it prevents subsequent batches/retries. The
driver cancels and joins the old capture before starting a replacement, serialized
under its task-map lock. A stale checkpoint logs its card/source/path/expected
checkpoint and stops the source;
normalizer state is never used after a failed/stale batch. Other errors, including
uncertain commit acknowledgement, stop capture so a fresh source reconstructs
from durable state instead of blindly repeating inserts. Path changes and rewrites
retain their current reset/reingest semantics using the actual durable checkpoint
as the compare value. Remove delayed cursor persistence configuration.

Fixtures expose typed, keyed one-shot handshake seams, with no environment hook.
First reproduce the old item-commit/checkpoint gap through real provider capture,
WorkerFlowSink and file-backed SqlxRepo. Interrupt and reopen the database. After
the fix the same item seam is inside the transaction before checkpoint SQL; an
additional seam observes commit before acknowledgement. Assertions pin payloads,
ordering, sequence, turn, source identity, cursor, and stable second recovery.
Cover both providers, multi-item Claude records, cursor SQL failures, BUSY with
cancellation, stale tasks, active driver boot, replacement settlement and source
path A→B on the same card. Mutation splits the production
transaction before the same ItemInserted seam. Run the entire registered
`capture_atomicity_` selection and compare its complete red set against the
prediction; selecting only the three crash tests is insufficient evidence.

Worker runs focused reproduction, repair, mutation, formatting and the supplied
seven gate prechecks in order. Kernel repeats gates against its frozen committed
target; CI owns broad suites. No real Codex E2E runs on the shared host. Historical
partial writes/duplicates and terminal-runtime boot policy are outside this fix.

The compare value includes the existing `updated_at_ms` activity timestamp along
with every position/identity field. Keep its wall-clock liveness semantics; it is
not a logical generation counter. A successful write returns the exact durable
checkpoint. Commit errors are final
for that source; only failed BEGIN or confirmed rolled-back statement contention
is retryable. Standalone item/cursor APIs remain for explicit read-model fixture
seeding; neither provider nor the capture sink calls them.

The existing `out_of_domain.rs` remains above 800 lines (889, previously 890).
New capture persistence and shared cursor SQL live in a separate module; splitting
the remaining unrelated repository implementation is outside this change.

Replacement recovery was reproduced with both real provider sources, the real
WorkerFlowDriver/WorkerFlowSink, and a file-backed SqlxRepo. A fixture-only SQLite
commit hook holds the sqlx worker inside the actual COMMIT before durability.
The WAL reader sees the previous checkpoint while the old transaction retains
SQLite's single writer lock. In the original lifecycle, cancellation drops the
sink future, replacement reads that old checkpoint, and the already queued
COMMIT then finishes. Its next BEGIN IMMEDIATE serializes the CAS, but cannot
correct the earlier source cursor read: Stale stops replacement, leaving only
2 of 3 active records with no subsequent running event. Both regressions failed
on that exact missing-tail assertion (`/tmp/fix1-replacement-red.log`).

The fix retains and awaits the in-flight capture future when cancellation wins.
Joining this source therefore includes its COMMIT/rollback acknowledgement;
joining a source that had dropped that future would not provide this guarantee.
The driver joins before replacement can read a checkpoint, and the same
settlement helper serves direct replacement, Superseded, terminal status and lazy
conversation cancellation. Finished-task pruning no longer discards a merely
cancelled task. Sources never acquire the driver task-map lock; holding that lock
also serializes concurrent attachments. Cancellation can now wait for one
in-flight database operation (existing SQLite acquire/busy bounds), and delays
other driver task-map operations during that settlement. Driver Drop remains
cancellation-only because it cannot await; it is shutdown, not an attachment
entry point. Terminal boot selection/final-drain policy remains unchanged.

New path tests first capture A through the real source, then capture distinct
records from B on the same card/runtime. They pin retained A row IDs/payloads,
all B payloads including reset seq/turn, and B path/index/offset/uuid/hash. The
writer's expected value is the unfiltered durable A checkpoint; only the source
read position is path-filtered. No production path/reset semantics changed.
All four new regressions are registered directly under
`worker_flow_driver_suite::worker_flow_capture_atomicity::capture_atomicity_`;
the helper module introduces no alternate test prefix.

Full mutation evidence (2026-10-08): predicted and actual red sets are identical,
with 20 selected tests, 7 failed and 13 passed. All names below have the module
prefix `worker_flow_driver_suite::worker_flow_capture_atomicity::`:

- `capture_atomicity_claude_crash_before_checkpoint_no_duplicates_or_loss`
- `capture_atomicity_codex_crash_before_checkpoint_no_duplicates_or_loss`
- `capture_atomicity_claude_partial_record_crash_no_duplicates_or_loss`
- `capture_atomicity_claude_cursor_error_restart_no_duplicates_or_loss`
- `capture_atomicity_codex_cursor_error_restart_no_duplicates_or_loss`
- `capture_atomicity_claude_boot_restart_preserves_every_record`
- `capture_atomicity_codex_boot_restart_preserves_every_record`

Crash and boot abort at ItemInserted after the mutated item COMMIT, so recovery
repeats committed items; cursor-error rollback cannot remove those items. The
other 13 tests, including the new path and replacement tests, do not interrupt
that split gap: path switches run sequentially, and replacement waits for the
old checkpoint COMMIT. Prediction was recorded before mutation. The production
mutation diff and hash confirm COMMIT+BEGIN IMMEDIATE after all items and before
the original seam, with no test/normalizer mutation. Complete logs/sets are
`/tmp/fix1-mutation-predicted.txt`, `/tmp/fix1-mutation-actual.txt`,
`/tmp/fix1-mutation-applied.diff`, `/tmp/fix1-mutation-full.log` and
`/tmp/fix1-mutation-comparison.json` (missing=[], extra=[]).
Byte-identical restoration is confirmed against `/tmp/fix1-pre-mutation.rs`;
restored SHA-256 is
`98fbcb763f95b66d79206c89ab399ef62bb3f3c826a8609164a74e32e3de96b5`.
The same complete selection passed 20/20 after restoration in
`/tmp/fix1-mutation-restored-green.log` (and before mutation in
`/tmp/fix1-focused-green.log`). This replaces the earlier selected-three-only
mutation claim; it does not claim all error classification branches are tested.

Both mutation and restoration used this exact selection, rejecting empty runs:

```bash
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 \
  cargo nextest run --locked -p calm-server --test runtime_suite \
  --features calm-server/fixtures \
  -E 'test(/^worker_flow_driver_suite::worker_flow_capture_atomicity::capture_atomicity_/)' \
  --no-tests fail --no-fail-fast --test-threads 8
```

Scope/caller review: both sources use the shared writer and the sole production
sink/repository capture entry point. Driver attach paths cover boot, Started,
Running/Idle/TurnPending, CardAdded and Superseded; cancellation paths above share
settlement. CAS SQL, parsing/normalizers, schema/migrations, generated/public wire
contracts and provider classification are unchanged. No generic layer gains a
provider-identity branch. Fixture handshakes do not enter release builds. Claude
prefix I/O default fallback and broader error/seam coverage remain follow-up work.
