# Conversation title read consistency investigation

The two-read ordering repair is insufficient across the existing lifecycle. This
is an investigation checkpoint for Planner disposition, not a completed fix.
Review tier remains L2: the alternative crosses a persistence read boundary.

The original 31-file reconstruction and its two ownership trailers remain in the
original changing commit. No upstream replay or frontend/scanner change is needed.

## Verified source evidence

- [Metadata owner](../crates/calm-server/src/conversation_metadata.rs) originally
  queries persisted title sources, then projectable pending snapshots only for
  cards without a persisted visible input. The decoder and indexed source query
  already own input visibility and attachment semantics.
- [Issuance](../crates/calm-server/src/harness/run_loop.rs) persists the pre-drain
  snapshot at line 3419, drains memory at 3421, writes the transcript projection
  at 3547, then persists issuance outcome at 3593. The projection's real
  `harness_item_insert` is at 2829. A source read before that insert and pending
  read after outcome persistence can miss both. The outcome writer at 4117 also
  covers a retired carrier; it does not reverse that order.
- Successful steer inserts its projection at 1646 before persisting queue removal
  at 1677. A projection-write failure is logged and queue removal still persists;
  read ordering cannot manufacture a failed write.
- Enqueue durably writes its snapshot/binding before acknowledgement (663;
  `persist_snapshot_for_durable_send`). Edit/delete apply the queue mutation and
  persist its snapshot before emitting the event (1497–1549), rolling memory back
  on persistence failure. A deliberate deletion may legitimately remove a title.
- Restored steers run the reverse transfer: `restore_steered_entries_codex_dropped`
  deletes the projection at 1828, collects entries, then re-buffers them at 1861.
  The caller persists the snapshot afterwards (for example 2283–2284). This is a
  genuine storage interval with neither source for that entry; even one read
  transaction alone
  cannot close that writer interval if it observes it.
- [Replace](../crates/calm-server/src/harness/run_loop/replace_command.rs) stages
  the replacement queue at 112–113, builds its snapshot at 121, then commits deletion
  of the transcript suffix (239) and the replacement snapshot (254) in the same
  transaction. For a first-turn replacement, pending-before-commit is empty and
  persisted-after-commit is empty although each committed state has visible
  input. Reversing the two independent reads therefore introduces a null window
  for this path. This is decisive independently of the restored-steer interval.
- [Start/reset](../crates/calm-server/src/operation/planner_harness_start_adapter.rs)
  writes the runtime snapshot and deletes transcript for clear in its checkpoint
  transaction (1382–1397). Replace owns its rewind cut; late rewound turn frames
  are rejected by the run-loop rewind fence. Clear/rewind can intentionally
  remove inputs; they must not be treated as a reason to remember old titles.
- [Projectable selection and harvest](../crates/calm-truth/src/db/sqlite/session_projection.rs)
  keep the card-backed projectable eligibility, excluding superseded carriers;
  harvest moves old queues in the successor transaction (378 onward). Title
  reads must continue using that selection, rather than scanning retired rows.
  Persisted-only input with no runtime remains valid under the existing query.

## Deterministic reproduction

[Owner tests](../crates/calm-server/src/conversation_metadata/tests.rs) call the
real metadata entry point through a two-method read adapter. Both methods call
real SqlxRepo queries and return those exact results. After the first read, the
adapter commits a real projection, then commits a typed empty HarnessSnapshot
using `session_set_handle_state_tx`. There are no sleeps, retries, provider calls,
copied naming rules, or production pause hooks. The setup asserts no original
persisted source; the final assertions verify empty persisted pending and require
that the title survives. A second test pins persisted-first precedence.

The narrow `TitleRead` interface in this checkpoint only delegates those existing
queries; the original source-first read order is deliberately retained for the
red reproduction. This checkpoint is not a candidate to merge.

## Proposed boundary for a new task

Planner should authorize an owner-specific consistent batch read in calm-truth
that reads both persisted candidates and the existing projectable snapshot query
on one SQLite read transaction/connection. Reuse the indexed candidate traversal,
visibility decoder and projectable query builder. Do not add domain policy to a
generic repo, scan all history, broaden runtime eligibility or add a global lock.
The server remains responsible for typed HarnessSnapshot/queue decoding and
persisted-first selection. Preserve explicit titles and attachment-only nulls.

A consistent transaction resolves both the drain interleaving and the atomic
replace interleaving. It does not by itself fix restored-steer deletion preceding
pending persistence, or failed steer projection writes: the new task must decide
whether those writers need an atomic transfer and add real lifecycle-owner tests.
Whether a restored steer can be the first visible user input also needs a real
lifecycle-owner reproduction; the writer interval is established by source, not
claimed here as an executed title regression. The restore writer evidence must
be assessed before promising a universal fix.

Cost: the simple pending-first proposal adds one batch snapshot query for requests
whose cards already all have persisted input (previously zero); mixed/missing
requests still use one snapshot batch, now covering all requested ids. It does
not add one read per card. A consistent-read design should preserve this constant
batch overhead plus the existing bounded indexed candidate traversal, with one
short read transaction per nonempty metadata request. Future inputs/tests do not
require editing a permanent list or pin. No new check should run per card.

## Verification status

Actual execution results and log paths accompany the worker failure report.
Green, mutation and kernel gates are not claimed: storage-boundary disposition
is required before implementing the complete fix. Existing frontend, title
query/decoder and scanner behavior has not been rewritten.

Executed from the checkout root on the final formatted reproduction:

```sh
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 \
  cargo nextest run --locked -p calm-server --lib conversation_title \
  --no-tests fail --test-threads 8
```

The clean final log is `/tmp/title-r7-owner-red-final.log`: compilation succeeded,
2 selected tests ran, precedence passed and
`conversation_title_drain_between_reads_never_loses_input` failed with
`None` versus `Some("Migrating input")` at the owner assertion. Exit status was
100. An earlier completed run had the same red set. Initial test-fixture compile
errors (typed ids used as strings) were fixed before these executions. Rustfmt
and `git diff --check` passed. No green fix or mutation was attempted after the
source sweep established that the proposed order was insufficient. All twelve
kernel gate steps and the calm-truth replay remain unrun for this attempt.
