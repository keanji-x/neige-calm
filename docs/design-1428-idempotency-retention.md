# Decision: #1428 — retention and reclamation for idempotency state

Branch `feat/1428-idempotency-retention`, based on `origin/main` (`2819dce9`).
Closes KNOWN GAPS 4, 8 and 10 of `docs/design-1384-track-idempotency.md`.

Every load-bearing claim below cites `path:line` read in this worktree at that
commit, or an experiment run here. Claims I could not verify are under
**OPEN QUESTIONS**; ground I did not cover is under **KNOWN GAPS**.

This is a **policy** document. Two of the three items resolve to *"do not build
the reaper, and here is the construction that says why"*; the third is a
15-line behaviour change that unlocks a recovery path the frontend already has.

---

## 0. The three decisions, in one line each

1. **Binding rows: no reaper, ever.** The row's absence is indistinguishable
   from "fresh key", so reaping it is `#1384`'s original bug on a timer. The
   growth it buys is ~370 bytes per *successful keyed track create* — measured,
   §1.3 — and it is bounded by track-mint volume, not by request volume.
2. **A deleted track's key answers 409 `idempotency_key_exhausted`, not 500.**
   Still fail-closed — it mints nothing — but it names the dead track and says
   "use a new key", and `fe/core/domain/track.ts:506` already rotates the draft
   key on exactly that code. No operator DB surgery, no binding deleter.
3. **`operations` rows carrying a non-NULL `idempotency_key` are permanent**,
   enforced by a `BEFORE DELETE` trigger (migration), not by a comment. This is
   a **correctness** obligation, not the behaviour change #1384 §6.4 called it:
   reaping a *succeeded* keyed row makes the next byte-identical replay
   **deliver the first message a second time** (§3.1).

---

## 1. Binding rows are never reclaimed (gap 10)

### 1.1 The complete reader/writer set

Grepped whole tree, `./target` excluded:

| role | site |
|---|---|
| writer (only) | `crates/calm-truth/src/db/sqlite/track.rs:640` `track_create_idempotency_claim_tx`, called from `crates/calm-server/src/routes/tracks.rs:2121` inside the create transaction |
| reader (pool) | `crates/calm-truth/src/db/sqlite/track.rs:683` `track_create_idempotency_get_pool`, via trait `crates/calm-truth/src/db/mod.rs:955`, impl `crates/calm-truth/src/db/sqlite/out_of_domain.rs:174` |
| reader (route) | `crates/calm-server/src/routes/tracks/create.rs:528` (`first_message` path) and `:630` (message-less path) |
| **deleter** | **none anywhere in the tree** |

`track_delete_tx` does not touch it: `crates/calm-truth/src/db/sqlite/track.rs:515-560`
cascades `track_vcs_refs`, `track_vcs_commits`, `task_ref_index`, `tasks`,
`worker_sessions`, then `tracks` — and stops. No `deleted_at`/`is_deleted`/
`trash` column exists in any migration, so a track delete is a hard delete.

### 1.2 What a reaper would do, as an interleaving

The retention window a reaper needs is *"how long can a client still retry
under this key"*. Nothing in the system bounds that, and the failure when the
bound is wrong is not a degradation — it is the bug the table exists to stop.
Reap the binding row for key `K` at time `T`; a byte-identical retry arrives at
`T+ε`. `select_arm` (`crates/calm-server/src/routes/tracks/create.rs:227-233`)
is a two-input table, and both cells the retry can land in are wrong:

* **The operations row under the base key still exists** → `select_arm(false,
  true)` = `SelectedArm::BindingLost` → a permanent 500
  (`create.rs:561-569`) on a key that would have replayed a 201.
* **The operations row is absent too** — the variant-4 shape, where `validate`
  refused before `insert_operation` ever ran (`create.rs:186-194`) —
  → `select_arm(false, false)` = `SelectedArm::Mint`. A **second track**, and a
  **second delivery** of the first message. That is the measured `tracks=2,
  cards=4, operations=0` failure #1384 was opened for.

There is no window length that makes this safe, because the client's retry
clock is not the server's: a phone that slept through the window retries into
`Mint`. A reaper could only be safe if the binding row's *absence* were
distinguishable from "this key was never used" — and making it so means keeping
a tombstone, i.e. keeping the row. **So: no reaper.**

### 1.3 Quantifying the growth instead of asserting it is fine

Measured here, not estimated: a `WITHOUT ROWID` table with 0092's exact column
list, 100 000 rows of realistic width (32-hex ids, 64-hex digests) is a
36 356 096-byte database with `page_size = 4096`, `page_count = 8876` —
**≈ 364 bytes per row**. (`/tmp/b1428/m.db`, reproducible from the schema in
`crates/calm-truth/migrations/0092_track_create_message_less_binding.sql:21-56`.)

Round to **370 B/row** to cover a 36-char dashed UUID key.

| keyed creates | binding table |
|---|---|
| 1 000 / year | 0.37 MB / year |
| 10 000 / year | 3.7 MB / year |
| 100 000 / year | 37 MB / year |
| 1 000 000 total | 370 MB |

The load-bearing property is **what it takes to write one row**. The row is
written inside the create transaction (`routes/tracks.rs:2121`), so every row
costs a committed track, two cards, a folder claim and a materialized
workspace. A client cannot cheaply inflate this table: a million binding rows
means a million tracks, and the `tracks` + `cards` + workspace bytes behind them
dwarf 370 MB. Binding-row growth is therefore not a distinct capacity concern —
it is a rounding error on top of the growth it accompanies.

Contrast the two tables that *do* have reapers, and why they differ:
`events` (`crates/calm-truth/src/events_prune.rs:79`, 30-day default) and
`track_vcs_commits` (`crates/calm-truth/src/track_vcs/gc.rs:16`, keep-50). Both
grow per *interaction* — many rows per track, unbounded while a track lives.
`track_create_idempotency` grows per *track mint*. Different growth class,
different answer.

### 1.4 Schema change: **none.**

---

## 2. A deleted track poisons its key (gap 4)

### 2.1 Today

`adopt_prior_track` (`crates/calm-server/src/routes/tracks/create.rs:862-880`)
is the shared half of both resuming arms. Its `track_get` miss is
`CalmError::Internal` — a 500 whose text is
`"track {} recorded by an earlier attempt under this Idempotency-Key no longer
exists"`. The comment above it (`create.rs:869-874`) is correct and stays:
answering 201 here would mint a replacement under a key that already means
"that track".

### 2.2 What changes, and what does not

**Keep fail-closed. Change only the code and the text.**
`CalmError::Internal(...)` → `CalmError::IdempotencyKeyExhausted(...)`, naming
the deleted track id and saying "retry under a new `Idempotency-Key`, which
mints a fresh track".

This is not a new shape — it is the shape the *sibling* refusal twenty lines
below already uses. `create.rs:906-935` maps a failed `materialize_workspace`
onto `IdempotencyKeyExhausted` with the reasoning written out: the key is
per-key poisoned, a new key derives a different managed path, and
`idempotency_key_exhausted` "already means *this key is used up; retry under a
new one*". Every word of that applies verbatim to a deleted track — more
cleanly, in fact, because a deleted track cannot come back, whereas an
unmarked workspace directory theoretically could.

It is also what #1427 and #1458 did for the analogous poisoned-workspace case,
per `docs/design-1384-track-idempotency.md` §9.5 and §9.13: keep the fence, make
the refusal say which path is dead, document "use a new key" as the escape.

### 2.3 Why this *is* the operator affordance, and a binding deleter is not

The escape already exists and is free — a new key misses the binding
(`create.rs:528`), takes `Mint`, and derives a managed path from a fresh id. The
gap was never "there is no escape"; it was **"the answer does not say so, and
nothing acts on it."** A 500 says "the server is broken", so:

`fe/core/domain/track.ts:503-514` — `trackCreateKeyAction`:

```
if (failure.code === 'idempotency_key_exhausted') return 'replace';
...
return 'preserve';                     // ← every 5xx lands here
```

and the doc comment states the rule: *"Transport errors and 5xx may have
committed, so replacing their key could mint a second track."* The new-track
route mints its key once per draft, deliberately
(`fe/web/src/app/router/public.tsx:1841-1861`), and the **only** in-place
replacement it performs is on the structured exhausted code (`:1852-1856`,
#1435). So today a user whose track was deleted is pinned to a dead key until
they reload the page; after this change, their next submit carries a fresh key
and works. **Zero frontend lines.**

A binding-row deleter (an admin MCP tool, or a `neige` subcommand beside
`track-gc` / `vacuum`, `crates/neige-cli/src/main.rs:1198`) would introduce the
tree's first `DELETE FROM track_create_idempotency` to buy a recovery the client
already gets for free — and a destructive operator command whose only argument
is "an idempotency key someone typed" is the worst possible input for one. Not
proposed.

### 2.4 Blast radius

`IdempotencyKeyExhausted` is already in the OpenAPI code list
(`crates/calm-server/src/error.rs:24`, `:246`), already 409, already handled by
both frontends (`fe/core/domain/track.ts:506`,
`fe/core/domain/conversation.ts:459`). `adopt_prior_track` runs on both resuming
arms and strictly *before* anything is submitted, so no operation state
changes. The refusal is reachable only by a request that already matched the
binding's create fingerprint (`ensure_binding_create_matches`,
`create.rs:531-537`), so a mismatched body still gets its 409 `conflict` first.

One doc line needs updating with it: `create.rs:995-999` currently enumerates
`resume_message_less`'s answer set as "201 / 500 when the track was deleted /
409 exhausted", and the 500 becomes a 409.

### 2.5 Schema change: **none.**

---

## 3. `operations` retention must not degrade the replay arm (gap 8)

### 3.1 The obligation is stronger than #1384 §6.4 states

§6.4 says a reaped operations row makes a replay "treated as `GenuineRetry` and
re-derive `cwd` from the live track — safe (no second track, **no double
delivery**)". **The second half is false**, and here is the chain, each link
read in this worktree:

1. `retryable_operation_key` returns the first key that is absent or non-`Failed`
   (`crates/calm-server/src/routes/conversations_shared.rs:90-101`). Reap the
   succeeded base row → the base key reads absent → it is returned.
2. `select_arm(binding_hit = true, chosen_is_occupied = false)` =
   `SelectedArm::GenuineRetry` (`create.rs:230`).
3. `GenuineRetry` checks the message against the binding and passes — the
   message *is* byte-identical (`create.rs:581-585`).
4. `resume_prior_attempt` submits `plan.text` under `plan.operation_key`
   (`create.rs:964-976`) — the first message, on **both** arms.
5. `OperationRuntime::submit` short-circuits only when
   `find_by_idempotency_key` finds an existing row
   (`crates/calm-server/src/operation/driver.rs:135-142`). The row was reaped,
   so it does not; `validate` then `insert_operation` then `drive`.
6. `PlannerHarnessStartAdapter::validate` takes the non-minting branch
   (`create_card` is `None` for a track create), which requires the planner card
   to **exist** — it does — and passes
   (`crates/calm-server/src/operation/planner_harness_start_adapter.rs:628-700`).
7. `prepare_tx` pushes `Observation::UserMessage { text }` onto the pending
   queue unconditionally — there is no "already enqueued" predicate on this path
   (`planner_harness_start_adapter.rs:899-907`) — supersedes the active runtime
   (`:945-951`) and writes a second `harness.user_message.enqueued`
   (`:983`).

**So the operations row is the double-delivery wall for a keyed create**, and
`submit`'s collision check is the mechanism. Delete it and the same sentence is
sent to the agent twice. That is a correctness hole, and it means the reaper
obligation cannot be discharged by a note.

(The `cwd`-freeze degradation §6.4 does describe is also real and also follows —
`PriorArm::Replay` vs `GenuineRetry`, `create.rs:957-963` — but it is the
smaller half.)

### 3.2 The criterion, and why it cannot match a row outside itself

**Criterion: `operations.idempotency_key IS NOT NULL` ⇒ permanent.**

That is not a heuristic about which kinds matter; it is the exact predicate the
dedup wall reads. `submit` consults `find_by_idempotency_key(kind, &key)` for
**every** operation kind (`driver.rs:135`), and
`OperationRepo::find_by_idempotency_key` matches on the `idempotency_key`
column. So:

* every row with a non-NULL `idempotency_key` is, by construction, a live
  short-circuit for any future `submit` under that `(kind, key)` — its deletion
  changes behaviour, whoever wrote it;
* every row with a NULL `idempotency_key` can never be found by that lookup, so
  its deletion cannot change any `submit` decision.

The criterion is therefore exactly co-extensive with "is this row a dedup wall",
with no over- or under-reach to argue about. A future reaper for unkeyed
operations (worker spawns and the like) remains possible with **no migration**,
which is the ordinary shape retention would want anyway.

`idempotency_key` is nullable in the schema and always has been:
`crates/calm-truth/migrations/0029_operations.sql:5`,
`crates/calm-truth/migrations/0042_operations_parked.sql:6`.

### 3.3 A real mechanism, not a documented obligation

The tree contains **zero** deletes against `operations` — grepped whole tree,
`./target` excluded: every `FROM operations` hit is a `SELECT`
(`operation/repo_sqlite.rs:176,234,255,769`, `operation/mod.rs:888`,
`tests/support/agent_diag.rs:109`, `tests/support/event_queries.rs:70`) plus
0042's rebuild `INSERT…SELECT` (`0042_operations_parked.sql:91`). Nothing
breaks by forbidding one.

**Proposal: a `BEFORE DELETE` trigger.** New migration:

```sql
CREATE TRIGGER operations_keyed_rows_are_permanent
BEFORE DELETE ON operations
WHEN OLD.idempotency_key IS NOT NULL
BEGIN
  SELECT RAISE(ABORT, 'operations rows carrying an idempotency_key are the '
                      'submit() dedup wall and are permanent; see '
                      'docs/design-1428-idempotency-retention.md §3');
END;
```

Verified by running it (`/tmp/b1428/t.db`, sqlite3 on this box):

| statement | result |
|---|---|
| `DELETE FROM operations WHERE id='b'` (unkeyed row) | succeeds, row gone |
| `DELETE FROM operations WHERE id='a'` (keyed row) | `Error: ... (19)`, message shown, row survives |
| `DELETE FROM operations;` (bare, no WHERE) | **also aborts** — the presence of a row trigger disables SQLite's truncate optimization, so the trigger fires per row |
| `DROP TABLE operations` | **succeeds silently**, trigger goes with the table |

The bare-`DELETE` row is the one that matters: it is why this is a fence rather
than a tripwire. It cannot be evaded by spelling, by a query builder, or by
`DELETE` without a `WHERE`. It fires in whatever process holds the connection,
with a message naming this document.

**Its one hole, stated rather than hardened:** a future migration that rebuilds
`operations` the way 0042 did (rename → create → copy → drop) silently drops the
trigger, and the fence is gone with no failing test. Closed by a companion test
asserting the trigger is present in the head schema —
`SELECT name FROM sqlite_master WHERE type='trigger'` over a freshly migrated
database. `crates/calm-server/tests/cases/head_schema_fixture.rs` is the right
neighbourhood but the wrong assertion (it is a filename inventory, `:35-48`), so
this is a new test beside it, not an edit to that one.

### 3.4 Why a guard test alone is not enough, and why one is still wanted

The house precedent for "a retention policy must not eat a row somebody reads as
permanent" is `events_prune.rs:93-118`: an exact-kind allowlist, a *reverse
anchor* doc comment naming the reader that depends on permanence, and
`first_message_dedup_kind_is_never_prunable` (`events_prune.rs:967-992`) which
asserts both the constant and that an aged row of that kind survives a real
prune pass.

That shape works there because a reaper **already exists** and the allowlist is
the only door into it. Here there is no reaper, so an `OPERATIONS_PRUNE_KINDS`
constant would be a door nobody has to walk through — decoration until someone
chooses to read it. Hence the trigger, which does not depend on the next
author's cooperation. The guard test in §3.3 is the events-pruner test's
counterpart: it proves the fence is *installed*, which is the half that can rot.

### 3.5 Schema change: **yes** — one new migration, trigger only.

Landed as `crates/calm-truth/migrations/0093_operations_keyed_rows_are_permanent.sql`.
The number was re-checked against `origin/main` at implementation time (`git
ls-tree origin/main` — head was still `0092_track_create_message_less_binding.sql`,
and `main` had not moved from `2819dce9`). No applied migration was edited.

### 3.6 The sweep, and the executed `/dev/reset`

Both were run rather than reasoned about, because "nothing deletes from this
table" is a universal negative and a `DELETE FROM operations` grep is not one.

**Sweep** — whole tree, `./target` excluded, case-insensitive, covering
`DELETE FROM operations`, `DROP TABLE [IF EXISTS] operations`, `TRUNCATE`-shaped
spellings, `format!("DELETE …")` with an interpolated table name, and any wipe
loop driven off `sqlite_master`. **One hit:**
`crates/calm-truth/migrations/0042_operations_parked.sql:93`, the
rename → create → copy → drop rebuild — which runs long before 0093 installs
the trigger, and which SQLite would not fire a delete trigger for anyway. Every
other `FROM operations` in the tree is a `SELECT`. No test helper, fixture, e2e
reset path or `sqlx` scaffolding deletes from it.

**`/dev/reset`, executed.** `reset_from_fixture` (`calm-server/src/replay.rs:325-392`)
is the widest table-wipe in the tree and the engine behind the replay binary's
`POST /dev/reset`, which every Playwright `beforeEach` calls. Reading its
statement list and observing that `operations` is absent is not the same as
running it, so `dev_reset_survives_the_keyed_operations_fence` plants a keyed
`operations` row — the only state that can trip the fence — and executes the
reset over it. **It passes**, and the planted row survives, which is the correct
outcome: the reset never claimed to wipe `operations`. Mutation named on the
test: add `"DELETE FROM operations"` to the statement list and the reset aborts
on the planted row, which is the alarm the fence exists to raise.

---

## 4. Slice plan

Three independent slices; 2 and 3 can land in either order. Line counts are
production lines, excluding tests.

| # | scope | lands as | prod lines | tests |
|---|---|---|---|---|
| **S1** | gap 4: `adopt_prior_track`'s `track_get` miss → `IdempotencyKeyExhausted`; the answer-set enumeration on `resume_message_less`; the `POST /api/tracks` 409 description gains the deleted-track case | code + OpenAPI | **~20** | `a_replay_onto_a_deleted_track_is_key_exhausted` (the pre-existing 500 test, **inverted** rather than routed around) and `a_new_idempotency_key_recovers_from_a_deleted_track` |
| **S2** | gap 8: migration `0093`; reverse-anchor comments at `driver.rs`'s `submit` short-circuit and `retryable_operation_key` | migration + comments | **~25** (≈15 SQL, ≈10 comment) | `a_keyed_operations_row_cannot_be_deleted`, `an_unkeyed_operations_row_is_still_deletable`, `a_bare_delete_from_operations_also_aborts`, `head_schema_has_the_keyed_operations_fence`, `dev_reset_survives_the_keyed_operations_fence` |
| **S3** | gaps 10 + 8 + 4 recorded: `docs/design-1384-track-idempotency.md` §6.4 corrected (its "no double delivery" sentence was **false**, §3.1) and §9's gaps 4/8/10 closed with mechanism, in the house style siblings 1/2/3/5/13 already use | documentation | 0 | — |

**What stays a documented gap, deliberately:**

* **No binding-row reaper and no binding-row deleter** (§1.2, §2.3). Not
  "deferred" — decided against, with the constructions. If create volume ever
  makes 370 B/row matter, the thing to revisit is *track* retention, and the
  binding row follows its track's key, not a clock.
* **§3.3's rebuild hole** — a migration that drops and recreates `operations`
  must recreate the trigger. The head-schema test catches it; nothing forces the
  migration author to look before they push.

Total ≈ 45 production lines. This does not need a design phase per
`feedback_tiered_review_by_change_size`; S1+S2 go straight to impl + review.

---

## 5. OPEN QUESTIONS

1. **Real create volume.** §1.3's bytes/row is measured; the rows/year column is
   a table, not a claim. I could not sample a production database — no
   calm-server SQLite file was reachable under `/home/kenji`. The owner's actual
   keyed-create rate is the only missing input, and it changes nothing about the
   decision (there is no safe reaper at any volume) — only how the acceptance is
   phrased.
2. **`Idempotency-Key` has no length cap.** `parse_idempotency_key_header`
   (`crates/calm-server/src/routes/terminal_cards.rs:154-168`) rejects empty and
   non-ASCII and accepts everything else; the row width is therefore bounded by
   the HTTP header limit, not by the schema. Unimportant for capacity (§1.3: a
   row still costs a whole track) but it means "370 B/row" is a typical value,
   not a bound. Whether a cap belongs there is a separate question from this
   issue.
3. **Does any non-`POST /api/tracks` route depend on a keyed `operations` row
   surviving?** §3.2's argument covers `submit`, which is generic, so the answer
   does not change the criterion. But the *severity* of a future reap (double
   delivery vs. a harmless re-run) is per-kind, and I traced only
   `planner-harness-start` end to end.

## 6. KNOWN GAPS

* ~~**The trigger's interaction with the fixtures-only `replay` binary is
  untested.**~~ **Closed by execution** — see §3.6. It was the right thing to
  flag: the reset really is the widest wipe in the tree, and the answer came
  from running it over a planted keyed row, not from re-reading the list.
* **Multi-tenancy is untouched.** #1384 §9.9 stands: `(area_id,
  idempotency_key)` carries no principal. Both §2's 409 and §3's trigger are
  principal-blind, exactly as the code they sit next to is.
* **I did not audit the other keyed-operation routes** (the two conversation
  write mouths) for what a reap would do to *them*. §3.2's criterion protects
  them and §3.3's fence is table-wide, so the policy covers them; the impact
  narrative in §3.1 does not.
