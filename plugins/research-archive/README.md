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
columns are `NOT NULL`. `run` and `run_end` key on `INTEGER PRIMARY KEY`
(the rowid itself); every other table is `WITHOUT ROWID` (see
[Append-only guarantee](#append-only-guarantee)).

| table | columns | notes |
|---|---|---|
| `endpoint` | `endpoint_id` PK, `method`, `path`, `query_json`, `headers_json`, `dc_restrict` NULL, `key_kind` | The frozen catalog. `key_kind` ∈ `index`, `security`. |
| `blob` | `sha256` PK, `body` | Holds the exact envelope `data` bytes. `sha256` is the lowercase hex digest of `body`. |
| `run` | `run_id` PK, `source`, `as_of`, `window_start`, `window_end`, `started_at`; UNIQUE (`source`, `as_of`) | Holds one run per source and trading day. |
| `run_key` | (`run_id`, `key`, `origin`) PK | `origin` is `config`, `index-symbol` or `index:<sym>`. |
| `run_membership` | (`run_id`, `index_sym`) PK, `mode`, `source_run_id` NULL, `frozen_at` | `mode` ∈ `observed`, `carried`, `none`. `source_run_id` is set if and only if `mode = carried`. |
| `observation` | (`run_id`, `key`, `endpoint_id`, `attempt`) PK, `fetched_at`, `status`, `sha256` NULL, `error_class` NULL, `error_code` NULL | Every attempt is a row. `attempt` starts at 1. `error_class` and `error_code` are both NULL on `ok` rows and both set on failure rows. |
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
  audit.
- **There is no `not_found` class.**

**Failure detail.** Every failure row has an `error_class` from a closed set
(a CHECK) and a non-empty `error_code` whose meaning depends on it:

| `error_class` | when | `error_code` |
|---|---|---|
| `envelope` | an OpenAPI envelope with a non-zero code | the envelope code, in decimal (`429002`) |
| `http_status` | a non-success HTTP status outside an envelope | the status, in decimal (`503`) |
| `transport` | no HTTP response: a timeout or a connection error | the SDK error variant, snake_case (`request_timeout`) |
| `unexpected_response` | a response that is not a usable envelope | the SDK error variant |
| `auth` | the SDK had no usable credentials | the SDK error variant (`invalid_access_token`) |
| `dc_region_restricted` | the SDK refused locally for the token's data center | the SDK error variant; status is always `region_restricted` |
| `client` | the SDK could not build the request | the SDK error variant |
| `incomplete_membership` | an `index_constituents` body that fails the completeness rule | the rule that failed (`zero_total`, `count_mismatch`, …); status is always `permanent` |

## Index membership

The catalog asks `index_constituents` for `offset=0, limit=1000`, so one call
returns every member of an index with at most 1,000 members. A body is a
**complete membership** only when `total > 0` and the number of distinct
`stocks[].symbol` equals both `total` and the length of `stocks`
(`membership::constituents_members`). A mistyped index symbol (`total = 0`),
an unpaged answer (20 of 501), a truncated page or an index above the
`limit` fails it. Such a body is filed as `permanent` /
`incomplete_membership`; `record_observation` refuses to store it as `ok`.
So an incomplete response can never freeze an observed universe.

**The freeze is derived, not supplied.** `freeze_membership(run_id,
frozen_at)` takes no membership from its caller; the runner only decides
when to freeze. For each planned `index-symbol` key, in one transaction:

1. **observed** when this run has an `ok` constituents row for the index (the
   latest attempt);
2. else **carried** from the run of the same source with the latest earlier
   `as_of` that has one (its latest attempt). That row is an original
   observation by construction, never a carry;
3. else **none**: no run has an `ok` membership for it.

The members are parsed from that body. The freeze is refused when a stored
`ok` body fails the completeness rule (only a writer bypassing the Store can
cause it), when `frozen_at` is outside the run's window or before the run's
clock, or when a carried body was fetched after `frozen_at` (no
look-ahead). Any catalog the Store accepts has exactly one index endpoint,
and that is the membership endpoint.

## Read contract

`Store::read_at(key, endpoint_id, t)` returns `(body, fetched_at, as_of)`.

- **Which row.** It returns the latest `ok` row with `fetched_at ≤ t`. Ties on
  `fetched_at` go to the higher `run_id`, then to the higher `attempt`. This
  "latest ok" order is defined once (`LATEST_OK`) and the freeze uses it
  too, so a freeze and a read at the same time pick the same body.
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

Each write is one `BEGIN IMMEDIATE` transaction.

**A run's clock is monotonic.** Its high-water mark is the latest of
`started_at`, every `frozen_at` and every `fetched_at` of the run. Every
`fetched_at`, `frozen_at` and `finished_at` written to the run must be at or
after the mark (equal is allowed); an earlier one is refused with
`ClockWentBack`. So within a run attempt order is time order, no row
predates the plan, and an index member's observations come after the freeze
that planned it. A runner that records results of concurrent requests takes
`fetched_at` when it records, or a late write is refused.

- **`plan_run`** writes the `run` row and the `config` and `index-symbol`
  keys. It is idempotent per `(source, as_of)`: when a plan exists, the
  stored plan comes back unchanged, and later config changes do not touch it.
  A new plan's `started_at` must be inside its window.
- **`record_observation`** writes the blob and the observation row. It
  numbers the attempt per `(run, key, endpoint)` inside the transaction. It
  refuses a pair that is not planned, a run that has ended, a `fetched_at`
  outside the run's window `[window_start, window_end)` or before its clock,
  an incomplete `ok` constituents body, and an `error_class` the endpoint
  cannot produce (`incomplete_membership` only from the membership endpoint,
  `dc_region_restricted` only from an endpoint with a `dc_restrict`, which
  today is none).
- **`freeze_membership`** derives and writes every `run_membership` row and
  the `index:<sym>` keys, all or nothing (see
  [Index membership](#index-membership)).
  - **A second freeze is refused** with `AlreadyFrozen` and writes nothing. A
    run is frozen exactly when it has a `run_membership` row.
  - A run that plans no index has nothing to freeze; the call writes nothing.
- **`end_run`** writes `complete` or `partial`. `complete` is refused unless
  every planned index is frozen and every planned pair has at least one row;
  whether the remaining failures are final is the runner's call.
- **`close_dangling_runs(now)`** writes `partial` (reason `dangling`) for
  every run with `window_end ≤ now` and no `run_end`.
- **`record_missed`** writes a keyless run row and its `run_end(missed)`,
  with `started_at = finished_at = now`. It does this only for a day whose
  window has closed and that has no run.

## Append-only guarantee

Every table has three guard triggers:

- **`BEFORE UPDATE`** raises `ABORT`.
- **`BEFORE DELETE`** raises `ABORT`.
- **`BEFORE INSERT`** fires when a row with the same PRIMARY KEY or UNIQUE
  value exists. It raises `IGNORE` for `blob`, which deduplicates bodies,
  and `ABORT` for every other table.

**The rule: no table has a uniqueness surface its guard does not check.**
Each insert guard checks every PRIMARY KEY and UNIQUE constraint, and no
table has an implicit rowid: `run` and `run_end` key on the rowid itself
(`INTEGER PRIMARY KEY`), and every other table is `WITHOUT ROWID`. An
implicit rowid would be a second key the guard does not see:
`INSERT OR REPLACE … (rowid, …)` with a stored row's rowid and a fresh
primary key would delete that row.

What follows from the rule:

- **Replace and upsert are covered.** A `BEFORE INSERT` trigger runs before
  conflict resolution, so any insert that would collide with a stored row
  meets its guard first: `INSERT OR REPLACE`, `REPLACE` and
  `INSERT … ON CONFLICT DO UPDATE` cannot delete or rewrite a row.
- **`recursive_triggers` does not matter.** The guards hold with it OFF
  (SQLite's default, under which a REPLACE deletes without firing delete
  triggers) and ON, and with foreign keys off. The tests run every statement
  above, plus an explicit rowid collision, against every table on raw
  connections in both settings, using the bundled SQLite 3.46.

**What this guarantees, and what it does not.** The guards and the CHECKs
hold for ordinary SQL on a connection with SQLite's default trigger and
check settings. They guard against mistakes and ordinary writers. They do
not stop a writer that turns them off:

- `SQLITE_DBCONFIG_ENABLE_TRIGGER` off (the `sqlite3` CLI's
  `.dbconfig enable_trigger off`) disables the guards, so `UPDATE` and
  `DELETE` go through.
- `PRAGMA ignore_check_constraints = ON` lets closed-set violations in.
- `PRAGMA writable_schema`, schema statements (`DROP TRIGGER`, `DROP TABLE`,
  `ALTER TABLE`) and editing the file's bytes are not covered either.

`Store::open` refuses a file whose schema has drifted, but it cannot see rows
written while a guard or check was off.

## Opening

`Store::open(path)` checks the file against the compiled catalog.

- **A missing or empty file** gets the schema and the catalog in one
  transaction, so the first capture creates the archive.
- **An existing file** is refused on any of these mismatches:
  - **`user_version`** differs from this build's.
  - **Schema drift.** Any table, index or trigger is missing, extra, or
    differs in its stored SQL text from what `DDL` produces.
  - **Catalog drift.** A stored `endpoint` row is missing, extra or changed
    compared with the compiled catalog.
- **A refused file is left as it was.** The WAL journal mode is set only
  after the check (or the creation) succeeds.

## Importing read-only

The capture job is the only writer. The file is in WAL mode, so recent
commits may still be in `archive.sqlite3-wal`. To read a consistent archive,
use one of these:

- **Checkpoint first.** The writer (or anyone, while no capture runs) runs
  `PRAGMA wal_checkpoint(TRUNCATE)`, and then the reader opens the main file.
- **Copy both files.** Copy `archive.sqlite3` together with its
  `archive.sqlite3-wal` at a moment when no capture
  is running.

Never run `ANALYZE` or `PRAGMA optimize` on the live file: they create
`sqlite_stat*` tables, which `Store::open` refuses as schema drift. Run them,
if at all, on a copy.

Then open the file read-only, for example
`sqlite3 'file:archive.sqlite3?mode=ro'` or `SQLITE_OPEN_READONLY` without
create, and check `PRAGMA user_version` = 1 before reading.

## Tests

Run `cargo nextest run -p research-archive`.

| test module | covers |
|---|---|
| `guards` | I2, on every table, including the rowid collision |
| `open` | the refusals, and that a refused file is untouched |
| `writes` | plan atomicity, the window bound, `complete`, the attempt counter (also with two writers) and the CHECKs |
| `freeze` | the derived freeze, I5, its atomicity and refusals |
| `reads` | I6, the universe, ties and coverage |
