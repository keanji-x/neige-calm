# Conversation title consistency and SQLite serialization

Review tier: L2. The complete repair crosses the title read and restored-input
persistence boundaries. Two fresh independent reviews remain Planner-owned.

## Final storage boundary

[The SQLite title owner](../crates/calm-truth/src/db/sqlite/conversation_title.rs)
uses the existing `begin_immediate_tx` once per nonempty metadata request, before
any candidate SELECT. All indexed 32-candidate batches and the existing
projectable-runtime batch run on that transaction connection; commit releases
writer eligibility. Empty ids return empty facts without acquiring a connection.
The transaction only performs local database reads and candidate decoding: no
network/provider call, nested repository write, or other lock is held at begin.
Errors propagate through the existing result type; dropping the transaction uses
the real repo's rollback/release repair. No statement retry is added.

This deliberately **serializes metadata reads with writers**, including in WAL.
It is not a contention-free read-only capability. The Planner withdrew the old
requirement that another writer commit during the read. The selected minimal
solution uses the repository's existing safe transaction entry point instead.
[Its bounded retry](../crates/calm-truth/src/db/sqlite/infra.rs) is only at BEGIN,
where a new attempt holds no table locks (six retries with existing bounded
backoff). Mid-transaction retries are prohibited and would not resolve a cycle.
Busy/acquisition budgets and all configuration remain unchanged.

[The #930/#1016 guard](../crates/calm-server/tests/cases/deferred_write_tx_invariant.rs)
keeps its empty deferred allowlist. [Shared-cache semantics](../crates/calm-truth/src/db/sqlite/deadlock_semantics_tests.rs)
show why an asynchronous deferred read is unsafe: it can retain a transcript-table
lock while SQLx parks waiting on runtime, against a writer holding runtime and
waiting on transcript. `sqlite::memory:` uses shared cache; WAL pragmas do not
change that into a WAL database. Claiming writer eligibility before any SELECT
prevents another immediate writer from already holding that other table's write
lock. A waiting begin has no reader table locks to complete the cycle. No guard,
scanner, allowlist, generic mutex, connection pool, or isolation exception is added.

Before changing production, the actual guard was rerun against r8 and failed at
`conversation_title.rs:96: deferred .begin(`. Evidence:
`/tmp/title-r9-guard-red.log`. The first attempted worker command named the module
`kernel_process_suite` as a Cargo target and selected nothing; inspection located
its real registration in `runtime_suite`, which produced the recorded red.
This was not a missing target in the declared gate.

## Ownership and consistent facts

[ConversationTitleContext](../crates/calm-truth/src/conversation_title.rs) contains
required persisted-source and projectable-runtime collections from one storage
snapshot. Candidate SQL, indexed traversal, visibility decoding, projectable
query builder, and row decoder remain authoritative in truth. The connection
helper in [session_projection.rs](../crates/calm-truth/src/db/sqlite/session_projection.rs)
is shared with the existing pool query. Current-session eligibility still excludes
superseded carriers. Persisted input without a runtime remains valid; structured
attachment-only input remains visible but unnamed, blocking later input.

[The server](../crates/calm-server/src/conversation_metadata.rs) prefers persisted
visible input, then parses opaque runtime JSON with `HarnessSnapshot::parse_known`
and renders typed pending entries with the existing queue renderer. Truth does
not parse private harness JSON. The obsolete independent `TitleRead` adapter stays
removed. Explicit titles remain authoritative. No API schema, provider protocol,
FE or scanner change is introduced by this attempt.

Issuance persists pre-drain pending, writes the transcript projection, then
persists the drained queue. Independent source-first reads can miss both sides.
Pending-first alone also fails: [first-turn replace](../crates/calm-server/src/harness/run_loop/replace_command.rs)
atomically deletes the transcript suffix and persists replacement pending.
One transaction sees either complete generation and retains it across all
candidate batches, including long invisible prefixes.

## Restored-steer durability (r8 preserved)

The real reset route starts with `goal: None` and `first_message: None`, and its
checkpoint clears transcript. Existing owner tests start at that permitted
boundary, issue a real system-only task-completion observation, receive
`turn/started`, durably enqueue a user input, and accept steer. That steer can be
the first visible user input. `TrackGoal` is intentionally user-presented and is
not substituted for the system-only observation.

[The restoration owner](../crates/calm-server/src/harness/run_loop.rs) re-buffers,
persists the typed snapshot through `persist_snapshot_inner`, checks its actual
written bool, then deletes projections. Error or unwritten/shutdown results keep
the only durable projection. Caller phase/head persistence and announcements
remain unchanged. This r8 fence is not rewritten here. The existing fixture-only
SQLite trigger captures pending at actual deletion, and a real snapshot-write
error proves projection retention through the production metadata entry point.

Separate pre-existing follow-up evidence remains in `handle_steer`: a failed
`insert_projection_row` is logged, stored as `row_id: None`, and the queue without
the entry is still persisted. Restoration's `row_id.is_none()` branch leaves it
delivered. This is source evidence, not a new injected-failure claim or an expanded
repair. No provider/lifecycle policy change is included.

## Deterministic scheduling and costs

[SQLite regressions](../crates/calm-truth/src/db/sqlite/conversation_title/snapshot_tests.rs)
use the actual repo entry point and documented connection-local SQLite
PROFILE/STMT/CLOSE instrumentation. Observers are installed on the real repo's
pooled connections without replacing the pool or release hooks. CLOSE frees the
callback context. The already-locked direct SQLite dependency remains dev-only.

For WAL drain, atomic replacement and multi-batch deletion, the first candidate
SELECT notifies a writer task. The task acknowledges launch and attempts its
write; the callback lets the metadata read continue, and its commit releases the
writer. Tests assert the complete old source/runtime state, await writer completion,
and assert the complete new state afterwards. They retain all product assertions;
only the invalid demand for a commit inside the immediate read was removed.

A separate autocommit negative control composes the same production candidate and
runtime helpers without a transaction. Its first SELECT waits for the WAL writer
commit, then demonstrates both missing source and drained pending. It documents
the old inconsistency independently of the new serialized schedule. Historical
r8 mutations also removed only the transaction and failed exactly drain, replace,
and multi-batch tests under the old legal autocommit schedule:
`/tmp/title-r8-snapshot-mutation-result.json`.

Shared-cache tests cover reader-first atomic replacement and reverse writer-first
runtime-update/transcript-delete. In the latter, STMT observes metadata at BEGIN
before it can take table locks; the writer finishes its reverse-order writes,
commits, and releases metadata to return the complete new state. Cancellation is
exercised both at begin and after the first SELECT; SQL-valid structured JSON with missing typed fields
exercises the real decoding-error return. Each checks subsequent writer eligibility
and every pooled connection's transaction state. Empty ids work even with a closed
pool. Timeouts diagnose stalled handshakes/deadlocks only; no sleep, retry-until-green,
or timing threshold selects a result.

A normal nonempty request costs one standard immediate transaction, one candidate
SELECT and one runtime batch, independent of card count while candidates fit the
first batch. Runtime facts cover all requested ids, even when persisted input
already suffices (previously such cards skipped runtime queries). Invisible
histories use additional bounded 32-row batches, without a history cutoff or full
history materialization. **A long invisible prefix keeps writer eligibility for
all those queries and CPU decoding, delaying other writers and metadata reads.**
There is no upper latency guarantee or separate reader concurrency claim.

The cost regression records actual SELECT count, SQLite PROFILE query nanoseconds,
and elapsed BEGIN-success-to-COMMIT writer holding time for zero and 1,024 invisible
candidates. It asserts query counts, never elapsed-time thresholds. PROFILE timing
has SQLite's resolution and excludes Rust decoding; the holding measurement
includes decoding and scheduling. The final restored run measured:

| Invisible prefix | SELECTs | SQLite query time | Writer eligibility held |
| --- | ---: | ---: | ---: |
| 0 | 2 | 0ms (below PROFILE resolution) | 1.157275ms |
| 1,024 | 34 | 5ms | 16.114474ms |

Source: `/tmp/title-r9-restored.log`. Earlier successful samples varied (the
1,024-prefix run held eligibility for 24.50–27.59ms); no sample is discarded to
select a performance outcome. They are local evidence, not a production performance promise.

Restoring a nonempty steer group adds one existing snapshot persistence before
deletion in addition to caller persistence. Failed persistence can temporarily
leave pending and projection overlapping; existing issuance replaces stale
projections by entry id. No background cleanup or generalized retry is added.

Ordinary new tests, files and input variants need no permanent name list. Changing
the candidate SQL prefix can require updating its observer. Each gate check has
one owner: worker focused runs; kernel quick/text/contract/FE checks plus intended
focused replays; CI broad suites. No broad worker suite, new reviewer, live provider,
or preview is part of this task.

## Preserved history and evidence

The r8 failure410597 record was read in full before edits. It preserves the r5
invalidation work, r6 ownership reconstruction, r7 drain evidence, scanner
parent-link optimization, and their actual failure/verification limits. Original
31-file history and both canonical #2789 ownership trailers remain on their actual
changing commit. No upstream replay or frozen FE path edit occurs here.

Historical r8 transfer mutation bypassed only the pending-durability fence and
failed exactly the durable-before-delete and failed-persist-retention tests, then
restored byte-for-byte and passed the full owner filter:
`/tmp/title-r8-transfer-mutation-result.json`. Those production branches and tests
remain unchanged. Historical FE results are not this attempt's gate results.

## Executed verification

All Rust runs use `env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6`, `--locked`,
`--no-tests fail`, and `--test-threads 8`. The truth `--lib conversation_title` filter passed 15/15 on the final test tree,
then the single-factor mutation changed only production `begin_immediate_tx` back
to deferred `.begin()`. The predicted complete red set was:

- `db::sqlite::conversation_title::snapshot_tests::conversation_title_shared_cache_reverse_writer_first`
- `db::sqlite::conversation_title::snapshot_tests::conversation_title_query_and_lock_cost`

All 15 tests ran with `--no-fail-fast`: exactly those two failed, the other 13
passed. Safe byte restoration then passed 15/15 again. Evidence:
`/tmp/title-r9-{final-baseline,mutation,restored}.log`,
`/tmp/title-r9-mutation-prediction.json`, and
`/tmp/title-r9-mutation-result.json`. The cost assertion requires observation of
the immediate BEGIN/COMMIT boundary, not a time limit. Original autocommit loss is
independently proven by the negative control and the preserved r8 red/mutation
records. The new serialized tests do not claim they would force a WAL commit
inside a deferred snapshot.

The real SQLite concurrency/connection filter
`cargo nextest run --locked -p calm-truth --lib -E 'test(/deadlock_semantics_|pool_repair/)'`
passed 8/8 (`/tmp/title-r9-sqlite.log`). The existing tests retain their original
semantic schedules; no sleep or retry was added by this repair.

Worker focused runs also passed:

- `-p calm-server --lib conversation_title`: 4/4 (`/tmp/title-r9-owner.log`),
  including both preserved restored-steer durability tests.
- `-p calm-server --test api_suite conversation_title`: 7/7
  (`/tmp/title-r9-api.log`), with original product assertions intact.
- `-p calm-server --test planner_harness_suite -E 'test(/planner_steer::.*restor/)'`:
  4/4 (`/tmp/title-r9-restoration.log`).

`cargo fmt --all`, `git diff --check`, byte comparison of restored production
against its pre-mutation backup, Markdown link resolution, and FE/scanner equality
against the original changing commit passed. The final diff is limited to this
document, the title transaction entrance, and its SQLite test module.

The declared kernel gate replays these exact target/filter pairs:

- `-p calm-server --test api_suite conversation_title`
- `-p calm-truth --lib conversation_title`
- `-p calm-server --lib conversation_title`
- `-p calm-truth --lib -E 'test(/deadlock_semantics_|pool_repair/)'`
- `-p calm-server --test planner_harness_suite -E 'test(/planner_steer::.*restor/)'`

It also owns `scripts/local-rust-gates.sh --quick --base origin/main`,
`scripts/local-ratchet-gates.sh`, and `scripts/local-contract-gates.sh`.
From `fe`, it owns `npm run lint && npm run build` and these Vitest commands:

```sh
./node_modules/.bin/vitest run --project web-dom web/src/app/router/track-conversation.test.tsx
./node_modules/.bin/vitest run --project browser web/src/app/router/conversation-title.browser.test.tsx
./node_modules/.bin/vitest run --project platform-independent core/domain/conversation.test.ts
./node_modules/.bin/vitest run --project platform-independent tools/architecture/architecture.test.ts
./node_modules/.bin/vitest run --project platform-independent core/events/invalidation-plan.test.ts
./node_modules/.bin/vitest run --project web-dom web/src/app/providers/queries.contract.test.ts
```

The worker report carries the frozen gate verdict and full log reference. This
local verification does not claim Planner review convergence or remote CI green.
