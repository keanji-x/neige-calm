# Events retention runbook (#36 / #854 slice 2)

Operator checklist for the `events` table retention pruner
(`calm_truth::events_prune`, spawned by `AppState::new`). Incident context:
production hit 214k rows / 1.7GB, 99.1% of rows in four transient kinds,
causing `database is locked` stalls and slow cold WS replay.

## What the pruner does

Hourly background pass that deletes rows matching ALL of:

* **exact-kind allowlist** — `claude.hook`, `codex.hook`,
  `harness.phase.changed`, `harness.item.added`, `harness.queue.changed`,
  `overlay.set`. Everything
  else (all `card.*` / `track.*` / `area.*` / `terminal.*` structural kinds,
  and `overlay.deleted` tombstones) is permanent by construction;
* **age horizon** on `at` — default 30 days (floored at 1 day, see Knobs);
* **keep-latest carve-out** — the newest `overlay.set` per
  `(plugin_id, entity_kind, entity_id, kind)` quad is always kept, so the
  last-writer-wins overlay fold (server `derive_layout_positions`, frontend
  `useOverlayState`) is invariant under pruning. A quad whose latest row
  carries an unsupported (future) `schemaVersion` is skipped entirely —
  replay hides that row, so the older supported row must survive.

Every pruning DELETE also advances a durable **retention watermark**
(`retention_meta.events_prune_watermark` = highest `events.id` ever pruned,
updated in the same transaction). WS clients reconnecting with a cursor
below the watermark receive `_snapshot_required` instead of a replay with
interior holes — the `MIN(id)` check alone can't detect those, because
structural events are permanent and the earliest id never advances.

Overlay history grouping and schema interpretation run outside SQLite's writer.
The pass captures the overlay append frontier before discovery, then refreshes
only newly appended overlay rows before each deletion batch. Scan pages accumulate
actual old candidates across sparse protected history; they do not repeat full
history discovery. Under `BEGIN IMMEDIATE`, each batch checks the current overlay
frontier and exact latest witnesses, rechecks candidate kind/age, and atomically
updates the watermark from actual deleted IDs. Unrelated event appends do not
invalidate overlay observations. Plain transient kinds retain their bounded DELETE.

Deletes remain bounded by the configured batch (default 5000), with a ~100ms yield
between full candidate batches. This bounds row work, not milliseconds: witness
reads, large payloads and storage contention still have costs. Three consecutive
overlay append races or a missing latest witness defer the remaining overlay work
to a later pass; prior committed batches and their counts/watermark remain valid.
Newly healed quads can conservatively retain previously skipped rows until that
next pass. Malformed rows and unsupported-latest quads remain protected.

This incremental protocol relies on the production event owner's immutable
AUTOINCREMENT appends. Dev replay `/dev/reset` can reset IDs and is booted through
the separate replay `NoBackgroundTasks` path; do not run this pruner concurrently
with a manual replay reset. The protocol does not claim an ID-reset/ABA fence.

## What you lose after the horizon

Pruning `claude.hook` / `codex.hook` rows older than the horizon is an
**accepted regression** for two production consumers that replay them from
genesis:

1. **Track-fs hook transcript** — `hook_events_for_card`
   (`crates/calm-truth/src/track_fs_view.rs`): a card's hook transcript
   projection no longer includes hook events (either kind) older than the
   horizon.
2. **Harness recovery catch-up** — `replay_harness_events_since`
   (`crates/calm-server/src/harness/mod.rs`): boot recovery replays
   both hook kinds (among others) above the harness push watermark; a
   watermark older than the horizon cannot recover hooks that were pruned.

Turn-start VCS transcript refresh prepares both formats from each visible card's
latest 500 hook rows outside the writer. Before atomic publication it rechecks
the current visible card IDs and each exact ordered selected hook-ID vector.
This catches retention holes and cap backfill even when the maximum ID and
retention watermark stay unchanged. Only hook bytes are staged: the current HEAD
and nonhook content remain authoritative, so unrelated commits do not force a
retry. Three input races abort that refresh without publication; the existing
harness timeout/failure path can issue the turn without a refreshed override.
Unchanged trees still produce refresh commits. The 500-row cap bounds event count,
not payload bytes; this does not change hook retention or dirty paths on each hook.

Both consume diagnostics-grade data; >30-day-old hook history is not needed
for correctness of live tracks. `harness.phase.changed` / `harness.item.added`
history and superseded `overlay.set` writes carry no state the folds need.

## Triggers — when to look

* DB file size growing past ~500MB.
* `database is locked` in calm-server logs.
* Slow cold WS replay / slow boot reconcile.

## Inspection

```sh
sqlite3 "$DB" "SELECT kind, COUNT(*) AS n FROM events GROUP BY kind ORDER BY n DESC;"
sqlite3 "$DB" "SELECT MIN(id), MAX(id), COUNT(*) FROM events;"
ls -lh "$DB"*
```

## Knobs

| Env var | Default | Meaning |
| --- | --- | --- |
| `NEIGE_EVENTS_PRUNE_INTERVAL_SECS` | `3600` | Pass interval. `0` disables the pruner. |
| `NEIGE_EVENTS_RETENTION_SECS` | `2592000` (30d) | Age horizon on `at`. Floor: `86400` (1 day) — smaller values clamp with a `warn!` so a secs-vs-days typo can't wipe all history. `0` falls back to the default (it is NOT a disable switch). |
| `NEIGE_EVENTS_PRUNE_BATCH` | `5000` | Rows per delete transaction. |

Starter profiles:

* **Default** — leave everything unset: on, 30-day horizon, hourly.
* **Aggressive (constrained boxes)** — `NEIGE_EVENTS_RETENTION_SECS=604800`
  (7 days).
* **Disable** — `NEIGE_EVENTS_PRUNE_INTERVAL_SECS=0`.

## Verifying it runs

The pruner logs one `info!` line per completed pass (including passes that
prune nothing, so liveness is checkable right after the first interval
elapses — note it deliberately skips the boot tick):

```
events_prune: pass complete pruned_total=… pruned_by_kind=… batches=… duration_ms=… horizon_ms=… events_earliest_id=…
```

Failed batches log `events_prune: batch failed` warnings and the pass
continues with the next kind. Misconfigured knobs (unparseable values,
sub-floor retention) log `warn!` lines at boot — the pruner stays ON with
safe defaults; only `NEIGE_EVENTS_PRUNE_INTERVAL_SECS=0` disables it. Cross-check with the inspection queries above:
allowlisted kinds should hold a bounded row count once steady state is
reached.

## Retry records are not event history

The event pruner does not reclaim Track-create bindings or keyed operations.
`track_create_idempotency` rows remain after a Track is deleted: removing a
binding would allow an old request key to create another Track. Operations with
a non-NULL `idempotency_key` are also permanent; migration 0093 enforces this with
a `BEFORE DELETE` trigger because deleting them can redeliver a first message.
Do not drop that trigger or delete retry records to reduce database size.

A retry whose bound Track was deleted returns **409 `idempotency_key_exhausted`**.
Use a new key to create a new Track; the new frontend replaces the exhausted
draft key for the next submission. Transport failures and ordinary 5xx errors
still require preserving the original key because the create may have committed.
See the [idempotency retention decision](design-1428-idempotency-retention.md)
for the permanent-record policy and its storage tradeoff.

## Reclaiming disk space

The pruner never VACUUMs. Freed pages are reused by new appends, so the file
size plateaus rather than shrinking. To actually shrink the file:

1. Back up the DB file.
2. Run `neige admin vacuum --force`.

**Warning:** VACUUM takes a full-database lock for its duration — run it
off-peak, never during active use.

## Pointers

* **#33 per-actor retention (future)** — the policy's `Vec<RetentionRule>`
  is the seam; additional rules keyed on an actor prefix slot in without
  touching the pass loop.
* **Postgres note** — storage is SQLite-only today
  (`crates/calm-truth/src/db/sqlite.rs`). A future PG backend needs its own
  query implementation preserving the same retention and replay contracts;
  its autovacuum also changes the space-reclaim procedure.
* **WS replay safety** — two triggers in `run_replay`
  (`crates/calm-server/src/ws/events.rs`): a cursor below
  `events_earliest_id - 1` (head of the log gone — slice 1 protocol) OR a
  cursor below the durable retention watermark (interior rows pruned by
  this pruner) receives `_snapshot_required` instead of a gappy replay.
  The watermark is re-read after the replay window is materialized and
  before the first frame streams, closing the race with a prune batch
  committing mid-replay; and the `_replay_complete` cursor floors at the
  watermark so a pruned TAIL (watermark above the live tip) cannot strand
  clients in a re-snapshot loop (`replay_complete_stamp`).
