# research-archive

A point-in-time archive of Longbridge fundamentals (issue #2575). Longbridge
returns only the current consensus, ratings and valuation, so the archive
stores every daily snapshot itself. The archive is one SQLite file,
`archive.sqlite3`. It is append-only, and its format is frozen at release.

This package is the persistence boundary (S1-1). It holds the compiled
endpoint catalog (`src/catalog.rs`) and the `Store` (`src/store/`), which is
the only owner of the schema, the guard triggers and the catalog check. The
capture job that fills the archive arrives in S1-2.

## Schema

The DDL is the const `store::schema::DDL`, and `PRAGMA user_version = 1`.
Every table is `STRICT`, so readers need SQLite 3.37 or later. Required
columns are `NOT NULL`.

| table | columns | notes |
|---|---|---|
| `endpoint` | `endpoint_id` PK, `method`, `path`, `query_json`, `headers_json`, `dc_restrict` NULL, `key_kind` | The frozen catalog. `key_kind` ∈ `index`, `security`. |
| `blob` | `sha256` PK, `body` | Holds the exact envelope `data` bytes. `sha256` is the lowercase hex digest of `body`. |
| `run` | `run_id` PK, `source`, `as_of`, `window_start`, `window_end`, `started_at`; UNIQUE (`source`, `as_of`) | Holds one run per source and trading day. |
| `run_key` | (`run_id`, `key`, `origin`) PK | `origin` is `config`, `index-symbol` or `index:<sym>`. |
| `run_membership` | (`run_id`, `index_sym`) PK, `mode`, `source_run_id` NULL, `frozen_at` | `mode` ∈ `observed`, `carried`, `none`. `source_run_id` is set if and only if `mode = carried`. |
| `observation` | (`run_id`, `key`, `endpoint_id`, `attempt`) PK, `fetched_at`, `status`, `sha256` NULL, `error_class` NULL, `error_code` NULL | Every attempt is a row. `attempt` starts at 1. |
| `run_end` | `run_id` PK, `finished_at`, `outcome`, `reason` NULL | `outcome` ∈ `complete`, `partial`, `missed`. |

The partial index `observation_ok_by_key (key, endpoint_id, fetched_at) WHERE
status = 'ok'` serves the point-in-time read.

**Representations.**

- **Timestamps.** `fetched_at`, `started_at`, `frozen_at`, `finished_at`,
  `window_start` and `window_end` are integer Unix milliseconds in UTC, taken
  from the wall clock.
- **`as_of`** is the run's trading day as an ISO date, `YYYY-MM-DD`.
- **Run ids** are integers that the Store assigns in order (max + 1).
- **`source`** is `longbridge`.

**Catalog encoding.**

- `method` is `GET`, and `path` is the request path.
- `query_json` is a compact JSON array of `[name, value]` pairs, in the order
  they are sent. The value `null` stands for the run key. Example:
  `[["symbol",null],["indicator","pe"],["range","1"]]`.
- `headers_json` uses the same format, for example
  `[["Accept-Language","en"]]`.
- `dc_restrict` is NULL for every current endpoint.
- **A changed field needs a new id.** A change to any field of an endpoint
  ships under a new `endpoint_id`.

The compiled catalog has six security endpoints and one index endpoint:

- **Security endpoints:** `consensus`, `forecast_eps`, `rating_latest`,
  `ratings`, `rating_detail` and `valuation`. They take the query `symbol`,
  except `valuation`, which takes `symbol, indicator=pe, range=1`.
- **Index endpoint:** `index_constituents`, with the query `symbol, offset=0,
  limit=1000`.

## Status classes

- **`ok`** is the only state-bearing status. The row's `sha256` names the
  body, and `error_class`/`error_code` are NULL. Lost coverage, such as a
  delisted symbol, arrives as an `ok` with empty fields and is stored as is.
- **`transient`, `throttled`, `auth_unavailable`, `region_restricted` and
  `permanent`** mean "no observation". These rows may still keep a body for
  audit. For example, an `index_constituents` body that fails the membership
  check is stored as `permanent` with `error_code = incomplete_membership`.
- **There is no `not_found` class.**

## Read contract

`Store::read_at(key, endpoint_id, t)` returns `(body, fetched_at, as_of)`.

- **Which row.** It returns the latest `ok` row with `fetched_at ≤ t`. Ties on
  `fetched_at` go to the higher `run_id`, then to the higher `attempt`.
- **Failure rows never win.** A failure row hides nothing: a `permanent`
  today leaves yesterday's value as the value.
- **An empty body is a value.** It means "no coverage at that time", and it is
  never skipped in favour of an earlier body. This holds for both zero bytes
  and an empty-shaped JSON body.
- **Later rows are invisible.** A row with `fetched_at > t` does not exist at
  `t`.

## Universe and coverage

Membership comes from `run_key`. `Store::universe_at(t)` works as follows:

1. **Pick the run.** It takes the run with the latest `started_at ≤ t` (ties
   go to the higher `run_id`). Runs that ended `missed` are skipped, because
   they have no keys.
2. **Add the planned keys.** It returns that run's `config` and
   `index-symbol` keys. They are written with the run row, so they are
   visible from `started_at` on.
3. **Add the frozen members.** It adds the run's `index:<sym>` keys only when
   that index's `run_membership.frozen_at ≤ t`.
4. **Report pending indexes.** Planned indexes that are not yet frozen at `t`
   are listed as `pending`, with no members. They do not fall back to an
   earlier run.

The membership facts work like this:

- **Each origin is a separate fact.** A key that is configured and is also a
  member of two indexes has three `run_key` rows.
- **Each key counts once.** `Universe::keys()` returns each distinct key
  once.

**Coverage of a run.** Coverage is each distinct key in its `run_key` rows ×
the endpoints of the key's kind, where each pair counts once:

- An `index-symbol` key takes the index endpoints.
- `config` and `index:<sym>` keys take the security endpoints.

`Store::coverage(run_id)` lists every planned pair, with its attempt count and
whether any attempt is `ok`. Index members join a run's coverage when its
freeze commits.

## Writes

Each write is one `BEGIN IMMEDIATE` transaction:

- **`plan_run`** writes the `run` row and the `config` and `index-symbol`
  keys. It is idempotent per `(source, as_of)`: when a plan exists, the
  stored plan comes back unchanged, and later config changes do not touch it.
- **`record_observation`** writes the blob and the observation row. It
  numbers the attempt per `(run, key, endpoint)` inside the transaction. It
  refuses a pair that is not planned and a run that has ended.
- **`freeze_membership`** writes every `run_membership` row and the
  `index:<sym>` keys, all or nothing.
  - It must name exactly the planned indexes.
  - `observed` needs an `ok` constituents row in this run. `carried` needs
    one in the earlier source run, never in the run itself.
  - **A second freeze is refused** with `AlreadyFrozen` and writes nothing. A
    run is frozen exactly when it has a `run_membership` row.
- **`end_run`** writes `complete` or `partial`.
- **`close_dangling_runs(now)`** writes `partial` (reason `dangling`) for
  every run with `window_end ≤ now` and no `run_end`.
- **`record_missed`** writes a keyless run row and its `run_end(missed)`,
  with `started_at = finished_at = now`. It does this only for a day whose
  window has closed and that has no run.

## Append-only guarantee

Every table has three guard triggers:

- **`BEFORE UPDATE`** raises `ABORT`.
- **`BEFORE DELETE`** raises `ABORT`.
- **`BEFORE INSERT`** fires when the primary-key or UNIQUE row exists. It
  raises `IGNORE` for `blob`, which deduplicates bodies, and `ABORT` for
  every other table.

The guards work without any pragma:

- **Insert guards cover replace and upsert.** A `BEFORE INSERT` trigger runs
  before conflict resolution, so it also stops `INSERT OR REPLACE` and
  `INSERT … ON CONFLICT DO UPDATE`.
- **`recursive_triggers` does not matter.** The guards hold with it OFF
  (SQLite's default, under which a REPLACE deletes without firing delete
  triggers) and with it ON.
- **Any connection is covered.** The tests run the guards on raw connections
  with foreign keys off, using the bundled SQLite 3.46. The guards also hold
  on the system `sqlite3` 3.40.1 CLI.

`Store::open` refuses an existing file on any of these mismatches:

- **`user_version`** differs from this build's.
- **Schema drift.** Any table, index or trigger is missing, extra, or differs
  in its stored SQL text from what `DDL` produces.
- **Catalog drift.** A stored `endpoint` row is missing, extra or changed
  compared with the compiled catalog.

A new, empty file gets the schema and the catalog in one transaction.

## Importing read-only

The capture job is the only writer. The file is in WAL mode, so recent
commits may still be in `archive.sqlite3-wal`. To read a consistent archive,
use one of these:

- **Checkpoint first.** The writer (or anyone, while no capture runs) runs
  `PRAGMA wal_checkpoint(TRUNCATE)`, and then the reader opens the main file.
- **Copy both files.** Copy `archive.sqlite3` together with its
  `archive.sqlite3-wal` at a moment when no capture
  is running.

Then open the file read-only, for example
`sqlite3 'file:archive.sqlite3?mode=ro'` or `SQLITE_OPEN_READONLY` without
create, and check `PRAGMA user_version` = 1 before reading.

## Tests

Run `cargo nextest run -p research-archive`.

| test module | covers |
|---|---|
| `guards` | I2, on every table |
| `open` | the refusals |
| `writes` | plan and freeze atomicity, the attempt counter and the CHECKs |
| `reads` | I5, I6, the universe and coverage |
