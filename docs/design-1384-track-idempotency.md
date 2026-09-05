# Design: #1384 — `POST /api/tracks` safe retry: the key→track binding must be persisted in the mint transaction

Branch `feat/1384-track-idempotency`, based on `origin/main` (`6b12ba60`).
Every claim below cites `path:line` read in this worktree at that commit.

> Revision note (review round 1): §4.3's headline is now scoped to its actual
> carrier (§4.3.0), the `Resume` arm re-runs `materialize_workspace` (§4.4), the
> `create_request_sha256` carrier is named (§6.2), and the concurrency test
> moved to KNOWN GAPS because the in-process lock makes it vacuous (§9.3).
>
> Post-merge follow-up #1434 supersedes §6.2 and KNOWN GAP 7: migration 0089
> stores a versioned create-request fingerprint and initial-message digest in
> the binding row itself. The create digest now covers every mint input and is
> enforced across every operation attempt; only the message digest may change,
> and only after a persisted terminal `Failed` attempt selects a fresh `#N`
> operation key. Pre-0089 rows are represented explicitly as legacy-unknown and
> fail closed because their original request cannot be reconstructed.

---

## 1. Problem restated, in this codebase's terms

`POST /api/tracks` is `crates/calm-server/src/routes/tracks.rs:629`. Its write
sequence is:

| step | code | writes |
|---|---|---|
| body / first-message validation | `routes/tracks.rs:653-655` | none |
| `request.into_parts()` | `routes/tracks.rs:657` | none |
| pre-tx 4xx: template admission `:720`, `template_input` binding `:750`, `cwd` shape `:762-767`, attached workspace `:782-787`, area 404 `:801-806` | — | none |
| **the create transaction** (`BEGIN IMMEDIATE`) | `routes/tracks.rs:1464` → `create_track_structure` | folder claim (`:1475`), recipe read + 400 (`:1502-1506`), **`track_create_tx` (`:1515`) mints the id**, planner + report cards, overlays, report |
| `materialize_workspace` | `routes/tracks.rs:1792-1804` | filesystem |
| `start_planner_harness` | `routes/tracks.rs:1420-1428` | submits `planner-harness-start` |

The track id is minted by `track_create_tx` — `let id = new_id();`
(`crates/calm-truth/src/db/sqlite/track.rs:192`). It is **not** a function of
any request field. That is the whole difference from the two conversation write
mouths, whose card id is `sha256(scope, Idempotency-Key)` and therefore
recomputable.

So "which track did this key create" has to be *remembered*. Today the only row
that could remember it is the `operations` row, written by `insert_operation`
(`operation/repo_sqlite.rs:99-118`), which `submit` reaches only **after**
`adapter.validate` succeeds (`operation/driver.rs:126-137`).
`PlannerHarnessStartAdapter::validate` ends with

```rust
if !self.daemon.is_running() {
    return Err(self.daemon.not_running_error());
}
```
(`operation/planner_harness_start_adapter.rs:612-616`)

`is_running()` is `self.core.try_lock().is_ok_and(…Running…)`
(`shared_codex_appserver.rs:1285-1293`) — false while the supervisor holds the
lock, so a restart wobble produces it naturally.

**Variant 4 falls straight out of that ordering.** Daemon down ⇒ the track, its
two cards, its folder claim and its workspace are already committed, `validate`
refuses, no `operations` row exists, and the next request under the same key
finds nothing to adopt and mints a second track. Measured in the issue:
`tracks=2, cards=4, operations=0`.

Variants 1–3 are the *predicate* being wrong given a surviving operation row;
variant 4 is the binding not existing at all. The rejected daemon-preflight fix
addressed 4 by shrinking the window to one `create_track_structure`, and
introduced a regression (in-transaction 4xx became 500 during an outage). Both
facts are recorded in the issue; this design does not re-propose it.

---

## 2. Decision: **(A)** — persist the key→track binding in the mint transaction

**Pinned, with one refinement:** the binding is persisted by a **sidecar table
`track_create_idempotency`** written inside the *same* `BEGIN IMMEDIATE`
transaction as `track_create_tx`, not as a column on `tracks`. That is direction
(A) in substance — "the binding commits with the mint" — with strictly less
blast radius. §3 gives the shape and §2.3 the reason.

### 2.1 Why (B) loses: `pending` is not a reservation state

(B) proposes pre-reserving the `operations` row in `pending` inside the create
transaction and having `submit` advance it. Three independent findings kill it:

1. **`pending` already means "run me now".** `claim_drive_batch` selects
   `phase IN ('pending', …)` (`operation/repo_sqlite.rs:174-183`) and
   `drive_one`'s `Phase::Pending` arm immediately calls `prepare_tx_and_advance`
   (`operation/driver.rs:413-417`). Boot recovery does the same:
   `plan_recovery_for` maps `Phase::Pending` to `RecoveryItem::Recover`
   (`operation/driver.rs:1042-1047`), reached from `recover_on_boot`
   (`lib.rs:240`). A row parked in `pending` for the duration of a create
   transaction is not reserved; it is a live work item the driver will execute
   against a track that does not exist yet, and that a reboot will re-execute.
   Making (B) safe needs a **new phase** in the `CHECK (phase IN (…))`
   constraint (`migrations/0042_operations_parked.sql:13-24`) plus matching
   skips in `claim_drive_batch`, `drive_one` and `plan_recovery_for` — a schema
   change to `operations` *and* three changes to the generic driver.

2. **`insert_operation` is not transactional.** It takes `&self` and executes
   against `&self.pool` (`operation/repo_sqlite.rs:79-118`), not a
   `Transaction`. (B) needs a new tx-taking method on the `OperationRepo` trait
   (`operation/mod.rs`), implemented by `SqlxOperationRepo`
   (`operation/repo_sqlite.rs:61`) and two test doubles
   (`tests/no_double_spawn.rs:101`, `:224`).

3. **The blast radius is 13 production adapters**, all submitting through the
   same `Driver::submit`. `rg 'impl ProviderAdapter' crates/calm-server/src/`
   returns 14 impls; 13 are production, the 14th is `TestParkingAdapter`
   (`operation/tests.rs:1762`). The thirteen:
   `child-track` (`child_track_adapter.rs:166`), `claude-create`
   (`claude_adapter/mod.rs:382`), `claude-worker` (`claude_adapter/mod.rs:720`),
   `claude-restart` (`claude_restart_adapter.rs:101`), `codex-create`
   (`codex_adapter/mod.rs:273`), `codex-worker` (`codex_adapter/mod.rs:723`),
   `forge-action` (`forge_action_adapter/mod.rs:1247`),
   `planner-harness-interrupt` (`planner_harness_interrupt_adapter.rs:41`),
   `planner-harness-shutdown` (`planner_harness_shutdown_adapter.rs:52`),
   `planner-harness-start` (`planner_harness_start_adapter.rs:435`),
   `task-verify` (`task_verify_adapter.rs:595`), `terminal-create`
   (`terminal_adapter.rs:235`), `terminal-worker` (`terminal_adapter.rs:579`).
   Every one relies on `submit`'s contract that `validate` runs before a row
   exists (`driver.rs:126-137`) — for several, `validate` is the *only*
   admission check they have (`planner_harness_start_adapter.rs:451-618`).
   Moving insertion ahead of `validate` for one caller changes the invariant all
   thirteen are written against.

(B) buys nothing (A) does not. Its only advantage would be "no migration", and
it needs a bigger one.

### 2.2 Why (A) wins

The binding becomes a row written in the same `BEGIN IMMEDIATE` transaction as
the id (`routes/tracks.rs:1464`). **On the arm that writes it — `Mint`, i.e. a
create carrying a `first_message` and therefore an `Idempotency-Key` (§4.0) —
there is then no interval in which the track id exists and the binding does
not.** That is the exact property the operation row cannot have, because it is
written by a later statement on a different connection. On the `Legacy` arm no
binding is written at all and the property is not claimed; §9.1 is that gap. And it touches the operation subsystem's generic contract not at
all: no new phase, no new repo method, no change to `submit`, no adapter
affected.

### 2.3 Why a sidecar table rather than a `tracks` column

- `tracks` is read through an explicit column list `TRACK_SELECT_COLUMNS`
  (`crates/calm-truth/src/db/rows.rs:87-89`) and its aliased twin (`:94-97`),
  mapped onto `TrackRow` (`:100`). The consistency test at `rows.rs:171-188`
  explicitly says it does **not** check either constant against `TrackRow` or
  against the schema. A column nobody reads is dead weight on that surface.
- The binding needs three values: the track id **and** the planner and report
  card ids, so the resume arm re-derives nothing (§4.2).
- `track_create_tx` has 39 call sites (`rg 'track_create_tx\('`), the great
  majority tests. A new positional parameter is 39 mechanical edits for a value
  only one caller can supply. The sidecar writer is called by that one caller,
  inside the same closure; the other 38 sites are untouched.

The substance of (A) — *the binding commits with the mint* — is unchanged.

---

## 3. Schema / migration shape

**New file `crates/calm-truth/migrations/0088_track_create_idempotency.sql`.**
Never an edit to an applied migration: `sqlx` checksums the whole file including
comments, and editing one bricks startup with `VersionMismatch`. `0086` is the
current head.

```sql
CREATE TABLE track_create_idempotency (
  area_id          TEXT NOT NULL,
  idempotency_key  TEXT NOT NULL,
  track_id         TEXT NOT NULL,
  planner_card_id  TEXT NOT NULL,
  report_card_id   TEXT NOT NULL,
  created_at_ms    INTEGER NOT NULL,
  PRIMARY KEY (area_id, idempotency_key)
) WITHOUT ROWID;
```

- **Nullability.** All `NOT NULL`. The row exists only when all five facts are
  known — inside the create transaction, after `track_create_tx` returned and
  after the card ids were minted at `routes/tracks.rs:1449-1450`. There is no
  half-binding state, so the database refuses to represent one (same reasoning
  as `track_recipe_origin_is_whole`,
  `migrations/0085_track_recipe_provenance.sql:29-31`).
- **Uniqueness scope: per-area.** `PRIMARY KEY (area_id, idempotency_key)`. The
  area is the scope every neighbouring idempotent write mouth uses — the
  reference branch's own derivation is `sha256("track-create:{area_id}:{key}")`
  — and `area_id` is a required body field (`routes/tracks.rs:801-806`). It is
  also **immutable in production**: a sweep of the 17 `UPDATE tracks`
  statements outside `#[cfg(test)]` modules (`track.rs:383-398`, `:426`,
  `:433`, `:440`, `:447`, `:479`; `session_row.rs:31`, `:43`, `:87`;
  `session_mirror.rs:371`; `track_workspace.rs:46`, `:134`;
  `child_track_adapter.rs:308`; `scheduler/mod.rs:832`; `routes/today.rs:393`;
  `replay.rs:368`; `calm-truth/src/test_helpers.rs:19`) shows none sets
  `area_id`. The claim is exactly "**no production writer** sets it" — the
  writers of that column after insert are tests:
  `db/sqlite/task_projection_snapshot_tests.rs:235` and
  `crates/calm-server/tests/cases/track_workspace_recycle.rs:688`, the latter
  deliberately moving a track to a user area (`:685-692`). So no production
  path can orphan a binding row by moving a track between areas.
- **Pre-existing rows.** None. The table is new and empty; no backfill, no
  `ALTER TABLE`, and no existing track can be selected by a lookup.
- **`.sqlx/` offline metadata: not applicable.** No `.sqlx` directory exists
  (`ls .sqlx` → no such file) and no compile-time `sqlx::query_as!` / `query!`
  macro exists anywhere in `crates/` (`rg 'query_as!\(' crates/` → 0 hits).
  Every query is the runtime, string-typed form — which is exactly why a schema
  addition is a **runtime** failure mode and never a compile error.
- **No foreign key, no `ON DELETE CASCADE`.** Deliberate and fail-closed: if the
  track a key created has been deleted, a replay must not mint a replacement
  under a key that already means "that track". The binding row survives as a
  tombstone, `track_get` misses, and the handler answers 500 rather than
  201-with-a-different-track (the reference branch's `no longer exists` branch
  in `resume_prior_attempt`). The cost is in §9.4.

---

## 4. The recovery predicate

### 4.0 What carries it — read this before §4.3

The whole mechanism lives on the **`first_message` path only**, because that is
the only path that reads an `Idempotency-Key`: `plan_first_message` returns
`CreatePlan::Legacy` from its first statement when `first_message` is absent,
before the header is touched (reference `create.rs`, `plan_first_message`'s
`let Some(text) = first_message else { return Ok(CreatePlan::Legacy) };`).

**Pinned: the binding row is written on the `Mint` arm only, never on
`Legacy`.** A message-less create writes no binding row, reads no header, and is
byte-for-byte the pre-#1299 path. The alternative — writing the row on `Legacy`
too — is actively wrong: `Legacy` has already returned from the dispatch by
then, so there is no `Resume` arm to map a primary-key collision onto, and a
message-less same-key retry would turn a working 201 into an error.

**Consequence, stated rather than hedged: a message-less `POST /api/tracks`
remains non-idempotent.** A retry still mints a second track, exactly as it
always has. That is §9.1, not something §4.3 covers.

### 4.1 Inputs

Two lookups, in this order:

1. `track_create_idempotency_get(area_id, idempotency_key)` — new, one
   primary-key hit. **The new authority for "does a track exist for this key".**
2. `find_by_kind_and_idempotency(PLANNER_HARNESS_START, chosen_key)` — existing
   (`operation/driver.rs:156-171`), via `retryable_operation_key`
   (`routes/conversations_shared.rs:73-97`). Unchanged in role: it decides
   *which harness-start attempt* this request joins, and whether a `Failed`
   predecessor is stepped over with `#N`.

Today lookup 2 is asked to answer both questions, and it cannot answer the first
when `validate` refused before the row existed.

### 4.2 Arms

| lookup 1 | lookup 2 (on the chosen key) | arm | mints? |
|---|---|---|---|
| miss | — | `Mint` | yes: full request validation, then `create_track_structure` |
| hit | occupied (non-`Failed`) | `Resume` / `Replay` | no |
| hit | vacant `#N` after a `Failed` predecessor | `Resume` / `GenuineRetry` | no |
| hit | absent entirely (the variant-4 shape) | `Resume` / `GenuineRetry` | no |
| **miss** | **occupied** | **`CalmError::Internal`, fail closed** | **no** |

The last row was `Mint` in review round 0 and that **failed open**: `Mint` would
commit a track and its cards, and `insert_operation` would then raise
`idempotency_payload_conflict` on the unique violation
(`operation/repo_sqlite.rs:121-131`), leaving an orphan track behind a 409 —
precisely the failure class this issue exists to abolish. The state is
unreachable by construction (the binding commits strictly before the operation
is submitted), so the honest answer to reaching it is 500, not a mint.

The row carries `planner_card_id` and `report_card_id`, so `Resume` reads them
from the binding rather than from `payload.planner_card_id` (impossible in the
variant-4 shape — there is no payload) or from a role query. A role query
*would* be well-defined — the partial unique indexes
`idx_cards_one_planner_per_track` and `idx_cards_one_report_per_track` make
both single-valued — but re-deriving a value the mint already knew is a second
source of truth.

(The two index-creating migration files are cited by name in review round 2's
notes rather than here: naming them in this document raises two cells of the
#1316 terminology ratchet, whose whole purpose is to make retiring vocabulary
cost something to write down. The index names above are the load-bearing
citation and are stable.)

### 4.3 Failure-point enumeration

**Claim, scoped to its carrier (§4.0):** *for a `POST /api/tracks` carrying a
`first_message` and therefore an `Idempotency-Key`, at every possible failure
point between the request arriving at `create_track` and the response leaving
it, a same-key retry resolves to exactly one of: the same track, or no track at
all. Never a second track.*

The proof is a partition of `create_track`'s statements by the create
transaction's commit, because the binding row is written **inside** it.

| # | failure point | state after | same-key retry resolves to |
|---|---|---|---|
| FP1 | header parse / `validate_first_message` (`routes/tracks.rs:653-655`) | nothing written | `Mint`; **no track** |
| FP2 | pre-tx 4xx: `:720`, `:750`, `:762-767`, `:782-787`, `:801-806` | nothing written | `Mint`; **no track** |
| FP3 | inside the tx: folder claim `:1475`, recipe 400 `:1502-1506`, `track_create_tx` `:1515`, cards/overlays/report, **and the binding INSERT** | whole tx rolled back | lookup 1 misses ⇒ `Mint`; **no track** |
| FP4 | process death between `COMMIT` and `materialize_workspace` (`:1792`) | track + binding committed, workspace absent or half-built | lookup 1 **hits** ⇒ `Resume` ⇒ re-materialize (§4.4); **same track** |
| FP5 | `materialize_workspace` returns `Err` (`:1792-1804`) | track + binding committed | lookup 1 hits ⇒ `Resume` ⇒ re-materialize (§4.4); **same track** |
| FP6 | `submit` → `validate` refuses, daemon down (`planner_harness_start_adapter.rs:612-616`); no operation row, because `validate` precedes `insert_operation` (`driver.rs:136-137`) | track + binding, `operations` = 0 | lookup 1 hits ⇒ `Resume`; **same track**. *Variant 4.* |
| FP7 | death after `insert_operation` (`repo_sqlite.rs:99-118`), op `pending` | track + binding + `pending` op | boot recovery re-drives (`driver.rs:1042-1047`); retry: lookup 1 hits, `submit` finds the row by idempotency key (`repo_sqlite.rs:141-149`) ⇒ same op; **same track, one delivery** |
| FP8 | operation reaches `Failed` | track + binding + failed op | `retryable_operation_key` steps over `Failed` (`conversations_shared.rs:84`) ⇒ `#N` ⇒ genuine re-execution **against the same track** |
| FP9 | operation reaches `Stuck` (`driver.rs:349-360`) | track + binding + stuck op | `retryable_operation_key` does **not** step over `Stuck` (`conversations_shared.rs:84`) ⇒ same key ⇒ `submit` finds it ⇒ the recorded 500 replays; **same track, no second delivery** |
| FP10 | the 201 is lost on the wire | everything committed | lookup 1 hits ⇒ `Resume` ⇒ `submit` finds the `Succeeded` op ⇒ 201 + **same track** |

The rows are total over the handler because FP3's transaction boundary is the
only place a track id can come into existence (`track.rs:192` is the sole
`new_id()` for a track on this path), and the binding is on the same side of it.

**Why this is a class fix and not a narrowed window.** The rejected preflight
left an interval — preflight passes, daemon stops, `create_track_structure`
runs, `validate` refuses — in which FP6's state was reachable with no binding.
Here there is no such interval: FP4/5/6/7 differ only in how far past the commit
the failure got, and all are past the binding's commit, because the binding and
the id are the same commit.

### 4.4 `Resume` re-runs `materialize_workspace` — and why that is safe

The reference branch's `resume_prior_attempt` calls only `track_get` →
`start_planner_harness_with_first_message`: no materialization. Inherited
verbatim, FP4/FP5 would answer **201 for a track whose workspace does not
exist**, because `materialize_workspace` runs after the commit
(`routes/tracks.rs:1792-1804`) and its failure is deliberately propagated as
non-2xx — `:1783-1791` says why: `warn!` + `Ok(())` "returns 201 for a track
whose first codex worker will then die with `spawn-failed`, which is #1147
itself replayed one layer down". `PlannerHarnessStartAdapter::validate` does not
look at `cwd` either (`planner_harness_start_adapter.rs:451-618`).

**Pinned: option (a). `resume_prior_attempt` calls `materialize_workspace`
before submitting**, with the same `map_err` treatment as the mint arm.

It is safe because that function is *designed* to be re-run:

- `TrackWorkspaceKind::Attached` is an unconditional `Ok(())`
  (`workspace_materialize.rs:152-158`) — a no-op, so `Resume` on an attached
  track behaves exactly as the reference branch did.
- On `Managed`, the owner marker gates everything: marker present and equal to
  this track ⇒ *"Re-running the steps below is idempotent, and — because the
  marker proves we created everything here — it is also safe to repair a
  half-built directory left by a crash"* (`workspace_materialize.rs:339-342`).
- Steady state costs **one `rev-parse`**: `if !git_head_resolves(path)` guards
  the whole `init` block, and the comment names the reason the function must be
  cheap — *"the worker lease path calls this on every acquisition (red-team B5)
  purely so an un-materialized track repairs itself"*
  (`workspace_materialize.rs:374-380`). Re-running it here is a use the function
  already has in production.
- A crash mid-`init` is repaired, not bricked: the marker is written **before
  anything else** (`:365-371`, *"claim it before writing anything else, so a
  crash at any later point leaves a directory we can prove is ours and repair,
  instead of an unmarked non-empty brick"*), and `clear_our_stale_git_locks`
  (called at `:399`) removes a `config.lock` left by a killed process.

**The permanently un-materializable state IS reachable from a create crash.**
*(Closed by #1427 — see KNOWN GAP 5. The construction below was correct and is
kept because it is what motivated the fix; the code it cites has since changed.)*
Round 2 of this document claimed it was not, on the grounds that the window
between `create_dir_all` and `write_owner_marker` leaves an *empty* directory.
Reading `write_owner_marker` itself refutes that, and the refutation is recorded
here rather than argued away:

- The marker is `<path>/.git/<OWNER_MARKER>` (`workspace_materialize.rs:531-533`).
- `write_owner_marker` first `create_dir_all`s the marker's **parent**, i.e.
  `<path>/.git` (`:547-555`), and only then `std::fs::write`s the marker
  (`:556-561`).
- Process death between those two syscalls leaves `<path>` containing `.git/`.
  `dir_has_entries` counts any `read_dir` entry (`:564-579`), so it is true, and
  `read_owner_marker` returns `None` (`:535-545`). That is exactly the
  `None if dir_has_entries(path)?` arm (`:353-364`) ⇒ permanent `Internal`.
- The window is *inside* the function round 2 cited as closing it, and FP4 puts
  process death during materialization in scope.
- The codebase already treats this state as real rather than theoretical:
  `:413-416` re-asserts the marker after `git init` precisely to cover "the case
  where the marker was lost along with a partially wiped `.git`".
- A second, weaker construction: `std::fs::write` is not crash-atomic, so a torn
  marker yields `Some(owner) != track_id` and lands on the foreign-owner arm
  (`:346-352`) — the same permanent `Internal`.

**The fence is not relaxed.** An unmarked non-empty directory stays refused.
Allowlisting "the only entry is `.git/`" would be a marker-absence heuristic —
the shape this repository has been burned by — and no positive fingerprint is
available that a user's own bare or partially-initialised repository could not
also match. The refusal stands and `Resume` inherits it.

**The trade, in both directions.** Today that window produces a *second track at
a fresh path* and the user gets a **working** one. Under this design the key is
poisoned: every retry under it re-materializes the same dead path and answers an
error, forever. **That is a liveness regression in a narrow window, bought for a
correctness fix** — one key can no longer silently become two tracks, and the
price is that this one key can no longer become any track.

**The escape, verified: the poisoning is per-key, and a new `Idempotency-Key` is
a complete recovery needing no new machinery.** A new key misses lookup 1 ⇒
`Mint` ⇒ `track_create_tx` mints a fresh id
(`crates/calm-truth/src/db/sqlite/track.rs:192`) ⇒ the managed path is
`root/<area_id>/<track_id>` built from *that* id (`track.rs:256-264`), so it is
a **different directory** and the poisoned one is never revisited. Nothing pulls
the new attempt back onto the old path: a managed workspace is the `cwd_omitted`
branch, which takes `FolderClaim::Skip` (`routes/tracks.rs:829-831`), so no
`area_folders` row contends on it either. The only residue is a dead directory
on disk (§9.5).

**So `Resume` maps a materialization failure to
`CalmError::IdempotencyKeyExhausted`, not to a generic `Internal`.** That
variant is already 409 (`error.rs:272-283`) with its own code
`idempotency_key_exhausted` (`:246`), and its existing meaning — "this key is
used up; retry under a new `Idempotency-Key`" (`conversations_shared.rs:95-99`)
— is precisely the actionable instruction here. Reusing it widens the code from
"64 terminally failed attempts" to "this key can no longer produce a working
track"; that widening is deliberate and is the one behavioural change this arm
makes. An operator distinguishes it from a generic 500 by status **and** code,
and the underlying `materialize_workspace` message is carried in the body
verbatim so the dead path is named. Pinned by T-BRICK-1; the escape by
T-BRICK-2 (§8).

**What `Resume` still does NOT re-check, deliberately:**
`validate_attached_workspace` (`routes/tracks.rs:782-787`). Re-running it is
variant 3: a deleted attached directory turned a byte-identical replay into a
permanent 400. So for an **attached** track whose directory was removed, a
replay answers 201 and the workspace is broken — unchanged from today, pinned by
`a_replay_survives_the_attached_directory_being_deleted`, and **excluded from
every "safe retry" sentence in this document** (§6.3, §9.6).

---

## 5. Inherited-from-#1299 inventory

Reference branch `origin/feat/1299-s1-squashed` (three commits over merge-base
`8b6e46e9`; `routes/tracks/create.rs` is 770 new lines).

| piece | verdict | why |
|---|---|---|
| `select_prior` / `PriorSelection` three-arm table | **REUSED WITH CHANGES** | The criterion — "what sits on the *chosen* key, never the shape of its name" — stays. It gains one input: the binding-row hit. `FreshKey` now means "mint **iff** lookup 1 also missed", and the `miss + occupied` cell fails closed (§4.2). Its unit tests port with rows for the new input. |
| `predecessor_operation_key` + test | **REUSED AS-IS** | Pure; unaffected by where the binding lives. |
| `derive_track_create_operation_key` + the two namespace tests | **REUSED AS-IS** | The operation-key namespace must still not collide with `conversation_keys`. |
| `CreatePlan::{Legacy, Mint, Resume}` dispatch; `Resume` short-circuits before the create path's request validation | **REUSED AS-IS** | The structural statement that `Resume` cannot mint, and what keeps variant 3 fixed. Do not move validation before `Resume`. |
| `PriorArm::{Replay, GenuineRetry}` + the `cwd` freeze-vs-re-derive split and its field-by-field payload audit | **REUSED AS-IS** | Fixes variants 1 and 2. Independent of the binding. |
| `resume_prior_attempt` (signature takes neither `NewTrack` nor `CreateTrackOptions`) | **REUSED WITH CHANGES** | Signature invariant preserved — that is what makes "this arm does not mint" compiler-enforced. Two changes: track and card ids come from the binding row, not the operation payload (only `cwd` still comes from the payload); and it now calls `materialize_workspace` (§4.4). The reference branch's "`report_card_id` is `None` ⇒ 500" fail-closed check goes with the ids it guarded. |
| `plan_first_message`'s `same_key_claim` (`lock_card(&s.conversation_first_message_locks, &base_key)`, taken before the chain is read) | **REUSED AS-IS** | The map exists on main (`state.rs:171`, constructed `:250`) with its documented outer-lock ordering (`state.rs:144-146`). In-process half only — see §9.3. |
| Regression tests for variants 1–3 (`replaying_a_successful_create_…`, `a_replay_survives_the_track_being_repointed_in_between`, `a_retry_after_a_failure_uses_the_repointed_workspace`, `a_replay_of_a_success_that_happened_on_a_retry_key_survives_a_repoint`, `a_replay_survives_the_attached_directory_being_deleted`, `the_same_key_with_a_different_first_message_is_a_conflict`, `a_key_exhausted_by_64_failed_attempts_answers_409`) | **REUSED AS-IS** | Already mutation-verified on the reference branch. |
| `boot_with_daemon(bool)` / `boot_without_daemon()` fixture | **REUSED AS-IS** | Both constructors exist on main (`shared_codex_appserver.rs:832`, `:840`). Required: with a fake installed `is_running()` short-circuits to `true` (`:1286-1289`), so the outage is otherwise unconstructible. |
| `a_daemon_outage_does_not_mint_a_track_per_retry_under_one_key` | **REUSED WITH CHANGES** | Construction stays; the **numbers invert**. The preflight version asserted `tracks == 0`; this design asserts `tracks == 1` after two 500s. Asserting `0` would be asserting the dropped preflight. |
| OpenAPI four-arm contract prose incl. arm (b)'s exception clause | **REUSED WITH CHANGES** | Arms unchanged. Two edits: the `Idempotency-Key` requirement sentence, and the 500's wording (§6.3). Regenerating `fe/core/api/generated/openapi.json`, `web/src/api/openapi.json`, `web/src/api/generated.ts` is a gate requirement. |
| daemon preflight + `SharedCodexAppServer::require_running()` extraction | **DROPPED** | Both of the issue's reasons, each reproduced by two review channels: (i) it narrows a window rather than closing a class — the window is one `create_track_structure` wide and `is_running()` flips false on ordinary supervisor lock contention (`shared_codex_appserver.rs:1291-1292`); (ii) it regresses in-transaction 4xx to 500 during an outage (unknown `recipe_id` 400 at `routes/tracks.rs:1502-1506`, folder-claim 409 at `:1475`). This design needs no daemon check: FP6 resolves by adoption, not prevention. Not re-proposed. |
| `a_template_create_refuses_a_first_message` | **DROPPED** | Stale: `as_template` was retired by #1318 S2 and `create_track_with_planner_harness` now calls `start_planner_harness` unconditionally (`routes/tracks.rs:1406-1428`), so the refusal it pins has no code path. `plan_first_message`'s `as_template` parameter goes with it. |

---

## 6. Sub-decisions

### 6.1 The `SucceededViaCollision` arm

The comment on #1384 gives two independent grounds for today's fold at
`routes/tracks.rs:1896-1897`. Both re-checked:

- **Ground 1 — HOLDS today; this issue removes it.** `start_planner_harness`
  submits `idempotency_key: None` (`routes/tracks.rs:1866-1870`), and
  `find_by_idempotency_key` returns `Ok(None)` *without touching the table* when
  the key is absent (`operation/repo_sqlite.rs:141-149`).
- **Ground 2 — HOLDS today; this issue does NOT remove it.** The sole producer
  is `operation_result_from` (`operation/mod.rs:948-960`), which requires the
  persisted `phase_detail.completion == "idempotency_collision"`. `rg
  '"completion"' crates/` returns the reader itself (`operation/mod.rs:952`), a
  comment (`routes/tracks.rs:1885`) and two unrelated test literals
  (`operation/tests.rs:739`, `:773`). `rg 'idempotency_collision' crates/` adds
  only HTTP error-code strings (`error.rs:245`,
  `calm-types/src/error.rs:67`) — response codes, not `phase_detail` writers.
  **Nothing writes the key.**

**Consequence the issue comment does not state:** giving the create an
`Idempotency-Key` does not make the variant reachable. `submit`'s collision
short-circuit returns the *existing* op's id (`driver.rs:126-137`) and
`wait(&op_id)` reads that op's own durable row (`driver.rs:267-270` →
`operation_result_from`), whose `phase_detail` carries no `completion` key. The
outcome is plain `Succeeded`. The variant stays globally unreachable after this
issue lands, on ground 2 alone.

**Pinned decision — split the arm, but do not build the answer on the runtime
signal.**

1. **The route already knows.** "The message was delivered, but not by THIS
   request" is exactly what `CreatePlan::Resume` means, computed before anything
   is submitted. Replay semantics is decided from the *arm*.
2. **The folded match arm is split anyway**, into a pure
   `fn response_for(arm, outcome) -> Result<()>`, because the current fold's
   justification cites ground 1, which this issue invalidates, and a comment
   that says something false is worse than no comment. In it,
   `SucceededViaCollision` is `CalmError::Internal` on the **Mint** arm (a fresh
   key cannot collide) and success on the **Resume** arms.

Coverage, honestly: the variant stays unreachable, so the only non-vacuous test
is a unit test over `response_for`'s `(arm × outcome)` matrix with an
`OperationOutcome` constructed directly. No integration construction exists and
none is faked.

### 6.2 Which fields bind into `payload_hash`, and where the digest is computed

**Today.** `payload_hash` is `stable_payload_hash({"actor": actor.as_str(),
"request": &request})` (`routes/tracks.rs:1850-1853`) over
`PlannerHarnessStartOperationPayload`
(`planner_harness_start_adapter.rs:240-320`): `actor`, `track_id`,
`planner_card_id`, `report_card_id`, `sort`, **`cwd`**, `goal`,
the two reset/force-new-thread flags, `profile`, `create_card`, and (when
set) `first_message_sha256` / `first_message`. It covers **none** of the create
request's own fields, which is why the same key with a different `title`
silently returns 201 + the original track.

**Pinned delta.** One new field on `PlannerHarnessStartOperationPayload`:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub create_request_sha256: Option<String>,
```

shaped exactly like `first_message_sha256`
(`planner_harness_start_adapter.rs:291-292`) — never read by the adapter,
`skip_serializing_if` so every existing caller's payload bytes and therefore
`payload_hash` stay byte-identical, `serde(default)` because boot recovery
re-drives payloads written by older binaries.

**Carrier — the part review round 0 left unnamed.** The digest is over exactly
three fields, the three the issue names: **`title`, `template_id`,
`recipe_id`**, each a pure function of the request body
(`CreateTrackRequest::into_parts`, `routes/tracks.rs:244-247`, called at `:657`)
and none movable by a later `PATCH`.

- **Computed in `plan_first_message`**, which gains one parameter carrying those
  three values, cloned at the call site right after `into_parts()`.
- **Stored on `FirstMessagePlan`**, and therefore reachable from
  `ResumeFirstMessage { plan, prior }` as `plan.create_request_sha256`.
- **`resume_prior_attempt`'s signature is unchanged**: it still takes neither
  `NewTrack` nor `CreateTrackOptions`, so the compiler-enforced "this arm does
  not mint" invariant (§5) survives. A digest is a hash of three strings, not a
  mint input.
- **`template_id` is the caller's string as received, read before the admission
  overwrite at `routes/tracks.rs:740`.** Using `admission.key()` instead would
  be nicer semantics under a future case-folding admission rule, but it requires
  running `admit_template` (`:720`) *before* the arm decision — i.e. validating
  on the `Resume` arm, which is the variant-3 class. Recorded in §9.7.

**`cwd` is deliberately excluded** and stays governed by the `PriorArm` freeze
(§5): it is already in the payload and it *is* moved by `PATCH` (variants 1/2).
Double-binding it would re-bind the field this design specifically un-binds.

**Not moving validation.** `Resume` still runs no create-path request check.

Deliberately **not** bound: `template_input`, `attach_folder`,
`fork_report_from`, `sort`. `area_id` is covered structurally — it is in the
primary key, so a different `area_id` is a different binding. See §9.7.

### 6.3 The 500 on harness-start failure, and the characterization test

The 500 is `routes/tracks.rs:1965-1978`; the test is
`crates/calm-server/tests/cases/track_create_first_message.rs:891`
(`a_stuck_start_after_spawn_has_already_delivered_the_first_message`), whose
header block (`:857-874`) says "if #1384 lands, THIS TEST IS EXPECTED TO
CHANGE".

**Pinned answer: no. The endpoint still cannot say whether the message was
delivered. The unknown stays unknown.** The decisive fact is the first of the
issue's two:

`harness.user_message.enqueued` proves only an *attempt*. `prepare_tx` seeds the
observation and writes the audit row in a transaction that commits at
`TxCommitted`; the later `AppServerInteract` can still fail; `events` is
append-only and compensation only marks the runtime failed. The existing test at
`:806-816` documents exactly this and deliberately does **not** count the event.
There is no other durable record of the turn leaving, so no read the handler can
perform answers the question.

The second fact — the `Stuck` path has already sent the message and compensation
does not run (`drive()` captures `from_phase` before `drive_one` at
`driver.rs:351` and on any `Err` calls `mark_stuck`, `:352-360`, never
`fail_with_compensation`) — is what makes a *negative* claim a lie. Narrower
than review round 0 stated: `from_phase` is not a coin flip. Of the two ways
`Stuck { from_phase: SpawnStarted }` arises, the delivered one is the phase
write failing after a successful `spawn_side_effect` (`driver.rs:562-570`),
while every `spawn_side_effect` **error** routes to `fail_with_compensation` ⇒
`Failed` (`driver.rs:617-624`); the only not-delivered leg is
`required_output(&op)?` failing at `driver.rs:557`, which the phase sequence
makes an invariant violation. So `from_phase` is *strongly* correlated with
delivery — it just does not answer the question, because the delivery evidence
itself does not exist. The conclusion rests on the paragraph above, not on this
one.

**What this issue genuinely adds is the actionable half, and only for the two
things it can prove.** Retrying under the same `Idempotency-Key` (i) creates no
second track and (ii) delivers no second copy — FP9: the retry resolves to the
same `Stuck` operation (`conversations_shared.rs:84`) and replays the recorded
500. It does **not** promise the track is usable (§4.4's attached-directory
carve-out), and the wording must not say so.

Change to the test — **additive; every existing assertion stays**:

- keep `copies_in_harness == 1` under a 500;
- keep the positive assertion that the text reports an **unknown** delivery;
- keep both negatives (`!contains("not delivered")`, `!contains("send the
  message from the track itself")`);
- **add** a positive assertion that the text names the two proven properties
  (no second track, no second copy) and drops the now-false "the create is not
  retryable" clause;
- **add** a negative assertion that the text does not claim the track is
  healthy, since §4.4 shows it may not be.

The header block is rewritten to say why the unknown survived; the test is not
deleted, and no fix is invented that promises more than the server knows.

### 6.4 Retention / operations-row cleanup

**This design no longer depends on the operations row surviving for the "one key
⇒ one track" property.** If the row is gone, lookup 1 still hits, the arm is
still `Resume`, and the retry runs against the existing track. The `RetryAfter →
Mint` degradation the issue names is closed by construction.

It still depends on the row for two narrower things: the `Replay`-vs-
`GenuineRetry` `cwd` freeze (§5) and the `#N` exhaustion chain
(`conversations_shared.rs:73-97`). If the row were reaped, a replay would be
treated as `GenuineRetry` and re-derive `cwd` from the live track — safe (no
second track, no double delivery), but a byte-identical replay of a repointed
track would answer 201-with-a-new-op rather than replaying. A behaviour change,
not a correctness hole.

Factually there is **no pruner on `operations` today**: `rg 'DELETE FROM
operations'` over `crates/` returns nothing, and `routes/today_summary.rs:89`
records the same. The events pruner's allowlist is exact-kind and unrelated
(`calm-truth/src/events_prune.rs:110-116`). §9.8.

### 6.5 Multi-tenancy

Verified: `crates/calm-server/src/actor.rs:33` — *"this file is plumbing, not a
security boundary."*

The binding's primary key is `(area_id, idempotency_key)` — **no owner**, the
same shape the operation key already has. No privilege escalation is
constructible today because there is no authorization layer to escalate past,
not because the key is scoped well.

**The invariant that must hold when principals arrive** (KNOWN GAP, not built):
*the `Resume` arm must authorize the calling principal against the track the
binding names, before that track is returned, and the binding's uniqueness scope
must include the principal (or the key must be salted with it).* Without both
halves, principal B guessing principal A's key in a shared area is handed A's
track — a read, and on `GenuineRetry` a write, under A's identity. The second
half alone is insufficient: a scoped key stops new collisions but not rows
already written.

---

## 7. Slice table

**One PR.** The binding, the predicate that reads it, and the four regression
variants are one root cause; the migration without the predicate ships a table
nobody reads, and the predicate without the binding is the reference branch
verbatim — the thing that did not converge.

| # | goal | files | prod Δ (est.) | acceptance tests |
|---|---|---|---|---|
| 1 | the durable binding | `crates/calm-truth/migrations/0088_track_create_idempotency.sql` (new); `crates/calm-truth/src/db/sqlite/track.rs` (`track_create_idempotency_claim_tx` + `_get`) | ~90 | T-BIND-1, T-BIND-2 |
| 2 | plan/arm dispatch + resume | `crates/calm-server/src/routes/tracks/create.rs` (new, ported from the reference branch minus the preflight, with lookup 1 as the authority, the fail-closed `miss+occupied` arm, and `materialize_workspace` on `Resume`); `routes/tracks.rs` (handler takes `HeaderMap`, dispatches `CreatePlan`, writes the binding inside the create closure at `:1464` **on the `Mint` arm only** — `create_track_structure` is reached by both arms, since `create_track_with_planner_harness` (`routes/tracks.rs:1406-1428`) calls it with `first_message: None` too, so the write is conditioned on the plan rather than on reaching the closure; maps a primary-key unique violation to a fail-closed `Internal`) | ~560 | T-V1…T-V4, T-ARM-1, T-ARM-2, T-MAT-1, T-MAT-2, T-LEGACY-1 |
| 3 | payload-hash binding + the split outcome arm | `operation/planner_harness_start_adapter.rs` (`create_request_sha256`); `routes/tracks/create.rs` (digest in `plan_first_message`, carried on `FirstMessagePlan`; `response_for`) | ~100 | T-HASH-1, T-HASH-2, T-COLL-1 |
| 4 | contract prose + the 500's wording | `routes/tracks.rs` utoipa block (`:617-627`), `routes/tracks/create.rs` module docs, regenerated `fe/core/api/generated/openapi.json`, `web/src/api/openapi.json`, `web/src/api/generated.ts` | ~60 + generated | T-500-1 |

Estimated production delta ≈ **830 lines** (round 2: +50 for the `Resume`
materialization, the fail-closed arm and the digest carrier; round 3: +20 for
the `IdempotencyKeyExhausted` mapping and the `Mint`-only conditioning of the
binding write). Test delta ≈ **1400 lines**, mostly ported from the reference
branch's 1666-line suite. **Slice count unchanged: one PR.**

---

## 8. Acceptance tests

Route-level tests live in
`crates/calm-server/tests/cases/track_create_first_message.rs` (933 lines on
main, wired at `tests/track_suite.rs:10-11`). Existing infrastructure they build
on: `boot()` (`:71`) and its helpers `create_track`, `track_count`,
`card_count`, `copies_in_harness`, `user_message_event_count`,
`shutdown_harnesses`; `fail_next_thread_start_for_test` (used by
`a_failed_harness_start_fails_a_create_that_carried_a_first_message`, `:790`);
`reject_spawn_succeeded` (used by
`a_stuck_start_after_spawn_has_already_delivered_the_first_message`, `:891`).
`boot()` gains the reference branch's `boot_with_daemon(bool)` /
`boot_without_daemon()` split; `create_track` gains an `Idempotency-Key`
parameter.

| id | test | pins | mutation that must turn it red | lives in |
|---|---|---|---|---|
| T-V1 | `replaying_a_successful_create_returns_the_same_track_and_delivers_once` | variant 1 | make lookup 1 always return `None` in `plan_first_message` | `track_create_first_message.rs` |
| T-V1b | `a_replay_survives_the_track_being_repointed_in_between` | variant 1 + `PATCH` (`cwd` freeze) | in the `PriorArm::Replay` branch take `track.workspace.path` instead of `prior.cwd` | same |
| T-V2 | `a_replay_of_a_success_that_happened_on_a_retry_key_survives_a_repoint` | variant 2 | make `select_prior` read the `#N` suffix instead of `chosen_is_occupied` | same |
| T-V3 | `a_replay_survives_the_attached_directory_being_deleted` | variant 3 (arm decided before validation) | move `validate_attached_workspace` (`routes/tracks.rs:782-787`) ahead of the `CreatePlan` dispatch | same |
| **T-V4** | `a_daemon_outage_adopts_the_track_it_already_minted_under_one_key` | **variant 4**: daemon down, same key twice ⇒ `tracks == 1`, `cards == 2`, two 500s, no second delivery | delete the `track_create_idempotency` INSERT from the create closure (`routes/tracks.rs:1464` block) — the retry then mints and `track_count` reads 2 | same; fixture `boot_without_daemon()` via `SharedCodexAppServer::new_stub_with_pending` (`shared_codex_appserver.rs:832`), because `is_running()` short-circuits to `true` with a fake installed (`:1286-1289`) |
| T-V4b | `a_create_without_a_first_message_still_succeeds_during_a_daemon_outage` | control: the message-less path keeps `warn!` + 201 | make any daemon check reachable from the `Legacy` arm | same |
| T-LEGACY-1 | `a_message_less_create_writes_no_binding_row` | §4.0: the binding is written on the `Mint` arm only. Asserts `SELECT count(*) FROM track_create_idempotency == 0` after a message-less create sent **with** an `Idempotency-Key` header | remove the `Mint`-arm condition on the binding write in `create_track_structure`'s closure, so it also fires for `Legacy` — the count then reads 1 | same |
| T-MAT-1 | `a_resume_after_a_materialize_failure_materializes_the_workspace` | §4.4 / FP5: `Resume` re-materializes; the returned track's managed directory has a resolvable `HEAD` | delete the `materialize_workspace` call from `resume_prior_attempt` | same. **Construction:** create successfully with key K, then `std::fs::remove_dir_all` the managed directory (available in-process; `InitCommit::Skip` is private to `workspace_materialize` and unreachable from `tests/`), then replay K. Without the fix the replay 201s onto a directory that does not exist |
| T-MAT-2 | `a_resume_on_a_healthy_managed_workspace_is_a_no_op` | §4.4's idempotence premise: a replay leaves the owner marker byte-identical and the `HEAD` commit id unchanged | in `materialize_managed_workspace_inner`, drop the `if !git_head_resolves(path)` guard (`workspace_materialize.rs:384`) so `git init` + the initial commit re-run on every call — every other `Resume` test still passes (the directory stays valid) while this one sees a moved `HEAD` | same |
| T-BRICK-1 | `a_resume_onto_an_unmarked_non_empty_workspace_is_key_exhausted` | §4.4: the fence is not relaxed, and the answer is 409 `idempotency_key_exhausted`, not a generic 500 and not a 201 | map the materialization failure back to `CalmError::Internal` — the test's status/code assertion then fires | `track_create_first_message.rs`. **Construction:** create with key K, then `remove_dir_all` the managed directory and recreate it containing only an empty `.git/` (the exact residue of the `:547-561` window), then replay K |
| T-BRICK-2 | `a_new_idempotency_key_recovers_from_a_poisoned_workspace` | §4.4's escape: the poisoning is per-key. After T-BRICK-1's state, a create under a **new** key 201s with a working track at a different path | derive the managed path from `(area_id, idempotency_key)` instead of the minted track id (`track.rs:256-264`) — the new key then lands on the poisoned directory and this test 409s | same |
| T-BIND-1 | `the_binding_and_the_track_commit_together` | FP3: an in-transaction failure leaves neither | write the binding on a second connection instead of `tx` | unit test beside `track_create_tx` (`calm-truth/src/db/sqlite/track.rs:168`) |
| T-BIND-2 | `the_database_refuses_two_tracks_under_one_area_and_key` | the primary key is the cross-process wall | **widen** the PK to `(area_id, idempotency_key, track_id)` — keeps the `WITHOUT ROWID` DDL valid, so exactly this test reddens rather than every test that boots a DB | same |
| T-ARM-1 | `the_arm_is_decided_by_the_binding_then_by_what_sits_on_the_chosen_key` | §4.2's table, as a pure unit test | swap any row of the table | `#[cfg(test)]` in `routes/tracks/create.rs` |
| T-ARM-2 | `a_binding_miss_with_an_occupied_key_mints_nothing` | §4.2's last row: `Internal`, never `Mint` | make the `(miss, occupied)` cell resolve to `Mint` — the test then observes `track_count == 1` behind the 409 instead of `0` | `track_create_first_message.rs`, **not** the pure unit module: the claim is about what is written, so it is constructed by inserting an occupied operation row under the derived key with no binding row, then POSTing that key |
| T-HASH-1 | `the_same_key_with_a_different_title_is_a_conflict` | the durable binding's complete create-request digest | omit `title` from the binding digest assembled in `plan_first_message` | `track_create_first_message.rs` |
| T-HASH-2 | `a_message_less_create_writes_byte_identical_payload_json` | `skip_serializing_if` keeps old callers' `payload_hash` stable | remove `skip_serializing_if` from `create_request_sha256` | same (companion of `a_create_without_a_first_message_is_unchanged`, `:437`) |
| T-COLL-1 | `a_collision_outcome_is_a_success_only_on_a_resume_arm` | §6.1's split | fold `SucceededViaCollision` back into `Succeeded` in `response_for` | `#[cfg(test)]` in `routes/tracks/create.rs`; constructs the `OperationOutcome` directly — §9.2 says why there is no integration construction |
| T-500-1 | `a_stuck_start_after_spawn_has_already_delivered_the_first_message` (amended) | §6.3 | assert non-delivery in the 500 text (existing negatives fire), or drop the two proven properties (new positive fires) | `track_create_first_message.rs:891` |
| T-EXH-1 | `a_key_exhausted_by_64_failed_attempts_answers_409` | `MAX_OPERATION_KEY_ATTEMPTS` (`conversations_shared.rs:17`) still governs | raise the cap without updating the assertion | same |

Every mutation must be applied for real, and the verdict read from **which**
test went red, not how many.

---

## 9. KNOWN GAPS

1. **A message-less `POST /api/tracks` is still not idempotent** (§4.0). The
   header is not read on the `Legacy` path and no binding is written; a retry
   mints a second track, as it always has. Extending the mechanism there means
   dispatching on the header for every create, which changes the pre-#1299
   ordering for every existing caller — none of which sends the header today.
   Follow-up: *"make message-less track creates idempotent"*.
2. **The in-flight and `Stuck` arms are not covered.** The claim spans the
   whole submit-and-wait (`driver.rs:267-300`) and `planner-harness-start`
   never parks, so a second request cannot observe a first one mid-flight
   *through the route*, in one instance. Declared in the test module header;
   **no test pretends to cover them.**

   **Narrowed by #1430** (this said "not coverable in-process" and "needs a
   cross-instance harness"; both were too strong, measured):
   * No second OS **process** is required. The boundary that serializes two
     same-key creates is `conversation_first_message_locks`, a per-`AppState`
     field (`state.rs:174`, minted at `:268`), so two `AppState`s over one
     on-disk SQLite file in **one process** already race. `SqlxRepo::open`
     takes any URL and sets WAL + `busy_timeout` on every connection
     (`db/sqlite/mod.rs:241-260`); the shared-file spelling
     `sqlite://{path}?mode=rwc` is already in the tree
     (`tests/support/kernel_proc.rs:129-131`). `sqlite::memory:` cannot be
     shared — sqlx gives each parse its own named cache (`mod.rs:186`).
   * The **`Stuck` arm needs no harness at all.** `retryable_operation_key`
     stops on any non-`Failed` phase (`conversations_shared.rs:84`), so an
     `operations` row inserted directly under the derived key drives it in one
     instance — the technique `a_binding_miss_with_an_occupied_key_mints_nothing`
     (`track_create_first_message.rs:2217-2255`) already uses.

   What remains a gap, for a smaller reason: see gap 12.
3. **The cross-process primary-key race is not tested, and the mapping is
   fail-closed rather than recovering.** `plan_first_message` takes
   `lock_card(&s.conversation_first_message_locks, &base_key)` *before* the
   lookup and holds it through the mint, so two same-key creates in one process
   serialize and the second takes `Resume` without ever reaching the primary
   key. A test of the unique-violation mapping built on **one** `AppState`
   therefore passes with or without the mapping — vacuous, so it is not
   written. The mapping ships as a fail-closed `CalmError::Internal`
   (`routes/tracks.rs:1958-1984`; the losing racer's client retries and gets
   `Resume`), explicitly commented as unreachable within one instance. Same
   root cause as gap 2. Follow-up: *"cross-instance idempotency harness"*.

   **Narrowed by #1430**: this said "unreachable in one process" and pointed at
   a cross-*process* harness. Measured, the wall is the per-`AppState` lock map,
   not the process: **two `AppState`s over one on-disk database, in one
   process**, reach the primary key, and no second OS process is required. The
   DB-layer refusal is already pinned by T-BIND-2
   (`track_create_idempotency_tests.rs:137`) sequentially; what is unpinned is
   the route-level `map_err`, that the loser's transaction leaves no orphan
   track, and that its retry resolves to the winner. Making the interleaving
   deterministic still needs one injection point between lookup 1
   (`routes/tracks/create.rs:412`) and the mint — a `Repo` decorator is not the
   cheap way there (109 methods across the supertraits, one impl in the tree).
4. **A deleted track poisons its key permanently.** No FK, no cascade (§3): the
   binding row survives, `track_get` misses, and the key answers 500 forever.
   Deliberate — fail-closed beats minting a different track for a byte-identical
   request — but there is no operator affordance to clear it.
5. ~~**A crash inside `write_owner_marker` poisons one `Idempotency-Key`
   permanently.**~~ **CLOSED by #1427.** The gap as written was real and the
   §4.4 construction stood: death between `create_dir_all(<path>/.git)` and the
   marker write left an unmarked non-empty directory that the fence refuses
   forever, so every retry under that key answered 409
   `idempotency_key_exhausted`. #1427 made the claim crash-atomic —
   `claim_owner_marker` assembles `.git/<marker>` in a staging directory beside
   `<path>` and publishes it with one `rename(2)` over the (empty) `<path>`, and
   the marker's re-assertion after `git init` renames a sibling temp file rather
   than truncating the published one. There is no longer an intermediate state
   in which `<path>` is non-empty and unmarked, and the fence was **not**
   relaxed to get there. The §4.4 paragraphs below still describe the window
   accurately as of this document's writing; read them as history, not as
   current behaviour.
6. **A replay does not repair an attached workspace.** `Resume` deliberately
   does not re-run `validate_attached_workspace` (variant 3), and
   `materialize_workspace` is a no-op for `Attached`
   (`workspace_materialize.rs:152-158`). A replay of a track whose attached
   directory was deleted answers 201 and the workspace is broken — unchanged
   from today, and excluded from every "safe retry" sentence here.
7. **Resolved by #1434.** The durable binding fingerprint now covers
   `template_input`, `attach_folder`, `fork_report_from`, `sort`, `theme`, and
   the request's original `cwd` in addition to the three original fields. It
   still binds `template_id` as the **caller's string**, not the admitted roster
   key, because deriving it from mutable admission state would reintroduce the
   replay-validation failure this design avoids.
8. **Operations-row retention** (§6.4). No pruner exists today. If one is added,
   the `Replay`/`GenuineRetry` `cwd` distinction and the `#N` exhaustion chain
   degrade. Follow-up: *"operations retention must not silently degrade the
   track-create replay arm"* — against the retention work, not here.
9. **Multi-tenancy** (§6.5). `(area_id, idempotency_key)` carries no principal.
   The invariant to satisfy when principals arrive is stated there; not built.
10. **Binding rows are never garbage-collected.** One row per keyed create.
    Bounded by create volume, `WITHOUT ROWID` keeps it compact, unbounded in
    time.
11. **`routes/tracks.rs` is 4269 lines** and this slice adds to it. The bulk of
    the new code goes to `routes/tracks/create.rs` (the `tracks/` module
    directory already exists — `fork_guard.rs`), but the file stays far past the
    800-line governance target. Pre-existing; not addressed here.
12. **A *live* in-flight duplicate on a second instance is not pinned, and is
    not worth the machinery today.** Measured while closing #1430: over a
    directly-inserted `running` `operations` row, a second instance contributes
    exactly one thing — a live runtime future actually awaiting that operation.
    Buying it costs a `planner-harness-start` adapter that parks plus a
    two-`AppState` boot, to pin one response shape whose decision table T-ARM-1
    already pins (`select_arm`, `routes/tracks/create.rs:175-182`). Recorded as
    a gap with that reason rather than built. The `Stuck` half of gap 2 does
    **not** need any of this — see gap 2.
13. ~~**The ownership claim is crash-atomic but not concurrency-atomic.**~~
    **CLOSED by #1458.** The gap as written was real and #1430 measured it
    rather than inferring it: two claimers on one `<path>` shared the staging
    name `<parent>/.neige-claim-<track_id>`, so if one was between its fsyncs
    and its publishing `rename` when the other entered, the other's
    `remove_dir_all(<staging>)` deleted the assembled claim and its
    `create_dir_all` put a **bare, unmarked** `.git` back under the same name;
    the first then renamed *that* onto `<path>` and returned **`Ok`**, leaving
    `<path>` non-empty and unmarked — the exact brick state #1427 abolished for
    process death, reached instead through a peer.

    **The closing mechanism: the staging name is now unique per *attempt***
    (`<prefix><track>-<pid>-<nanos>-<seq>`), so no claimer can address, remove
    or recreate another's staging directory — the delete-**and-recreate** pair
    on one shared name was the whole construction. Both claimers assemble their
    own claim and the `rename` decides: the winner publishes over the empty
    `<path>`, the loser's rename finds it non-empty, fails `ENOTEMPTY` — the
    fail-closed direction the `rename` was chosen for — and returns `Internal`
    having written nothing to `<path>`. The fence was **not** relaxed, the
    marker's location and format are unchanged, and #1427's fsync ordering
    stands. The characterization test was **inverted, not deleted**:
    `a_concurrent_claim_cannot_make_its_peer_publish_an_unmarked_workspace`
    drives the identical interleaving and now asserts the marked publish and the
    fence's acceptance. The benign interleaving is pinned beside it as before
    (`a_claim_that_loses_the_staging_race_fails_closed_onto_the_winners_marker`),
    with the loser failing one step later — at the rename, not at a vanished
    staging marker.

    Reclaim, since the name no longer reclaims by being reused: a claim removes
    its **own** staging on every error path, and each claim sweeps this track's
    staging directories older than one hour (`reclaim_stale_claim_staging`).
    Residue is therefore what it was before — one directory per claim killed
    mid-flight by process death, cosmetic to the one reader that sees it — and
    it is not unbounded: it is bounded by that track's crashes within the sweep
    window, and, exactly as before, a track never materialized again keeps its
    debris.

---

## 10. Open questions for the owner

None. Both forks the issue left open resolved from code: (A) vs (B) in §2, and
the `SucceededViaCollision` arm in §6.1 (ground 2 survives this issue, which
changes what the split can be based on). Round 2's blocker resolved from code
(`materialize_workspace` is documented and used as a repeatable repair,
`workspace_materialize.rs:339-342`, `:374-380`, so `Resume` can call it), and
round 3's from code plus a construction (§4.4): the poisoned-key window is real,
the fence stays closed, and the escape is a new `Idempotency-Key`.
