# Conversation title read consistency

Review tier: L2, because this change crosses a persistence read boundary and
repairs the title source transfer inside the harness owner. Formal independent
reviews remain Planner-owned.

## Final ownership and read design

[ConversationTitleContext](../crates/calm-truth/src/conversation_title.rs) returns
persisted input candidates and projectable runtime facts. The required collections
are not optional. Absence of visible input remains distinct from an attachment-only
input whose derived title is null.

[The SQLite title owner](../crates/calm-truth/src/db/sqlite/conversation_title.rs)
opens one deferred read transaction for a nonempty request. Its first candidate
SELECT establishes the SQLite snapshot. Every subsequent 32-candidate batch and
the projectable runtime batch use the same transaction connection, then commit.
An empty request returns empty facts without acquiring a connection. There is no
cache, generic lock, history cutoff, per-card transaction, or in-memory snapshot
claim. The candidate SQL, indexed traversal, and visibility decoder are reused.

[The projectable runtime helper](../crates/calm-truth/src/db/sqlite/session_projection.rs)
accepts a connection and shares the existing query builder and row decoder with
the pool entry point. Eligibility still follows the card's current session pointer
and excludes superseded rows. It does not search retired carriers. Persisted input
without any runtime remains valid.

[The server owner](../crates/calm-server/src/conversation_metadata.rs) consumes
this context, prefers persisted visible input, and only then parses the opaque
runtime value with `HarnessSnapshot::parse_known` and renders typed pending entries
with the existing queue renderer. Truth does not interpret private harness JSON.
The obsolete `TitleRead` adapter and independently callable title-source repo
method are removed. Explicit titles remain authoritative in their existing
callers. No API schema, provider protocol, frontend, or scanner change is needed.

## Why one snapshot is required

Issuance persists the pre-drain snapshot, drains memory, writes its transcript
projection, then persists the issuance outcome. Two independent source-first
reads can see no projection before the insert and empty pending after the outcome.
A pending-first reorder alone is insufficient: [first-turn replace](../crates/calm-server/src/harness/run_loop/replace_command.rs)
commits transcript suffix deletion and replacement pending together. Independent
reads can combine pre-commit empty pending and post-commit empty transcript.

The new read transaction sees either committed state in both cases. It also holds
one snapshot across candidate batches, so a concurrent deletion cannot erase a
visible input between the first and second candidate SELECTs.

## Restored-steer transfer

The title read fix cannot repair a committed writer state containing neither
source. The restored-steer sweep previously deleted its projection, rebuffered
memory, then relied on a caller to persist pending later.

The real reset path in `routes/planner_cards.rs::start_harness_card` supplies
`goal: None` and `first_message: None`; the start adapter initializes an empty
queue when there is no inherited input and deletes the old transcript in its
checkpoint transaction. The fixture begins at this permitted context boundary.
The regression uses the existing harness fixture to issue a real task-completion
observation through the dispatcher, durable enqueue, and issuance owner. This is
a system-only input, unlike `TrackGoal`, which intentionally presents as user
input. A real `turn/started` notification followed by durable user enqueue and
accepted steer makes that steer the first visible user input. No provider service
or production pause hook is used.

[The harness restoration owner](../crates/calm-server/src/harness/run_loop.rs)
now re-buffers entries, persists their typed snapshot through the existing
`persist_snapshot_inner`, checks that the runtime was actually written, and only
then deletes their projections. Snapshot failure or an unwritten runtime retains
the durable projection. The callers keep their existing phase/head-stamping
persistence and announcements. A crash during the overlap leaves at least one
source. This is a title lifecycle transfer repair, not a generic lifecycle or
provider change.

An independent pre-existing defect remains: successful steer logs a failed
`insert_projection_row`, records `row_id: None`, and still persists the queue
without the entry. The restoration sweep explicitly leaves such entries delivered.
The exact production branches are in `handle_steer` and
`restore_steered_entries_codex_dropped`. These source facts are verifiable, but
this work does not claim an executed failure-injection reproduction of that
separate delivery defect and does not alter its policy.

## Deterministic evidence and mutation plan

The original r7 checkpoint was independently rerun before edits:
`/tmp/title-r8-initial-red.log` contains 2 selected server-lib tests, 1 pass and
1 drain failure (`None` versus `Some("Migrating input")`). The original evidence
is failure408903 and `/tmp/title-r7-owner-red-final.log`.

[Storage regressions](../crates/calm-truth/src/db/sqlite/conversation_title/snapshot_tests.rs)
call the real `RepoRead::conversation_title_context`. SQLite's documented
`SQLITE_TRACE_PROFILE` callback stops the fixture reader after the first candidate
SELECT while another WAL connection commits the writer transaction. Replacement
and suffix deletion use the same `transcript_delete_thread_suffix_tx` helper as
the real replace owner, paired with `session_set_handle_state_tx` in one transaction. A channel
acknowledgement proves the commit precedes return to the next read. Timeouts only
fail a broken handshake; they never retry or choose an outcome. `SQLITE_TRACE_CLOSE`
releases the callback context. The direct SQLite dev dependency adds no production
hook or production dependency.

The independent-read implementation failed exactly the drain, atomic first-turn
replace, and multi-batch consistency tests; the other four truth title tests
passed (`/tmp/title-r8-context-red.log`). The consistent implementation passed all
seven (`/tmp/title-r8-context-green.log`). The replace test checks both source and
runtime facts, detecting a mixed generation even when persisted-first rendering
would hide that mixture in the displayed string. Test snapshot payloads in truth
are opaque markers; only server tests parse real harness snapshots.

[Harness transfer regressions](../crates/calm-server/src/harness/run_loop/conversation_title_tests.rs)
use a fixture-only SQLite trigger to capture the actual durable snapshot at
projection deletion, and inject a real snapshot write failure to check retention.
They assert through the existing metadata entry point as well as typed snapshots.
The same owner test module preserves typed pending rendering, drained rendering,
and persisted-first precedence through the real metadata entry point. It reuses
the existing harness fixture, real issuance, and projection writer instead of
retaining a second repository fixture or a double-read adapter.

Critical mutations are run exclusively against production code, with the entire
corresponding `conversation_title` filter and `--no-fail-fast`. The snapshot
mutation removes only the transaction boundary; its predicted red set is exactly
the three storage schedule tests. The transfer mutation bypasses only durable
pending persistence before projection deletion; its predicted red set is exactly
the transfer boundary and failed-persist retention tests. Every mutation is safely
restored from a byte backup, followed by the same green filter and residue check.
The complete predicted and actual red sets, exit codes, and byte-equal restore
checks are saved in `/tmp/title-r8-{snapshot,transfer}-mutation-result.json`.

## Costs and maintenance

A nonempty title request adds one deferred read transaction and one runtime batch
query, including requests whose cards all already have persisted visible input
(previously zero runtime queries there). Mixed or missing-input requests still
have one runtime batch. Runtime facts now cover all requested ids. Candidate
query count remains the existing bounded traversal count; normal cards require
one candidate batch plus one runtime batch, independent of card count. No history
page endpoint or full-history materialization is introduced. Many invisible
candidates still require multiple batches and can retain a WAL snapshot longer;
there is no timing guarantee or permanent performance threshold.

Restoring a nonempty group of dropped steers adds one existing snapshot persistence
before deletion, in addition to the callers' existing final phase/head persistence.
Failed persistence deliberately retains the projection; a later pending snapshot
may temporarily coexist with it. Existing issuance replaces stale projections by
entry id. No background cleanup, general retry, or new durability protocol is added.

Future input variants use the existing renderer/visibility contracts. Ordinary new
tests or files do not require editing a permanent case list. The SQLite observer
identifies the owner candidate SELECT prefix; changing that SELECT shape may require
updating its test observer. The new dependency is test-only. The existing oversized run-loop core is changed
only in its restoration helper; new regressions live in separate small modules,
without moving unrelated lifecycle code. Kernel verification
runs the truth and server title filters once each, alongside existing API, text,
contract, Rust quick, and FE checks; worker runs focused checks and the intended
mutation replays. CI continues to own broad suites.

## Preserved history

The original 31-file reconstruction, r5 scoped invalidation, scanner parent-link
optimization and ordered diagnostics fixtures remain in the original changing
commit. Its two canonical #2789 ownership trailers remain bound to the actual
frontend changes. The r5 gate-target typo, later ownership-audit failure, corrected
r6 reconstruction, r6 fresh-list failure408447, and r7 drain investigation were
read from their own-track run records. No upstream replay or new frozen-path
change is part of this repair. Historical FE/scanner results are not counted as
this attempt's verification.

## Executed focused verification

All Rust commands unset `NEIGE_CODEX_BIN`, set `RUSTC_WRAPPER=` and
`CARGO_BUILD_JOBS=6`, and use `--test-threads 8 --no-tests fail`.

- `cargo nextest run --locked -p calm-truth --lib conversation_title`: 7 passed.
  Removing the read transaction failed exactly the three predicted storage tests;
  the restored filter passed 7/7. Logs: `/tmp/title-r8-snapshot-mutation.log` and
  `/tmp/title-r8-snapshot-restored.log`.
- `cargo nextest run --locked -p calm-server --lib conversation_title`: 4 passed.
  The old transfer implementation failed the two transfer assertions, with both
  metadata assertions green (`/tmp/title-r8-transfer-red-final.log`). Bypassing
  the durable pending fence failed exactly those two again; safe restoration
  passed 4/4 (`/tmp/title-r8-transfer-{mutation,restored}.log`). Retention covers
  both a SQLite write error and the existing unwritten/shutdown result.
- `cargo nextest run --locked -p calm-server --test planner_harness_suite
  -E 'test(/planner_steer::.*restor/)'`: 4 passed, covering stopped and failed
  turns, restored revision/delivery, and provider-recorded input not restored.
  Log: `/tmp/title-r8-steer-recovery.log`.
- `cargo fmt --all` and `git diff --check` passed. Workspace Markdown links were
  checked against the actual files.

The original server API first-input assertion is retained. The kernel gate owns
its API replay, the title-filter replays, Rust quick, text/contract checks and the
existing frontend visibility/invalidation/scanner/browser regressions. Its final
verdict and the focused steer recovery result accompany the worker report; broad
suites, new formal reviews, live providers and previews are outside this worker.

The metadata regressions share the existing harness fixture and production
issuance/projection writer. The storage replacement regression also uses the
production suffix-delete helper. This removes duplicate fixture writes while
keeping the retiring-vocabulary baseline unchanged at 298 occurrences in crates.
The consolidated server filter passed 4/4
(`/tmp/title-r8-refactored-owner-green.log`), and both affected mutation filters
were replayed to their exact predicted red sets followed by green restorations.
