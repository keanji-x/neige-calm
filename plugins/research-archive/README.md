# research-archive

A point-in-time archive of Longbridge fundamentals (issue #2575). Longbridge
returns only the current consensus, ratings and valuation, so the archive
stores every daily snapshot itself. The archive is one SQLite file,
`archive.sqlite3`. It is append-only, and its format is frozen at release.

The package has two layers:

- **Persistence (S1-1).** The compiled endpoint catalog (`src/catalog.rs`)
  and the `Store` (`src/store/`), which is the only owner of the schema, the
  guard triggers and the catalog check.
- **Capture job, first half (S1-2a).** The `research-archive` binary with its
  typed config, environment guard, data-dir lock, token storage, `auth`
  commands and the Longbridge source; see [The binary](#the-binary).
- **Capture job, second half (S1-2b).** The `capture` command with its
  trading calendar and run phases, the `notify` alert, and the systemd unit
  templates; see [The capture job](#the-capture-job).
- **Status plugin (S1b).** The read-only `research-archive-plugin` binary,
  a Neige plugin that reports the archive's health and reads archived values
  as of a past moment; see [Status plugin (S1b)](#status-plugin-s1b).

## Schema

The current schema is the frozen v1 `store::schema::DDL` followed by
`MIGRATION_V2`, with `PRAGMA user_version = 2`. Released migration text is
never rewritten.
Every table is `STRICT`, so readers need SQLite 3.37 or later. Required
columns are `NOT NULL`. `run` and `run_end` key on `INTEGER PRIMARY KEY`
(the rowid itself); every other table is `WITHOUT ROWID` (see
[Append-only guarantee](#append-only-guarantee)).

| table | columns | notes |
|---|---|---|
| `endpoint` | `endpoint_id` PK, `method`, `path`, `query_json`, `headers_json`, `dc_restrict` NULL, `key_kind` | The frozen catalog. `key_kind` ∈ `index`, `security`. |
| `blob` | `sha256` PK, `body` | Holds the exact envelope `data` bytes. `sha256` is the lowercase hex digest of `body`. |
| `run` | `run_id` PK, `source`, `as_of`, `window_start`, `window_end`, `started_at`; UNIQUE (`source`, `as_of`) | Holds one run per source and trading day. |
| `run_endpoint` | (`run_id`, `endpoint_id`) PK | Immutable endpoint set saved with each run plan; old sets are backfilled from the legacy catalog during explicit v1 upgrade. |
| `run_key` | (`run_id`, `key`, `origin`) PK | `origin` is `config`, `index-symbol` or `index:<sym>`. |
| `run_membership` | (`run_id`, `index_sym`) PK, `mode`, `source_run_id` NULL, `frozen_at` | `mode` ∈ `observed`, `carried`, `none`. `source_run_id` is set if and only if `mode = carried`. |
| `observation` | (`run_id`, `key`, `endpoint_id`, `attempt`) PK, `fetched_at`, `status`, `sha256` NULL, `error_class` NULL, `error_code` NULL | Every attempt is a row. `attempt` starts at 1. `error_class` and `error_code` are both NULL on `ok` rows and both set on failure rows. |
| `run_core_complete` | `run_id` PK, `finished_at` | Append-only v3 checkpoint: Core settled, supplemental work may remain open. No legacy backfill. |
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

The original v2 catalog has seven security endpoints and one index endpoint;
v3 additions are documented under [Market capture](#market-capture-and-schema-version-3):

- **Security endpoints:** `consensus`, `forecast_eps`, `rating_latest`,
  `ratings`, `rating_detail`, `valuation` and `consensus_annual`. They take
  the query `symbol`, except `valuation` (`symbol, indicator=pe, range=1`)
  and `consensus_annual` (`symbol, period_type=af`).
- **Index endpoint:** `index_constituents`, with the query `symbol, offset=0,
  limit=1000`.

## Status classes

- **`ok`** is the only state-bearing status. The row's `sha256` names the
  body, and `error_class`/`error_code` are NULL. Lost coverage, such as a
  delisted symbol, arrives as an `ok` with empty fields and is stored as is.
- **`transient`, `throttled`, `auth_unavailable`, `region_restricted` and
  `permanent`** mean "no observation". They keep no bytes, with one
  exception: a membership body that fails the completeness rule is kept
  (`permanent` / `incomplete_membership`). A CHECK pins it: `sha256` is set
  exactly on `ok` rows and `incomplete_membership` rows.

**The caller never classifies a body.** `record_observation` takes either
`Response::Body` (the envelope's `data` bytes) or `Response::Failed` (a
failure without a usable body, with its class and detail). The Store
decides the status of a body: on a security endpoint it is `ok`; on the
membership endpoint it is `ok` when it is a complete membership and
`permanent` / `incomplete_membership` otherwise. Bodies of other failures
(for example an HTTP error page) are not data and are not stored.
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
| `incomplete_membership` | derived by the Store, never stated by a caller: an `index_constituents` body that fails the completeness rule | the rule that failed (`zero_total`, `count_mismatch`, …); status is always `permanent`; the body is kept |

## Index membership

The catalog asks `index_constituents` for `offset=0, limit=1000`, so one call
returns every member of an index with at most 1,000 members. A body is a
**complete membership** only when `total > 0` and the number of distinct
`stocks[].symbol` equals both `total` and the length of `stocks`
(`membership::constituents_members`). A mistyped index symbol (`total = 0`)
or a truncated response (an unpaged answer of 20 of 501, a short page, a
duplicate) fails it. The rule has no size limit of its own: the 1,000 cap
comes from the request's `limit=1000`, and an index with more members fails
only because its response then carries fewer symbols than `total`. The Store files such a body as `permanent` /
`incomplete_membership`, so it is never `ok`, and an incomplete response can
never freeze an observed universe.

**The freeze is derived, not supplied.** `freeze_membership(run_id,
frozen_at)` takes no membership from its caller; the runner only decides
when to freeze. For each planned `index-symbol` key, in one transaction:

1. **observed** when this run has an `ok` constituents row for the index (the
   latest by `LATEST_OK`);
2. else **carried** from the latest `ok` row, by `LATEST_OK`, among runs of
   the same source with an earlier `as_of`. Since windows follow `as_of`
   order, that is the latest earlier day's latest `ok`, the row `read_at`
   returns at `frozen_at` as of the moment the freeze executes. It is an
   original observation by
   construction, never a carry. A later day's row is never taken, even if
   it was written first;
3. else **none**: no run has an `ok` membership for it.

The members are parsed from that body. The freeze is refused when a stored
`ok` body fails the completeness rule (only a writer bypassing the Store can
cause it), or when `frozen_at` is outside the run's window or before the
run's clock. Look-ahead cannot happen: this run's rows are at or before its
clock, and an earlier day's rows are before its window end, which is at or
before this window's start. Any catalog the Store accepts has exactly one index endpoint,
and that is the membership endpoint.

## Read contract

`Store::read_at(key, endpoint_id, t)` returns `(body, fetched_at, as_of)`.

- **Which row.** It returns the latest `ok` row with `fetched_at ≤ t`, by
  `LATEST_OK`: `fetched_at`, then the higher `run_id`, then the higher
  `attempt`. Equal `fetched_at` happens only within a run (the clock allows
  equal times), where the higher attempt wins; across runs of a source the
  windows do not overlap, so `fetched_at` order is `as_of` order and the
  `run_id` step never decides. The freeze uses the same order, so the
  freeze takes the body `read_at(frozen_at)` returns **in the snapshot at the
  moment the freeze executes**, within a run and across runs. The clock
  allows equal times, so a row recorded at the same millisecond after the
  freeze is not part of it: record `{A}` at 10, freeze at 10, record `{B}` at
  10, and the membership is `{A}` while `read_at(10)` now returns `{B}`.
- **Failure rows never win.** A failure row hides nothing: a `permanent`
  today leaves yesterday's value as the value.
- **An empty body is a value.** It means "no coverage at that time", and it is
  never skipped in favour of an earlier body. This holds for both zero bytes
  and an empty-shaped JSON body.
- **Later rows are invisible.** A row with `fetched_at > t` does not exist at
  `t`. Quarterly `consensus` and `consensus_annual` are independent series;
  neither falls back to the other. Annual values begin at their actual fetch
  time, never at a fiscal period date embedded in the response.
- **Planning is historical.** `ever_planned` and coverage use each run's saved
  endpoint set. Upgrading a v1 run does not plan annual data retroactively.
  Once a later run plans annual data, an earlier `at` without an annual fetch
  answers `no_value`; before any annual plan it answers `not_archived`.

## Universe and coverage

Membership comes from `run_key`. `Store::universe_at(t)` works as follows:

1. **Pick the run.** It takes the run with the latest `started_at ≤ t`.
   Planned runs of a source cannot share a `started_at` (each lies inside
   its own window, and windows do not overlap); the higher `run_id` would
   decide. Runs that ended `missed` are skipped, because
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

**Coverage of a legacy run.** V3 runs use persisted requests (see the v3 section).
For legacy runs, coverage joins each distinct key in `run_key` to the
same run's saved `run_endpoint` set and then filters by endpoint kind. Each
pair counts once. It never crosses historical keys with today's whole catalog:

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
- **Windows of one source never overlap and follow `as_of` order.**
  `plan_run` and `record_missed` refuse (`WindowConflict`) a window that
  overlaps another run of the source, or whose order contradicts `as_of`
  order: an earlier day's window must end at or before this one starts, a
  later day's must start at or after it ends. Windows may touch.
  Production windows (D 17:30 New York to the next open) never overlap.
- **`record_observation`** writes the blob and the observation row. It
  numbers the attempt per `(run, key, endpoint)` inside the transaction. It
  refuses a pair that is not planned, a run that has ended, a `fetched_at`
  outside the run's window `[window_start, window_end)` or before its clock,
  and a `dc_region_restricted` failure from an endpoint without a
  `dc_restrict` (today, every endpoint). It returns the attempt number and
  the status it derived.
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
    differs in its stored SQL text from what `DDL`, `MIGRATION_V2`, and `MIGRATION_V3` produce.
  - **Catalog drift.** A stored `endpoint` row is missing, extra or changed
    compared with the compiled catalog.
- **A refused file is left as it was.** The WAL journal mode is set only
  after the check (or the creation) succeeds.

Ordinary capture and plugin opens require v3 and never migrate. Follow the
explicit [v3 upgrade path](#market-capture-and-schema-version-3); v1 copies first
use the retained annual command. Refused versions remain untouched.

## Importing read-only

The capture job is the only writer, and the file is in WAL mode: recent
commits may still be in `archive.sqlite3-wal`, so the main file alone can
miss them (a copy of the main file taken while a capture held commits in the
WAL showed none of its rows). Opening the live file with `?mode=ro` is not
a read-only import either: a WAL reader needs the `-shm` file. When the
`-wal`/`-shm` files do not exist, the open fails in a read-only directory
("attempt to write a readonly database") and in a writable one creates them
next to the archive. (SQLite 3.22+ can read a WAL file in a read-only
directory when readable `-wal` and `-shm` files already exist; this recipe
does not rely on that.)

Take a snapshot copy, then open the copy:

1. **Snapshot**, either way:
   - `sqlite3 archive.sqlite3 ".backup copy.sqlite3"`, which is a consistent
     copy even while a capture runs; or
   - while no capture runs, `PRAGMA wal_checkpoint(TRUNCATE)` on the live
     file, then copy `archive.sqlite3` alone.
2. **Open the copy** with `sqlite3 'file:copy.sqlite3?immutable=1'` (or
   `SQLITE_OPEN_READONLY` with the `immutable=1` URI parameter). That works in
   a read-only directory and creates no files. `?mode=ro` on a copy without
   `-wal`/`-shm` files works only in a writable directory, where it creates
   them.
3. Check `PRAGMA user_version` = 2 before reading current archives; v1 copies
   require a v1-aware reader or an explicit upgrade of a writable copy.

Verified with the system `sqlite3` 3.40.1 against a Store-created file:
`?mode=ro` on a copy without sidecars in a mode-555 directory fails; `.backup` (also with a writer
holding the file open) and checkpoint-then-copy, each opened with
`immutable=1` in a mode-555 directory, read every row and create no file.

Never run `ANALYZE` or `PRAGMA optimize` on the live file: they create
`sqlite_stat*` tables, which `Store::open` refuses as schema drift. Run them,
if at all, on a copy.

## The binary

`research-archive <command> --config <file>`. `capture`, the `auth` commands
and `upgrade-annual-catalog` first refuse a hostile environment, then read
the config, then set the region, all before any thread starts. The upgrade
builds no SDK client and needs no token or network, but uses the same guard.
`notify` skips the guard and only reads the config.

**Config.** Existing fields remain required; unknown fields are refused.
Optional `benchmarks` defaults to `[]`, so existing deployed configs load. `research-archive.example.toml` documents each field:
`data_dir` (absolute), `oauth_client_id`, `access_region` (`global` or
`cn`), `http_url` (`https://host`, no path; plain `http://` only for a
loopback IP literal, a local test server), `quote_ws_url` (`wss://`),
`callback_port`, `research` (`CODE.MARKET` keys), `benchmarks` (bar-only
comparison keys; absent or `[]` disables extra benchmarks), `indexes` (dotted index
symbols such as `.SPX.US`), `rps` (1 to 20), `attempt_cap`,
`attempt_timeout_minutes`, `carry_deadline_minutes` (which must end before
the shortest window `window_start` can open: to 09:30 New York the next
day, one hour less across the spring DST change; 899 minutes at most for
17:30, capped at 600), `window_start` (`HH:MM` New York time, at or after
16:00) and `notify_command` (argv with an absolute program). The membership request asks for `limit=1000`, so an
index with more than 1,000 members never yields a complete membership.

**Environment.** The SDK reads `LONGBRIDGE_*`/`LONGPORT_*` variables and
loads a `.env` from the working directory or any parent into the process
environment (`src/env_guard.rs` cites the SDK lines). Any such variable,
or such a `.env` file, makes the binary exit `FATAL` before it reads the
config. The binary then sets `LONGBRIDGE_REGION` from `access_region`
itself, so the SDK never probes the network for the region.

Proxy variables (`HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY`, …) are deliberately
honoured: production reaches Longbridge through the host proxy, and both
reqwest clients in the SDK read them by default. TLS uses rustls with the
compiled-in webpki roots, so no `SSL_CERT_FILE` or native-certificate
override is in play.

**Lock.** `<data_dir>/archive.lock`, a non-blocking exclusive `flock`.
`auth import`, `auth login` and `upgrade-annual-catalog` take it before
opening auth/archive state; while another process holds it they exit `LOCKED`
without opening that state. After the environment guard and config, capture
(S1-2b) also takes it and exits `BUSY` (0) when it is held.

**Token.** The archive's own token lives in
`<data_dir>/oauth/<client_id>.json` (file 0600, written by temp file with
a random name, fsync and rename; a name that exists is skipped, never
removed), in the SDK's token-file format. That write, `files::replace`, is
the only one: the calendar cache, `last_exit.json` and `notify_state.json`
go through it too. Writable storage creates
`oauth/` as 0700, and sets an existing `oauth/` to 0700; the data dir keeps
whatever mode it has (it is created 0700 when missing). No command prints or
logs a token.

Every read and write checks the token's contents the same way: the access
token, and the refresh token when present, must be non-empty visible ASCII,
which the SDK can send. A token that fails is refused by `auth import`,
reported by `auth status` (exit 75), and never written. Errors name the
field, never the value. `--config`, `--from` and the stored token must be
regular files of at most 64 KiB. Each is opened with `O_NONBLOCK`, so a FIFO
cannot block the open, and its type is then checked on the open file
(`fstat`): a FIFO, a device or a directory is refused. `auth import` also
checks `--from` with `stat` before it takes the lock, so a wrong path never
holds the lock at all.

- `auth import --from <file>` copies a token file the SDK wrote for the
  configured client id. It refuses a file for another client id and a
  malformed one (the error names only the JSON category and position), and
  never writes the source file. Without `--force` it also refuses to replace
  a stored token that expires later, and refuses a token that is unusable:
  no refresh token and expiring within the SDK's five-minute refresh margin.
- `auth login` runs the SDK's browser login on `callback_port`, the redirect
  port registered for the client, printing the URL on stderr. It is the only
  path that runs the browser flow; a capture that would need it exits
  `AUTH_UNAVAILABLE` instead. The whole login ends within six minutes (the
  SDK's five-minute wait for the browser plus a minute for the code
  exchange, which has no timeout of its own); past that it exits
  `AUTH_UNAVAILABLE`, has saved nothing, and has released the lock. On a
  headless host the browser runs elsewhere and redirects to
  `localhost:<callback_port>`, so forward that port to the server first, for
  example `ssh -L <callback_port>:localhost:<callback_port> <host>`, then
  open the printed URL on the machine running ssh.
- `auth status` prints the stored token's expiry and whether it has a
  refresh token, and exits `AUTH_UNAVAILABLE` when the token is unusable.

**Exit codes** (`src/exit.rs`). `OK`, `BUSY` and `NO_WINDOW` are 0;
`PARTIAL` is 3, `LOCKED` 4, `DEGRADED` 5, `FATAL` 70 and `AUTH_UNAVAILABLE`
75. `upgrade-annual-catalog` exits `LOCKED` on lock contention. The capture
job's endings are listed under
[The capture job](#the-capture-job).

**Logs** go to stderr, and only this crate's target is emitted. Every
third-party target is off: the SDK logs full response bodies at `info` and
the OAuth callback's attacker-chosen `error` parameter at `warn`. SDK
errors are rendered only as a fixed variant name (`src/sdk_error.rs`),
never as their text.

**Source.** `LongbridgeSource::fetch` sends one catalog request at a paced
start (`rps`) and returns the Store's `Response`: the envelope's exact
`data` bytes as `Body`, or `Failed` classified as below. The Store, not the
source, judges a membership body. A `fetch` runs in this order:

1. It resolves the access token, a refresh included, within 30 s. A
   failure is `auth_unavailable` and sends nothing; like every auth pause
   it does not count toward the attempt cap.
2. It takes its rate-limit slot, so callers that waited behind a refresh
   are paced, not released together.
3. It sends within 60 s, the SDK's own retries included, through an HTTP
   client built on a snapshot of that token, which can never refresh. The
   token is resolved in exactly one place: a token that crosses the SDK's
   five-minute refresh margin while a fetch waits for its slot is still
   sent (it is valid until it expires), and the next fetch refreshes in
   step 1. The snapshot client is cached per token value.

A deadline can drop a refresh that the server has already committed. If
Longbridge rotates refresh tokens on use, the stored one is then spent and
the login is lost; `auth login` restores it.

- **Exact bytes hold for UTF-8.** The SDK reads the body with reqwest's
  `text()`, which replaces invalid UTF-8 with U+FFFD before the envelope is
  parsed. JSON is UTF-8 by specification, so a valid body is kept byte for
  byte; an invalid one would not be. A `charset` in `Content-Type` is
  ignored: reqwest transcodes only with its `charset` feature, which
  nothing in this build enables (a test pins it).
- **The limiter cannot pace SDK retries.** The SDK retries a bare HTTP 429
  up to five times inside one request, outside the limiter. The server
  throttles with `429xxx` envelope codes instead (`429002`, and `429003`
  seen in production), which the SDK does not retry, and the probe saw no
  bare 429.

| upstream result | status | `error_class` | `error_code` |
|---|---|---|---|
| envelope `429002` (the 1-second interval limit) | `throttled` (retry after the message's "retry after: Xs", at most 60 s, else 1 s) | `envelope` | `429002` |
| any other envelope code `429000`–`429999` (`429001` per IP, `429003`, …) | `throttled` (retry after the message's "retry after: Xs", at most 60 s, else 60 s: the interval is unknown) | `envelope` | the code |
| other non-zero envelope code, at any HTTP status | `permanent` | `envelope` | the code |
| `code: 0` with `data` absent or `null` | `permanent` | `unexpected_response` | `unexpected_response` |
| HTTP 200 that is not an envelope | `transient` | `unexpected_response` | `deserialize_response_body` |
| other non-envelope 1xx–3xx | `transient` | `unexpected_response` | `unexpected_http_response` |
| non-envelope 429, after the SDK's five retries | `throttled` (retry after 1 s) | `http_status` | `429` |
| non-envelope 401, 403 | `auth_unavailable` | `http_status` | the status |
| non-envelope 408, 5xx | `transient` | `http_status` | the status |
| other non-envelope 4xx | `permanent` | `http_status` | the status |
| request timeout, or the fetch timeout | `transient` | `transport` | `request_timeout` |
| connection or body error | `transient` | `transport` | `http` |
| token resolution failed, before any request | `auth_unavailable` | `auth` | the OAuth error variant (`no_refresh_token`, `refresh_token_failed`, …) |
| token resolution past its 30 s bound | `auth_unavailable` | `auth` | `refresh_timeout` |
| the SDK's own token or key check inside the request | `auth_unavailable` | `auth` | `oauth`, `invalid_access_token`, `invalid_api_key` |
| the SDK's data-center guard | `region_restricted` | `dc_region_restricted` | `dc_region_restricted` |
| the SDK could not build the request | `permanent` | `client` | the SDK variant |

## The capture job

`research-archive capture --config <file>` is one start of the job. A
systemd user timer starts it every 30 minutes; the job itself owns the
whole lifecycle of a run. Every decision and every stored timestamp comes
from one clock (`src/clock.rs`): the system clock in the binary, a fake one
in the library tests. There is no flag or variable that changes it.

**Start, in this order.**

1. The environment guard and the config (as for the `auth` commands;
   `notify` skips the guard, see [The alert](#the-alert)).
2. The data-dir lock. If another process holds it, the start exits `BUSY`
   (0) having opened nothing: no Store, no token, no request.
3. Open the Store and end every run whose window has closed (`window_end ≤
   now`) and that has no end as `partial`, reason `dangling`.
4. The calendar (below). A refresh builds the `OAuth` client first.
5. Find the window open now. With none, mark the missed days (below) and
   exit `NO_WINDOW` (0), or `PARTIAL` when step 3 ended a run.
6. Plan the run: `plan_run` for `(longbridge, D)` with the configured
   research list and indexes. When the day already has a run, the stored
   plan wins and the current config is not consulted (I12). A run that has
   ended exits 0.
7. Build the `OAuth` client (if not built yet) and the source, then run the
   phases.

**The calendar.** `<data_dir>/calendar/us.json` holds the US trading days
and half days of a contiguous date range and the time it was fetched, read
up to 1 MiB (more than a century of days).

- **Today first.** When the cache is missing, invalid or at least 24 hours
  old, today's range alone, today (New York) − 5 days to that + 30 days
  (the span the SDK asks for itself), is fetched through
  `QuoteContext::trading_days(Market::US, from, to)` on the quote WebSocket
  (`quote_ws_url`), merged with the cache when the two touch, and saved. A
  refresh that finds no usable token exits `AUTH_UNAVAILABLE`; any other
  failure falls back to the old cache with a warning while that cache still
  tells whether a window is open now, and is `FATAL` otherwise.
- **Then history, only when no window is open.** When the calendar does not
  reach back to the first run's `as_of` (the job was offline), the missing
  days are fetched in ranges of at most 31 days and merged, so every closed
  trading day of an outage can be marked `missed`. Missed days are closed
  days, so this runs only on a start that finds no window open: a start
  inside a window never spends its time on history. A failed back-fill only
  warns. As long as the history is incomplete, `last_exit.json` says
  `missed_scan_incomplete`, and every start without a window retries it,
  whatever the cache's age.
- **Half days.** The SDK returns trading days and half days as two lists and
  does not say whether they overlap, so the calendar's trading days are their
  union. A half day has a window like any trading day.

**The window of trading day D** runs from D at `window_start` New York
time to the next trading day at 09:30 New York time, both converted with
`America/New_York`, so daylight saving time is handled. A window is open at
`t` when `window_start(D) ≤ t < window_end(D)` for the latest trading day D
whose window has started. Half days do not change the window.

**Missed days.** With no window open, every trading day after the first run
whose window has closed (`window_end ≤ now`) and that has no run gets a
`missed` run. Days before the first run are never marked.

**Deadlines.** The start's deadline is `min(window_end, start +
attempt_timeout)`. An attempt starts only before it, checked when it is
dispatched and again at the actual request start, after the rate-limit slot
(`LongbridgeSource::fetch_if`), against the injected clock. A token
resolution or request still in flight at the deadline is dropped there.

The deadline is the clock's, not a timer fixed when a request starts:
`clock::within` follows the wall clock in steps of at most 250 ms
(`clock::WALL_STEP`), so an in-flight request, together with the SDK's own
retries of a bare HTTP 429 inside it, is dropped within one step of the
deadline, even when the wall clock steps forward (suspend, NTP) while it
waits. Every other wait on the system clock follows it the same way. The
send-time check (`window_start ≤ now < deadline`) and the recorder's window
check still refuse a request start or a row outside the window.

**A backward clock step.** A step back to before the window start sends
and records nothing more: the start ends `resumable`. A step back
that stays inside the window reaches the Store's monotonic run clock
([Writes](#writes)): the next row is refused (`ClockWentBack`) and the start
ends `FATAL` (exit 70), which alerts. Nothing is lost; the next start
resumes the run once the clock has passed its high-water mark again.

**The retry schedule.** Every pair still to be worked waits in one queue,
keyed by the time it may next be attempted (`src/capture/schedule.rs`). At
most four requests are in flight; the earliest eligible pairs go first, and
a pair waiting out its backoff holds no worker. Membership and coverage have
different rules.

- **Membership, before the freeze.** Each unfrozen index's constituents is
  retried every 2 minutes (`MEMBERSHIP_RETRY`), a `throttled` one after its
  `retry_after`. The cap does not apply. No retry is scheduled past the
  carry deadline: within a start that spans the carry deadline, the last
  starts 15 s before it.
  At the shipped cadence (a start every 30 minutes, each at most 25
  minutes, a carry deadline 60 minutes after the window start, with the
  window starting on the hour or half hour), the second start ends 55
  minutes in and the third begins after the carry deadline and freezes at
  once: the last constituents attempt is at about +54 minutes, and none is
  made in the 5 to 6 minutes before the carry deadline.
- **Coverage, after the freeze** (every planned pair, constituents
  included): a `transient`, `permanent` or `region_restricted` failure counts toward
  `attempt_cap` and waits 1, 4, 16, then 64 minutes (64 minutes from then
  on). The example config's cap of 6 spreads a pair's attempts over 149
  minutes (1 + 4 + 16 + 64 + 64), so a pair is lost only to an outage of
  about two and a half hours.
- A `throttled` failure waits its `retry_after` and never counts; neither
  does an auth pause.
- After the freeze, only the constituents pair's rows at or after the
  freeze count toward its cap; its failures before were the membership
  rule's, so a recovered upstream is still retried.
- The wait survives the end of a start: the next start derives each pair's
  next time from its last stored attempt. The wait a throttle named is not
  stored, so a pair whose last row is `throttled` waits the longest wait a
  server may name, 60 s, from that row.

**Phases.**

1. **Membership.** For each planned index not yet frozen, fetch
   `index_constituents` on the membership rule. The Store classifies each
   body.
2. **Freeze**, exactly when every planned index has an `ok` constituents row,
   or once `now ≥ carry_deadline`: `freeze_membership(run, now)` from the
   stored rows (`observed` with an `ok` of this run, else `carried` or
   `none`). The Store derives the modes; the runner only decides when.
   - A start that begins after the carry deadline freezes at once, before any
     attempt.
   - When the carry deadline falls inside this start, the start waits for it.
     A constituents attempt still in flight then is not waited for: it, and
     every later one, is coverage work, and changes nothing in the frozen
     universe.
   - When this start's deadline comes first, the run is left unfrozen, with
     no per-key fetch, for the next start (exit 0).
3. **Fetch**, only after the freeze: every planned pair (`coverage`) without
   an `ok` and below its cap, on the coverage rule.

**Core first.** After membership/freeze, settle every Core pair before sending
any Supplemental request, including while Core retries wait for backoff. Catalog
metadata owns this classification: the released eight endpoints are Core and the
four new endpoints are Supplemental. Each start resumes that same priority.
The tier is frozen with each request in `run_pair`; later catalog changes do
not change historical health. Store records `run_core_complete` once Core
settles (ok or capped), independently
of the overall run. This checkpoint never marks failures as successful data.

**The end.** A run ends `complete` only when it is frozen and every pair has
an `ok` or is at its cap; with the schedule above, a capped pair was retried
over a long span. With work left, a start whose deadline is the window end
waits for it and ends the run `partial`; any other start leaves it open.

**Config at each start.** The run's planned keys and its frozen index set
come from its plan and freeze, never from the current config (I12). This includes
the configured benchmark list; editing it affects the next daily plan. The
retry and timing policy (`attempt_cap`, `carry_deadline_minutes`, `rps`,
`attempt_timeout_minutes`) is read from the config at each start, so a
change applies to the open run from its next start.

**One serialized recorder.** Every observation, the freeze and the end are
written by one recorder that takes the timestamp after it has its turn and
writes under that turn. So `fetched_at` is the wall time the response was
**recorded**, and a run's clock never goes back, however the in-flight
requests finish (#2577).

**The first stop latches** (`src/capture/latch.rs`). When an attempt stops
the run (a request found no usable token, the window closed, or a write
failed), every other attempt in flight (its token resolution, rate-limit
wait or request) is dropped at once, the stop winning even when the
attempt's result is ready in the same poll; no request starts after the
latch (the request-start check asks the latch too); and nothing more is
dispatched. An auth failure latches *before* its own row is written, so a
slow write cannot let the attempts queued on the token or the rate-limit
slot go on. Only the attempt that latched records its auth row; a later
auth failure is dropped. Other responses that have already arrived are
recorded: the latch governs sending, not data received. If the auth row
cannot be written, the start is `FATAL`, not `AUTH_UNAVAILABLE`. Building
the `OAuth` client (a token refresh included) is bounded by
`capture::AUTH_BOUND` (30 s) within the start's deadline, so a hanging
refresh does not hold the lock for the whole start. An auth failure therefore never triggers one more token
refresh per request.

**The browser flow.** A token that the SDK could use only through the
browser login (none stored, or an expiring one whose refresh fails) stops
`build_oauth` at the flow's first step: it returns `BrowserFlow`, and the
start ends `AUTH_UNAVAILABLE` through its normal exit, `last_exit.json`
included. The SDK has already bound its callback listener on an ephemeral
loopback port (never the registered `callback_port`), and the task serving
it may outlive the dropped build until the runtime shuts down. In the
binary that is the process exit, moments later; nothing is sent to it, and
no browser is ever pointed at it. Likewise, a calendar refresh dropped at
its deadline leaves the SDK's quote task on the SDK's own runtime until the
process exits.

**Burst bound.** The SDK retries a bare HTTP 429 up to five times inside one
request, outside the limiter. The server throttles with `429xxx` envelope
codes instead, which the SDK does not retry. The worst burst beyond the
limiter is therefore four requests in flight × 6 = 24 requests within about
3.1 s.

**How a start ends.**

| ending | exit | `last_exit.json` kind | the run |
|---|---|---|---|
| every Core pair `ok` | 0 | `complete` | may remain open for Supplemental work |
| every Core pair `ok` or capped, some capped, at least half `ok` | 0 | `complete_with_failures` | may remain open for Supplemental work |
| Core settled, fewer than half Core pairs `ok` (`DEGRADED_BELOW` = 0.5) | 5 (`DEGRADED`) | `degraded` | may remain open for Supplemental work |
| the day's run had ended | 0 | `already_ended` | unchanged |
| the attempt timeout, Core still pending, window still open | 0 | `resumable` | left open; the next start resumes it |
| no window open | 0 | `no_window` | none |
| the lock is held | 0 (`BUSY`) | not written | untouched |
| the window closed with Core work left, or a dangling run lacks Core completion | 3 (`PARTIAL`) | `partial` | ended `partial` |
| Core/preflight without Core completion found no usable token, or calendar refresh needed browser login | 75 (`AUTH_UNAVAILABLE`) | `auth_unavailable` | left open; resumes after `auth import`/`login` |
| anything else | 70 (`FATAL`) | `fatal` | left open |

**`last_exit.json`** is written atomically before the lock is released:
`{invocation?, code, kind, at, as_of?, failure?, closed_dangling?,
missed_scan_incomplete?}`. `invocation` is the start's systemd
`$INVOCATION_ID`, when it runs under systemd. A start whose lock fails with an I/O error (not a
held lock) writes `fatal`, so the alert never reads an older start's state.

- `failure` is the Core tier's most frequent counted failure as
  `error_class:error_code`, for `complete_with_failures`, `degraded` and
  `partial`.
- `closed_dangling` lists runs without Core completion whose window closed and
  that this start ended `partial`, oldest first, as `{as_of, failure?}`. It
  is kept whatever the start's own ending: when the start would otherwise
  exit 0, it exits `PARTIAL` for the latest such day instead (with `failure`
  from that run); when it exits `FATAL` or `AUTH_UNAVAILABLE`, the dangling
  runs are still on record.
- `missed_scan_incomplete` is `true` when the calendar back-fill failed.
- It holds no token and no server text. A start that never took the lock
  writes none.

**A mostly lost Core day alerts.** Settled Core capture with fewer than
half of its Core pairs having an `ok` (`capture::DEGRADED_BELOW`, 0.5)
exits `DEGRADED`, so `OnFailure=` fires; at half or more, the losses are
ordinary (a few delisted symbols answering 2601400, a few capped pairs) and
the start exits 0 as `complete_with_failures`. This is what catches a
server-side auth rejection inside the envelope: its code was never probed,
so it is stored as `permanent`, not `auth_unavailable`, every pair reaches
its cap, and the Core result is `degraded` with `failure` naming the envelope
code.

## The alert

`research-archive notify --config <file> --failed-unit <unit>` is what the
capture unit's `OnFailure=` runs.

- **No environment guard.** `notify` builds no SDK client, so it does not
  refuse `LONGBRIDGE_*` variables or a `.env`: it alerts when the capture
  refused its environment.
- **It needs the config**, for `notify_command`. With an unreadable or
  invalid config it cannot alert at all: it logs the error and exits
  `FATAL`, and the failure is visible only in the journal
  (`systemctl --user status`).
- **The kind.** It reads `last_exit.json` and trusts it only when it is the
  failed start's own, non-zero state. Under systemd (251 or later; this host
  runs 252) the `OnFailure=` unit gets the failed start's id as
  `$MONITOR_INVOCATION_ID`, and the state counts only when its `invocation`
  matches. Without that variable (a manual run), a state at most 15 minutes
  old counts. Anything else is `unknown`: a capture that refused its
  environment, failed before it took the lock, or was killed by
  `TimeoutStartSec=` wrote no state of its own. Neither id is passed to the
  notify command, and the unit templates need no change for them.

It runs `notify_command` with one more argument, the message, for example:

```
research-archive: research-archive-capture.service failed: auth_unavailable (exit 75) at 2026-10-13T21:30:02+00:00, trading day 2026-10-13
```

It sends only when the kind differs from the last one sent or six hours
have passed, and records what it sent in `<data_dir>/notify_state.json`
after the command succeeded. A capture that works again (`complete`,
`complete_with_failures`, or `resumable`) deletes that file, so the first
failure after a recovery always alerts. The command runs without a shell, with stdin
closed, for at most 60 s, and with only these variables from the alert's
own environment: `PATH`, `HOME`, `LANG`, `TZ`, `XDG_RUNTIME_DIR` and the
proxy variables (`HTTP_PROXY`, `HTTPS_PROXY`, `NO_PROXY`, upper and lower
case). Nothing else is inherited, and no token data is passed.

## Installing the units

`systemd/` holds three templates: `research-archive-capture.service`,
`research-archive-capture.timer` and `research-archive-notify@.service`.
Replace the placeholders and install them as user units:

| placeholder | value |
|---|---|
| `@BIN@` | the release binary, absolute |
| `@CONFIG@` | the TOML config, absolute |
| `@DATA_DIR@` | the config's `data_dir` (create it with mode 0700 first) |
| `@TIMEOUT_SEC@` | `TimeoutStartSec=` of the capture: above `attempt_timeout_minutes × 60`, for example that + 300 |
| `@PROXY_ENV_FILE@` | only if a proxy is needed (below) |

```sh
units=~/.config/systemd/user
for f in systemd/*; do
  sed -e "s|@BIN@|$BIN|g" -e "s|@CONFIG@|$CONFIG|g" \
      -e "s|@DATA_DIR@|$DATA_DIR|g" -e "s|@TIMEOUT_SEC@|1800|g" \
      -e "s|@PROXY_ENV_FILE@|$PROXY_ENV_FILE|g" "$f" > "$units/$(basename "$f")"
done
systemctl --user daemon-reload
systemctl --user enable --now research-archive-capture.timer
```

- **The service is `Type=oneshot`.** Its backstop above the job's own
  deadline is `TimeoutStartSec=`: `RuntimeMaxSec=` has no effect on a
  oneshot service (`systemd.service(5)`).
- **The capture's `WorkingDirectory=` is the data dir.** The SDK's dotenv
  walks up from the working directory, so the job still refuses to start
  when a `.env` exists in the data dir or any parent. The notify unit runs
  in `/`: it builds no SDK client, and a missing data dir must not keep it
  from alerting (`unknown`).
- **The timer can fire before time sync.** `Persistent=true` catches up a
  missed start at boot, possibly before the clock is synchronized, and a
  user unit cannot order itself after `time-sync.target` (a system unit).
  Such a start works on the unsynchronized time; when the sync then steps
  the clock back mid-run, it ends as above (`resumable` or `FATAL`), and a
  later start resumes the run.
- **No unit sets an SDK variable.** The region comes from the config, and the
  job refuses to start when any `LONGBRIDGE_*`/`LONGPORT_*` variable is set.
- **A proxy, only if this host needs one.** Uncomment `EnvironmentFile=` in
  both services and point it at an operator-owned file (mode 0600) holding
  only `HTTPS_PROXY=`, `HTTP_PROXY=` and `NO_PROXY=` lines.

`systemd-analyze verify` on rendered copies reports no problem.

## Status plugin (S1b)

`research-archive-plugin` is a Neige plugin (`manifest.json`, plugin id
`archive`: an installable id is one word, `^[a-z0-9]{2,32}$`) that reports
the archive's health to Planners and Reports, and reads archived values as
of a past moment. The capture stays the systemd timer's; the plugin captures
nothing and changes nothing.

**What it reads.** Two files in the configured data dir, and nothing else:

- `archive.sqlite3`, through `ReadOnlyStore`: opened `SQLITE_OPEN_READONLY`,
  never created, with a busy timeout, and refused on the same `user_version`,
  schema and catalog checks as `Store::open`. Like any WAL reader it may
  create the `-wal` and `-shm` sidecars when they are missing, so it runs as
  the capture's user, which can write the data dir (see
  [Importing read-only](#importing-read-only)).
- `last_exit.json`, how the last capture start ended.

It never takes `archive.lock`, never reads `oauth/` or any token, and calls
no host. It works while a capture holds the lock and writes. A source scan
(`plugin::the_plugin_reaches_only_the_read_side`) pins the modules it can
reach.

**Configuration** (`PATCH /api/plugins/archive/config`):

| Key | Default | Meaning |
|---|---|---|
| `data_dir` | required | absolute path of the capture's data dir (its config's `data_dir`) |
| `poll_seconds` | 60 | seconds between status reads for watching Tracks (10 to 3600) |
| `stale_after_minutes` | 90 | minutes after the last recorded exit at which the timer counts as stopped (60 to 2880); see below |

A missing or out-of-range key fails every tool call with the reason. The
plugin checks the ranges itself: the kernel's `config_schema` subset has no
`minimum`/`maximum`. `stale_after_minutes` must exceed 30 (the timer's
interval) plus the capture's `attempt_timeout_minutes`, with a margin: a
start can hold the lock that long, and a start that finds the lock held
records nothing. The default 90 fits an `attempt_timeout_minutes` up to
about 45.

**Tools.**

- `capture_status` answers `ok`, `attention` or `failing`, every finding, the last
  exit and the last 7 runs with their pair counts (ok, tried with no ok, not
  tried, planned) and frozen membership. One source per fact:
  - **The Core tier** records consensus/archive data loss for good. Core that ended `partial`, or
    `complete` but degraded (fewer than [`DEGRADED_BELOW`](src/types.rs) of
    its Core pairs `ok`, the capture's own `DEGRADED` rule), is `failing` until a
    later day ends `complete`, which demotes it to `attention`. A later
    `missed` or open run does not, so an outage across several windows (a
    day closed `partial` at the next start, the days after it `missed`)
    stays `failing`. A `missed` day is `attention`, and so is a run
    `complete` with Core pairs that have no `ok`. Pending Core is not judged.
    Supplemental failures, quotas, auth pauses and timeouts are report-only;
    overall partial/open does not invalidate an existing Core checkpoint.
  - **`last_exit.json`** tells only what the runs cannot: it is `failing`
    when it is missing or older than `stale_after_minutes`, or when the last
    start had no usable token (`auth_unavailable`) or stopped on `fatal`;
    an incomplete missed-day scan is `attention`. The capture overwrites
    the file on every start, so a `partial` or `degraded` exit read from it
    would turn healthy 30 minutes later.
  - An archive that is missing or refused is `failing`.

  No finding names the exit's time (the staleness finding names it, as it
  is fixed while stale), so the findings change only when the state does.
  The tool's text adds when the last start ended and when it checked.
- `capture_watch {on}` acts on the calling Track, which the kernel names in
  `params._meta["dev.neige/track"].id`. `on: true` stores `watch/<track>` in
  the plugin's KV; the poller then pushes the `archive.status` table overlay
  to that Track, and later only when the table changes. `on: false` deletes
  the key and the overlay.

  A Track deleted while watching keeps its `watch/<track>` key: the plugin
  is not told of the deletion. Each change of the table is still pushed to
  it, and the kernel's `neige.overlay.set` stores it without checking that
  the Track exists, until the key is removed (`capture_watch` with `on: false`
  names only the calling Track, so by clearing the plugin's KV). Each key
  is about 40 bytes of the 16 KiB quota.

- `read_at {at, reads}` (#2597) answers what the archive held at `at`, an
  RFC 3339 time with an offset. Each of 1 to 12 reads names a `symbol`, one
  of the catalog's security `endpoint`s (the manifest's enum is pinned to
  the catalog by a test; `index_constituents` is not readable here) and an
  RFC 6901 `pointer` into the body (`""` is the whole body). Each read is
  [`read_at`](#read-contract) through `ReadOnlyStore`, the same query, and
  answers one `result`:

  | `result` | when | fields beyond `symbol`, `endpoint`, `pointer` |
  |---|---|---|
  | `value` | an `ok` row with `fetched_at ≤ at`, and the pointer resolves | `as_of`, `fetched_at` (RFC 3339 UTC), `value` |
  | `pointer_missing` | that row's body has nothing at the pointer | `as_of`, `fetched_at` |
  | `no_value` | the pair was planned in some run, but no `ok` row is at or before `at` | none |
  | `not_archived` | no run ever planned the pair (`PLANNED_PAIRS`; an index symbol is planned only for the index endpoint) | none |

  The first data was fetched 2026-10-13 (UTC), for trading day 2026-10-12;
  visibility is by `fetched_at`, so an earlier `at` gives `no_value`
  (`not_archived` for a pair never planned). A stored body that is not JSON
  fails the call naming the read; the capture stores the envelope's `data`
  JSON as is, so it never happens. A pointer with a `~` not followed by `0`
  or `1` is refused with the arguments. The answer (`{at, reads}`, as
  compact JSON in the text and the same object in `structuredContent`) is
  capped at 40 KiB: over it the call fails with each read's size, and
  nothing is truncated. Codex binds the cap: it shows the model only
  `structuredContent` and middle-truncates a tool output over 10,000 tokens
  × 1.2 × 4 bytes ≈ 48,000 bytes, so 40 KiB keeps a margin (Claude Code's
  MCP cap, 25,000 tokens, is higher).

  Bodies are large: in production consensus averages about 20 KB,
  forecast_eps 26 KB (up to 271 KB), rating_detail 51 KB and valuation 14 KB,
  as valuation and rating_detail carry the vendor's own long time series. The
  top-level shape of each endpoint's body, for picking pointers, is in the
  tool's description in `manifest.json` (the SDK's types at v5.2.0, checked
  against NVDA.US in production), where the Planner reads it.

The manifest's `planner_instructions` tell the Planner to ask the owner with
`neige_user_ask` on `failing` (again only when the reasons change) and to
report `attention` in the Report without asking, and name `read_at` for
where consensus, ratings or target prices stood at a time. The plugin itself
sends no alert anywhere.

**Report block.** A Report of a watching Track shows the live table with

````markdown
```neige-block table
{ "source": "neige://plugin/archive/archive.status" }
```
````

The table has one row per recent run, and the verdict with its findings in
the caption (capped at the block's 2048 characters). It holds no check or
exit time, so it is pushed only when the status changes.

**Install.** Build the binary and stage the manifest next to it:

```sh
cargo build --release --locked -p research-archive --bin research-archive-plugin
dir=<plugins_dir>/archive
mkdir -p "$dir/bin"
cp plugins/research-archive/manifest.json "$dir/"
cp "${CARGO_TARGET_DIR:-target}/release/research-archive-plugin" "$dir/bin/"
```

Then, against the server:

```sh
curl -X POST "$CALM/api/plugins/install" -H 'content-type: application/json' \
  -d "{\"source\":{\"kind\":\"local_path\",\"path\":\"$dir\"}}"
curl -X PATCH "$CALM/api/plugins/archive/config" -H 'content-type: application/json' \
  -d '{"data_dir":"<the capture config'"'"'s data_dir>"}'
curl -X POST "$CALM/api/plugins/archive/enable"
```

A config change takes effect after `POST /api/plugins/archive/reload`.

## Tests

Run `cargo nextest run -p research-archive`. The source and CLI tests use a
local fake HTTP server and fabricated tokens; none reaches a real host.

Two integration binaries:

- **`suite`** drives the Store and the binary as a subprocess.
- **`net`** holds every test that builds an SDK `OAuth` or a
  `LongbridgeSource` in process. Its harness serializes them, refuses a
  `.env` in reach, and pins the proxies to a local black hole, `NO_PROXY`
  to loopback, `LONGBRIDGE_ENV` and `LONGBRIDGE_REGION`. A source scan
  (`net::scan`) fails if any other test code names the SDK or builds a
  client: every file under `tests/` but `tests/net/`, every `src/**/tests.rs`
  and every `#[cfg(test)]` item in `src/`. Production code is not scanned.
  Its patterns are regexes over whole files, comments included. The scan
  catches a test that builds an SDK client accidentally placed outside the
  hermetic harness; it is not a defence against deliberate evasion.
- **Harness note.** `hermetic::pinned` sets process-wide environment once;
  its `SAFETY` argument assumes nextest's one-process-per-test model. Under
  `cargo test` the binary's tests share a process and are serialized by a
  lock, but they do not isolate the environment from each other.

| test module | covers |
|---|---|
| `guards` | I2, on every table, including the rowid collision |
| `open` | new archives use v3 and the compiled catalog; the refusals, and that a refused file is untouched |
| `annual_catalog` | frozen released v1 input; explicit upgrade preserving historical bytes/plans, schema/catalog refusals without writes, CLI lock (`LOCKED`), token-free upgrade and idempotence |
| `writes` | plan atomicity, the window bound and window order, `complete`, the attempt counter and the CHECKs |
| `clock` | the monotonic run clock, blob + observation atomicity, error class per endpoint |
| `freeze` | the derived freeze, Store-classified membership bodies, I5, atomicity, carry vs `read_at` |
| `reads` | I6, the universe, read order and coverage |
| `send` | every public async method returns a `Send` future |
| `cli` | the environment guard, the lock (`LOCKED`), `auth import`, `status` and `login` as subprocesses |
| `net::source` | I3 through the Store (also under a misleading charset), the request of every catalog entry, the classification table |
| `net::crossing` | a token crossing the refresh margin during the slot wait: no refresh inside a send, never `transient` |
| `net::refresh`, `net::login` | a hung token refresh is bounded and releases the `OAuth` lock; a hung code exchange ends `login` by its deadline |
| `net::logs` | only our own log target is emitted |
| `net::secrets` | I11 for the binary's output, our logs, the archive file, `Debug`, and a login callback carrying a canary |
| `net::dotenv`, `net::scan` | a `.env` in reach is refused and cannot override the pins; no SDK client is built, and no capture started, outside the harness |
| `net::runner` | on a fake clock, under the harness: I1, I7, freeze before fetch, resumption, the carry deadline, missed and dangling days, the serialized recorder, a hanging request, the `degraded` threshold, the first auth failure stopping every worker |
| `net::calendar` | the calendar refresh's error mapping, the back-fill of a 40-day outage (every closed day `missed`), a failed back-fill that only warns and is retried, no back-fill inside a window (a hanging history request does not touch the capture), the fallback to a cache that still covers now |
| `net::schedule` | the retry schedule through an outage; membership scenarios A (a blip is retried and `observed`), B (retried every 2 minutes, then a freeze at the carry deadline, across starts or within one) and C (a start to the window end waits for the carry deadline, never `partial`); constituents outages of 22, 25, 40 and 55 minutes `observed`; a late start freezing from stored rows; a resume after the carry deadline not held up by a hanging constituents attempt; the auth latch before a slow auth row; of two simultaneous auth failures only the winner recorded; an unwritable auth row `FATAL`; a throttle wait surviving a restart; no request starts after the window end however late its rate-limit slot |
| `net::membership` | constituents outages at the shipped cadence (25-minute starts every 30 minutes); the freeze not waiting for a retry in flight at the carry deadline; only post-freeze failures counting toward the constituents cap |
| `net::interrupt` | I4 and I12 in process: a capture interrupted during membership leaves a whole plan and no freeze; the rerun completes with the planned keys and index set |
| `net::exit_state` | dangling runs on record (all of them) through a later failure; a dangling-only `partial` with its failure; a lock I/O error on record; a completed or resumable start clearing the alert dedupe |
| `net::deadline` | a clock jump past the window end stopping the SDK's in-request retries within a step; a clock stepped back before the window start sending nothing, and inside the window ending `FATAL`; stamps taken under the recorder's turn with parallel workers and a jittery clock |
| `capture_cli` | `capture` as a subprocess: a kill during the fetch (I4), I9, a hanging token refresh, I10, the calendar refresh. The membership phase cannot be reached in real time at every hour, so its kill and I12 run in `net::interrupt` |
| `capture_secrets` | I11 for `capture` and `notify`; the alert's dedupe and environment; `notify` alerting when `capture` refused its environment; `notify` trusting only the failed start's own state (`$INVOCATION_ID` / `$MONITOR_INVOCATION_ID`) |
| `units` | the systemd templates' directives and placeholders |
| `read_only` | `ReadOnlyStore`: never creates or changes the file, refuses what `Store::open` refuses, reads while the capture writes |
| `status` | the health rules over a real archive and `last_exit.json` |
| `plugin` | the plugin binary against a fake kernel: `capture_status`, `capture_watch` and its overlay pushes (only on change), the config, re-`initialize`, the lock held during reads, and the source scan of what it reaches |
| `plugin_read_at` | `read_at` through the plugin binary over a real archive: the row visible at a time, the four results, pointers, the size cap, the argument checks, and the manifest schema against the catalog |

## Annual consensus and schema version 2

`consensus` keeps its released default-view request. `consensus_annual` is a
separate security endpoint on the same path with `period_type=af`; the parameter
was checked against a live response. `period=af` and `report=af` do not select the
annual view. The read-only plugin exposes both through `read_at`.

The released v2 format introduced an append-only `run_endpoint` table freezes the
endpoint set in the same transaction as each run's plan. Coverage, completeness,
write eligibility and `ever_planned` use that stored set: adding an endpoint must
not turn an old completed run into a newly incomplete run.

A released v1 archive is upgraded explicitly by the operator, under the existing
data-directory lock:

```sh
research-archive upgrade-annual-catalog --config /path/to/research-archive.toml
```

The command needs no token and sends no network request. It verifies the frozen
v1 schema and the exact legacy catalog, backfills old run endpoint sets before
adding the annual endpoint, and commits the schema version and catalog together.
Missing files and schema/catalog drift are refused. Ordinary opens and the
read-only plugin never migrate a file. Repeating the explicit upgrade is a no-op.

Before upgrading, pause captures and take a consistent SQLite backup with
`sqlite3 archive.sqlite3 ".backup pre-v2.sqlite3"`; verify that it opens and
record its checksum. Do not copy only the main file while WAL commits may exist.
Coordinate the capture binary and plugin rollout, and reload the matching
plugin before resuming. The old binary cannot
read v2, so rollback needs a v2-capable build rather than copying the old binary
back. The first annual capture has its real fetch time; past annual history is
not reconstructed from today's response.

The upgrade command remains a supported, permanent v1-to-v2 entry point for
offline copies and installations that have not upgraded. It is not removed
after the deployed archive has migrated. Its accepted legacy contract stays
frozen; it does not become a general catalog repair command.

The v1-to-v2 command deliberately recognizes only the released legacy catalog
and this annual addition. A future catalog change must update the explicit
upgrade policy and its compatibility tests; do not silently generalize the
accepted drift set. The migration does not infer missing historical endpoint
sets from observations.

## Market capture and schema version 3

See [design and probe limitations](../../docs/research-archive-market-capture.md).
The capture binary and the read-only plugin must be upgraded together. Ordinary
opens require v3 and never upgrade an existing file. With captures paused, take
and verify a consistent SQLite `.backup` (including committed WAL contents), then:

```sh
# Only if the archive is still v1; this remains narrowly v1→v2.
research-archive upgrade-annual-catalog --config /path/to/research-archive.toml
# Explicit v2→v3, holding the same archive.lock; no auth or network needed.
research-archive upgrade-market-catalog --config /path/to/research-archive.toml
```

The market command refuses v1, missing files, schema/catalog drift, and broken
foreign keys. Repeating it on a valid v3 archive validates without migrating.
It reconstructs only the two closed-set tables (`endpoint` and `run_key`),
retaining every historical column value and recreating their append-only guards.
The original v1 DDL and v2 migration remain byte-frozen. This is an explicit
format upgrade, not an ordinary append operation. No past day's observations are
backfilled. The old binary cannot open v3: rollback requires a v3-capable build,
or deliberate reconciliation of post-backup captures before restoring a backup.
This PR does not perform any production migration, rollout, or plugin reload.

V3 adds append-only `run_plan_v3(run_id)` and
`run_pair(run_id,key,endpoint_id,request_json)`. A marker identifies runs using
explicit immutable pair requests. Plan creation commits initial pairs with the
keys; membership freeze commits each new member's pairs with its keys. Each
request is an ordered JSON array of resolved string pairs; `capture_tier`
freezes its Core/Supplemental classification in that same row. Dates come from the
run's `as_of`, so midnight, changed config, and resumption never change a request.
Historical runs without a marker keep the released `run_key × run_endpoint`
relation, including an old run whose index membership freezes after upgrade.
All coverage, completeness, write eligibility, and `ever_planned` use that one
Store-owned relation. Marker and request tables reject updates, deletes, and
replacement/conflict inserts under either recursive-trigger setting.

`endpoint.method` now identifies a request contract: `GET` retains its released
meaning; `GET_CALENDAR_V1` means paginated HTTP GET with an auditable page wrapper;
`QUOTE` with `quote:27` means the declared daily-bar WebSocket projection. Scope
and transport are independent. The query recipe admits `{ "as_of_days": N }`
for dates resolved and persisted at planning. A changed request or codec needs a
new endpoint id; legacy catalog fields are never rewritten.

| endpoint | scope / request | body |
|---|---|---|
| `earnings_snapshot` | research securities, daily; symbol, upstream latest report | raw `data`: report, fiscal_year/period, currency, fp_start/end, fo_* and fr_* |
| `financial_reports` | research securities, daily; symbol, kind=ALL, report=qf | raw `data`: list.IS/BS/CF.indicators[].accounts[].values[] |
| `finance_calendar` | one `market:US` key, daily; as_of−7 through as_of+30, report events | `earnings_calendar_v1` |
| `daily_candlesticks` | research securities plus configured `benchmarks` | `daily_bars_v1` |

The optional `benchmarks` TOML list lives next to `research` and `indexes`. The
example selects QQQ/SOXX/KBE/SPY/SMH; these are operator research choices, not a
Store constant. Symbols are validated and unique. Absent or `[]` means no
additional benchmarks; existing deployed configs and upgrade commands continue
to load without editing the production config. Research/member stock bars still
run. Add the list explicitly to enable comparison ETFs on a future daily plan.
No production config is edited by this PR.

Benchmark-only keys have origin `benchmark` and only get daily bars. The US
calendar has origin `market` and is not passed as a security symbol. Overlapping
research/benchmark origins remain separate facts, with one shared bar pair.
Reports are daily in this first version, covering weekly/post-earnings needs
without a separate scheduling policy. A successful daily snapshot may precede
later same-night earnings updates; capture does not promise continuous freshness.

Calendar coverage is `upstream_group_company_dates`: API-published company/date
representatives, not every underlying fiscal variant or verified NY earnings
date. Live probes explain why count cannot be compared with unique event IDs:
on group2026-10-14, report count55 yields46 nonempty raw positions,36 IDs and22
companies; financial-only yields46 IDs for the same22 companies. A broad report
page displays22 representatives. qf/3q/saf/af variants and normalized report IDs
are not additional company dates. Reported count remains source metadata; its
exact private-backend meaning is not inferred.

Page requests use count100. A short normalized page can precede more companies:
continue through the stable reported offset bound, advancing by at least one and
otherwise by returned current-group infos length. This conservatively overlaps
positions rather than assuming the requested size was consumed. Identical pages
are valid; conflicting event versions, changing counts, malformed data, failed
HTTP pages, or cancellation fail the logical pair. Strictly increasing offsets,
a100,000 bound per group,2,048 pages,8MiB raw data and the run deadline bound work.
No partial failed/cancelled window is published.

The completed wrapper has `coverage,market,start,end,reported_counts,company_counts,
days,pages`: `days[GROUP_DATE][SYMBOL][EVENT_ID]` retains observed representatives.
Each page retains `date,offset,count,fetched_at,raw_json`; decoding raw_json
reproduces exact upstream data bytes. The wrapper itself is not a raw HTTP body.
Absent reported_counts keys mean the source provided no such group; derived
company_counts describe published values, not an inferred source total.
Group dates, event date/datetime and ext.local_date may disagree; they remain
unchanged. Never assume group keys are New York event dates.

Daily bars query as_of−14 calendar days through as_of and retain the latest six
available closes (five returns). The body declares `format,symbol,timezone,start,
end,session,adjust,bars`; each bar has `trade_date,timestamp,open,high,low,close,
volume,turnover`. NY session dates, regular hours, and no adjustment are explicit;
prices/turnover are decimal strings. Invalid/nonpositive/inconsistent OHLC,
negative volume/turnover, duplicate dates, and dates outside the query fail the
pair. This is a validated projection of the official protobuf response: unknown
protobuf fields are not retained. Decimal parsing rejects malformed strings;
zero volume/turnover remain valid, and their original strings are retained.
This endpoint does not claim raw-wire preservation. The pinned SDK's detached
quote queue cannot fence cancelled sends, so bars use a narrowly scoped owned
socket adapter. SDK OAuth, HttpClient and OTP/connection-limit checks are reused.
Actual writes are paced and fenced, and cancellation drops the socket; no
background reconnect or business request queue survives the caller.

Use the existing `plugin_archive_read_at` with symbol for security endpoints, or
`{"market":"US","endpoint":"finance_calendar","pointer":"/days/2026-10-21/MSFT.US"}`
for the calendar. One call can read a stock and its benchmark. Align actual
`trade_date` values; the same `at` does not guarantee matching sessions. Missing
sessions are missing, never an older-date substitute. `as_of` is the capture's
trading day; `fetched_at` is knowledge time, not an event/report/trade date.
Existing `not_archived`, `no_value`, empty-value, and 40 KiB limits remain.

`capture_status.runs[].endpoints` reports planned/ok/failed/untried counts per
endpoint, including market and benchmark pairs, with a `tier` classification.
The eight released endpoints are Core; the four additions are Supplemental.
`runs[].core` and `runs[].supplemental` each expose planned/ok/failed/untried;
`core_outcome` and `core_finished_at` describe Core independently of the overall
`outcome`/`finished_at`. Total `pairs_*` counts still cover both tiers. Report
rows show both tiers explicitly; supplemental gaps never add health findings or
OnFailure notifications. Missing starts/unreadable storage remain operational
failures. Core auth/data loss, degraded ratios and membership warnings retain
existing semantics. Old run history is unchanged and receives no checkpoint
backfill. `complete` in either tier can include capped failures; counts expose
them rather than treating failure as data.

The sample token read MSFT and all five benchmarks successfully. This does not
prove monthly symbol capacity for the full universe. Permission/quota failures
keep their numeric codes (including 301604/301607); no fallback bypasses them.
Sample reports include MSFT CapEx and JPM BPS but did not establish separate
finance-lease additions, segments, or TBVPS. Missing actuals must not be inferred.
The corrected production-source live window (2026-10-13…2026-11-19) completed:
199 pages,2,246 upstream group/company date keys,5,534,482 raw bytes,139 seconds.
The22-company historical sample and181-company boundary sample matched their
completed exhaustive small-page references. These probes held the archive lock,
disabled refresh/storage writes, and never opened the production database.

At N=505, five additional benchmarks and one index, first-pass logical pairs are
10N+5+1+1=5,057. At 2 starts/s that is about 42 minutes before calendar pagination,
quote bootstrap, latency and retries. Retain the 25-minute attempt and 30-minute
timer: saved successful pairs resume, and one invocation need not complete.

The measured199-page calendar adds198 starts over its single logical pair:
approximately44 minutes of ideal start pacing for the expanded first pass,
plus bootstrap, latency and retries. The calendar itself completed within one
25-minute attempt; successful stock pairs still resume across timer starts.

Follow-ups: [SDK adapter removal and financial-report cadence #2993](https://github.com/keanji-x/neige-calm/issues/2993);
[existing market.series US chart/501 defect #2994](https://github.com/keanji-x/neige-calm/issues/2994).
Archive closes do not themselves repair the Report chart data source.
