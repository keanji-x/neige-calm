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
contention, and observes cancellation. A stale checkpoint stops the source;
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
cancellation, stale tasks and active driver boot. Mutation splits the production
transaction and must fail exactly the three selected crash regressions.

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
