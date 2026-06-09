# MCP `update_wave_state` / SQLite `database is locked`

## Conclusion

The BEGIN-DEFERRED upgrade hypothesis is confirmed.

`SqlxRepo::open` configures every SQLite connection with
`PRAGMA busy_timeout = 5000` and `PRAGMA journal_mode = WAL`, but the audited
write wrappers start transactions with `self.pool.begin()`. In sqlx 0.8.6,
`Pool::begin()` calls `Transaction::begin(..., None)`, and SQLite's default
statement is `BEGIN`, i.e. a deferred transaction.

`update_wave_state` first reads the wave outside the write transaction via
`resolve_wave_for_identity`, then starts `write_with_events_typed`; inside that
transaction it runs `apply_wave_patch_tx`, which delegates to
`wave_update_tx`.

Inside `wave_update_tx` the SQL order is:

1. `SELECT ... FROM waves WHERE id = ?1`
2. patch the in-memory `Wave`
3. `UPDATE waves SET ... WHERE id = ?8`
4. `event_append_in_tx` then `INSERT INTO events (...) RETURNING id`
5. `COMMIT`, then bus emit

So two concurrent deferred txs can both read `waves`; once another writer
commits, the loser upgrades a stale read snapshot to writer and SQLite returns
BUSY/SNAPSHOT immediately. That path is not the ordinary waitable writer-lock
case that `busy_timeout` helps with.

## Concurrent Hot Spots

All audited event writes contend on `events`; wave updates additionally contend
on `waves`.

`waves` + `events`:

- MCP spec daemon: `mcp_server/tools/wave_state.rs:update_wave_state` ->
  `write_with_events_typed` -> `wave_update_tx` -> `WaveUpdated`.
- REST wave patch: `routes/waves.rs:update_wave` follows the same
  `write_with_events_typed` -> `wave_update_tx` sequence.
- REST wave create/spec-card boot path writes `waves`, `cards`, and `events`
  in one `write_with_events_typed` transaction.
- Replay forced transition uses `write_with_events_typed` + `wave_update_tx`.

`events` only, but still a concurrent SQLite writer:

- Dispatcher failure path logs `TaskFailed` with `log_pure_event`.
- Operation runtimes (`claude_adapter`, `terminal_adapter`, `codex_adapter`)
  mark runtime status / bind thread attribution with `write_with_events_typed`
  and append runtime events.
- Codex/Claude hook ingest logs hook events via `routes/codex.rs` /
  `Repo::log_pure_event`.
- Spec harness / daemon heartbeat-style progress logs harness item/phase
  events via `harness/run_loop.rs` and `operation/spec_harness_start_adapter`.
- Plugin host state changes append `plugin.state` via `log_pure_event`.
- Pending codex thread reconciliation uses `write_with_events_typed` and
  appends runtime/card events.

Any of these can commit between the MCP transaction's `SELECT waves` and later
`UPDATE waves` / `INSERT events`, causing a snapshot upgrade failure.

## sqlx 0.8.6 API Surface

sqlx 0.8.6 does have an official custom-begin API:

- `Pool::begin_with(statement)` exists in `sqlx-core-0.8.6/src/pool/mod.rs`.
- `Connection::begin_with(statement)` exists in `sqlx-core` and is implemented
  by `SqliteConnection`.
- `SqliteTransactionManager` accepts the custom statement and verifies the
  connection entered a transaction.

So the direct sqlx option is `self.pool.begin_with("BEGIN IMMEDIATE").await?`.
Raw `EXECUTE "BEGIN IMMEDIATE"` is possible only if manually managing a
connection + commit/rollback; it gives up the ergonomic `Transaction` wrapper
unless carefully rebuilt.

## Fix Options

1. Change audited event writes (`write_with_event`, `write_with_events`,
   `log_pure_event`, probably `write_in_tx`) from `pool.begin()` to
   `pool.begin_with("BEGIN IMMEDIATE")`.
   Cost: serializes writers at tx start; contenders wait under
   `busy_timeout` before any stale snapshot exists. Wider effect, but matches
   "all audited writes append events" semantics.

2. Use `BEGIN IMMEDIATE` only for wave update paths.
   Cost: smaller blast radius, but requires a parallel repo API or
   downcast/SQLite-specific path because current trait wrappers own `begin()`.
   Other read-before-write eventized txs can still hit the same class.

3. Retry on SQLite BUSY/SNAPSHOT around audited writes.
   Cost: must re-run the whole closure, ensure captured typed rows/events are
   reset per attempt, and avoid duplicating externally visible side effects.
   Safer only after the closure contract is made retry-clean.

4. Collapse `wave_update_tx` into one SQL `UPDATE ... RETURNING`.
   Cost: avoids the `waves` read-upgrade in that helper, but event append still
   needs a writer and other helpers retain deferred read-before-write patterns.

Recommended first fix: option 1, with a focused regression test around
concurrent `update_wave_state` + event append.

## Fix applied (PR #?)

- Changed the four `RepoEventWrite` transaction entry points
  (`write_with_event`, `write_with_events`, `log_pure_event`, `write_in_tx`)
  from `Pool::begin()` to sqlx 0.8.6 `Pool::begin_with("BEGIN IMMEDIATE")`.
- Kept `RepoSyncDomainRaw` / `RepoRead` deferred `pool.begin()` calls unchanged
  for cursor and replay read paths.
- Converted `mcp_wave_state_db_lock_repro` from an ignored repro into a
  regression gate that asserts zero SQLite busy/snapshot errors under
  concurrent MCP wave updates and event appends.
