# Events retention runbook (#36 / #854 slice 2)

Operator checklist for the `events` table retention pruner
(`calm_truth::events_prune`, spawned by `AppState::new`). Incident context:
production hit 214k rows / 1.7GB, 99.1% of rows in four transient kinds,
causing `database is locked` stalls and slow cold WS replay.

## What the pruner does

Hourly background pass that deletes rows matching ALL of:

* **exact-kind allowlist** — `claude.hook`, `codex.hook`,
  `harness.phase.changed`, `harness.item.added`, `overlay.set`. Everything
  else (all `card.*` / `wave.*` / `cove.*` / `terminal.*` structural kinds,
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

Deletes run in small batches (default 5000 rows) inside `BEGIN IMMEDIATE`
transactions with a ~100ms yield in between, so writer-lock hold stays in
the milliseconds even on a first pass over a bloated DB.

## What you lose after the horizon

Pruning `claude.hook` / `codex.hook` rows older than the horizon is an
**accepted regression** for two production consumers that replay them from
genesis:

1. **Wave-fs hook transcript** — `hook_events_for_card`
   (`crates/calm-truth/src/wave_fs_view.rs`): a card's hook transcript
   projection no longer includes hook events (either kind) older than the
   horizon.
2. **Harness recovery catch-up** — `replay_harness_events_since`
   (`crates/calm-server/src/harness/mod.rs`): boot recovery replays
   both hook kinds (among others) above the harness push watermark; a
   watermark older than the horizon cannot recover hooks that were pruned.

Both consume diagnostics-grade data; >30-day-old hook history is not needed
for correctness of live waves. `harness.phase.changed` / `harness.item.added`
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

## Reclaiming disk space

The pruner never VACUUMs. Freed pages are reused by new appends, so the file
size plateaus rather than shrinking. To actually shrink the file:

1. Back up the DB file.
2. Run `neige vacuum --force`.

**Warning:** VACUUM takes a full-database lock for its duration — run it
off-peak, never during active use.

## Pointers

* **#33 per-actor retention (future)** — the policy's `Vec<RetentionRule>`
  is the seam; additional rules keyed on an actor prefix slot in without
  touching the pass loop.
* **Postgres note** — storage is SQLite-only today
  (`crates/calm-truth/src/db/sqlite.rs`). The DELETE predicate ports as-is
  to a future PG backend, where autovacuum makes the space-reclaim section
  moot.
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
