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
  `t`.

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
    differs in its stored SQL text from what `DDL` produces.
  - **Catalog drift.** A stored `endpoint` row is missing, extra or changed
    compared with the compiled catalog.
- **A refused file is left as it was.** The WAL journal mode is set only
  after the check (or the creation) succeeds.

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
3. Check `PRAGMA user_version` = 1 before reading.

Verified with the system `sqlite3` 3.40.1 against a Store-created file:
`?mode=ro` on a copy without sidecars in a mode-555 directory fails; `.backup` (also with a writer
holding the file open) and checkpoint-then-copy, each opened with
`immutable=1` in a mode-555 directory, read every row and create no file.

Never run `ANALYZE` or `PRAGMA optimize` on the live file: they create
`sqlite_stat*` tables, which `Store::open` refuses as schema drift. Run them,
if at all, on a copy.

## The binary

`research-archive <command> --config <file>`. Every command that can build
an SDK client (`capture` and the `auth` commands) first refuses a hostile
environment, then reads the config, then sets the region, all before any
thread starts. `notify` builds none and only reads the config.

**Config.** A TOML file with every field required; unknown fields are
refused. `research-archive.example.toml` documents each field:
`data_dir` (absolute), `oauth_client_id`, `access_region` (`global` or
`cn`), `http_url` (`https://host`, no path; plain `http://` only for a
loopback IP literal, a local test server), `quote_ws_url` (`wss://`),
`callback_port`, `research` (`CODE.MARKET` keys), `indexes` (dotted index
symbols such as `.SPX.US`), `rps` (1 to 20), `attempt_cap`,
`attempt_timeout_minutes`, `carry_deadline_minutes`, `window_start`
(`HH:MM` New York time, at or after 16:00) and `notify_command` (argv with
an absolute program). The membership request asks for `limit=1000`, so an
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
`auth import` and `auth login` take it; while another process holds it they
do nothing and exit `LOCKED`. The capture (S1-2b) takes it first and exits
`BUSY` (0) when it is held.

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
75. The capture job's endings are listed under
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
  throttles with envelope `429002` instead, which the SDK does not retry,
  and the probe saw no bare 429.

| upstream result | status | `error_class` | `error_code` |
|---|---|---|---|
| envelope `429002` | `throttled` (retry after the message's "retry after: Xs", at most 60 s, else 1 s) | `envelope` | `429002` |
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
- **Then history.** When the calendar does not reach back to the first
  run's `as_of` (the job was offline), the missing days are fetched in ranges
  of at most 31 days and merged, so every closed trading day of an outage can
  be marked `missed`. A failed back-fill never blocks the capture: it warns,
  records `missed_scan_incomplete` in `last_exit.json`, and every later start
  retries it, whatever the cache's age.
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

The I1 fence relies on the clock and the runtime's timer agreeing: in
production the clock is the system wall clock, so the timer that drops an
in-flight request (`clock::within`) expires at the deadline, and the SDK's
own retries of a bare HTTP 429 inside that request are cut there too. A
forward step of the wall clock (an NTP correction) after the timer was set
shifts when it fires relative to the wall clock; the send-time check and the
recorder's window check still refuse a request start or a row past the
window end.

**The retry schedule.** Every pair still to be worked waits in one queue,
keyed by the time it may next be attempted (`src/capture/schedule.rs`). At
most four requests are in flight; the earliest eligible pairs go first, and
a pair waiting out its backoff holds no worker. Membership and coverage have
different rules.

- **Membership, before the freeze.** Each unfrozen index's constituents is
  retried every 2 minutes (`MEMBERSHIP_RETRY`), a `throttled` one after its
  `retry_after`. The cap does not apply. No retry is scheduled past the
  carry deadline; the last starts 15 s before it.
- **Coverage, after the freeze** (every planned pair, constituents
  included): a `transient`, `permanent` or `region_restricted` failure counts toward
  `attempt_cap` and waits 1, 4, 16, then 64 minutes (64 minutes from then
  on). The example config's cap of 6 spreads a pair's attempts over 149
  minutes (1 + 4 + 16 + 64 + 64), so a pair is lost only to an outage of
  about two and a half hours.
- A `throttled` failure waits its `retry_after` and never counts; neither
  does an auth pause.
- The wait survives the end of a start: the next start derives each pair's
  next time from its last stored attempt.

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

**The end.** A run ends `complete` only when it is frozen and every pair has
an `ok` or is at its cap; with the schedule above, a capped pair was retried
over a long span. With work left, a start whose deadline is the window end
waits for it and ends the run `partial`; any other start leaves it open.

**Config at each start.** The run's planned keys and its frozen index set
come from its plan and freeze, never from the current config (I12). The
retry and timing policy (`attempt_cap`, `carry_deadline_minutes`, `rps`,
`attempt_timeout_minutes`) is read from the config at each start, so a
change applies to the open run from its next start.

**One serialized recorder.** Every observation, the freeze and the end are
written by one recorder that takes the timestamp after it has its turn and
writes under that turn. So `fetched_at` is the wall time the response was
**recorded**, and a run's clock never goes back, however the in-flight
requests finish (#2577).

**The first stop latches.** When an attempt stops the run (a request found
no usable token, the window closed, or a write failed), every other attempt
in flight (its token resolution, rate-limit wait or request) is dropped at
once, and nothing more is dispatched. An auth failure latches *before* its
own row is written, so a slow write cannot let the attempts queued on the
token or the rate-limit slot go on; the auth row is still recorded, and any
other row already waiting for the recorder is dropped. An auth failure
therefore never triggers one more token refresh per request.

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
request, outside the limiter. The server throttles with the envelope code
429002 instead, which the SDK does not retry. The worst burst beyond the
limiter is therefore four requests in flight × 6 = 24 requests within about
3.1 s.

**How a start ends.**

| ending | exit | `last_exit.json` kind | the run |
|---|---|---|---|
| every pair `ok` | 0 | `complete` | ended `complete` |
| every pair `ok` or at its cap, some at the cap, at least half `ok` | 0 | `complete_with_failures` | ended `complete` |
| every pair `ok` or at its cap, fewer than half `ok` (`DEGRADED_BELOW` = 0.5) | 5 (`DEGRADED`) | `degraded` | ended `complete` |
| the day's run had ended | 0 | `already_ended` | unchanged |
| the attempt timeout, window still open | 0 | `resumable` | left open; the next start resumes it |
| no window open | 0 | `no_window` | none |
| the lock is held | 0 (`BUSY`) | not written | untouched |
| the window closed with work left, or a dangling run was ended | 3 (`PARTIAL`) | `partial` | ended `partial` |
| a request or the calendar refresh found no usable token, or the SDK wanted the browser flow | 75 (`AUTH_UNAVAILABLE`) | `auth_unavailable` | left open; resumes after `auth import`/`login` |
| anything else | 70 (`FATAL`) | `fatal` | left open |

**`last_exit.json`** is written atomically before the lock is released:
`{code, kind, at, as_of?, failure?, closed_dangling?,
missed_scan_incomplete?}`. A start whose lock fails with an I/O error (not a
held lock) writes `fatal`, so the alert never reads an older start's state.

- `failure` is the run's most frequent counted failure as
  `error_class:error_code`, for `complete_with_failures`, `degraded` and
  `partial`.
- `closed_dangling` lists every run left open whose window had closed and
  that this start ended `partial`, oldest first, as `{as_of, failure?}`. It
  is kept whatever the start's own ending: when the start would otherwise
  exit 0, it exits `PARTIAL` for the latest such day instead (with `failure`
  from that run); when it exits `FATAL` or `AUTH_UNAVAILABLE`, the dangling
  runs are still on record.
- `missed_scan_incomplete` is `true` when the calendar back-fill failed.
- It holds no token and no server text. A start that never took the lock
  writes none.

**A mostly lost day alerts.** A run that ends `complete` with fewer than
half of its planned pairs having an `ok` (`capture::DEGRADED_BELOW`, 0.5)
exits `DEGRADED`, so `OnFailure=` fires; at half or more, the losses are
ordinary (a few delisted symbols answering 2601400, a few capped pairs) and
the start exits 0 as `complete_with_failures`. This is what catches a
server-side auth rejection inside the envelope: its code was never probed,
so it is stored as `permanent`, not `auth_unavailable`, every pair reaches
its cap, and the run ends `degraded` with `failure` naming the envelope
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
- **The kind.** It reads `last_exit.json`; a state that is non-zero and at
  most 15 minutes old names the failure kind. Anything else is `unknown`: a
  capture that refused its environment, failed before it took the lock, or
  was killed by `TimeoutStartSec=` wrote no state.

It runs `notify_command` with one more argument, the message, for example:

```
research-archive: research-archive-capture.service failed: auth_unavailable (exit 75) at 2026-10-13T21:30:02+00:00, trading day 2026-10-13
```

It sends only when the kind differs from the last one sent or six hours
have passed, and records what it sent in `<data_dir>/notify_state.json`
after the command succeeded. A capture whose run completes (`complete` or
`complete_with_failures`) deletes that file, so the first failure after a
recovery always alerts. The command runs without a shell, with stdin
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
- **`WorkingDirectory=` is the data dir.** The SDK's dotenv walks up from
  the working directory, so the job still refuses to start when a `.env`
  exists in the data dir or any parent.
- **No unit sets an SDK variable.** The region comes from the config, and the
  job refuses to start when any `LONGBRIDGE_*`/`LONGPORT_*` variable is set.
- **A proxy, only if this host needs one.** Uncomment `EnvironmentFile=` in
  both services and point it at an operator-owned file (mode 0600) holding
  only `HTTPS_PROXY=`, `HTTP_PROXY=` and `NO_PROXY=` lines.

`systemd-analyze verify` on rendered copies reports no problem.

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
| `open` | the refusals, and that a refused file is untouched |
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
| `net::dotenv`, `net::scan` | a `.env` in reach is refused and cannot override the pins; no SDK client is built outside the harness |
| `net::runner` | on a fake clock, under the harness: I1, I7, freeze before fetch, resumption, the carry deadline, missed and dangling days, the serialized recorder, a hanging request, the `degraded` threshold, the first auth failure stopping every worker |
| `net::calendar` | the calendar refresh's error mapping, the back-fill of a 40-day outage (every closed day `missed`), a failed back-fill that only warns and is retried, the fallback to a cache that still covers now |
| `net::schedule` | the retry schedule through an outage; membership scenarios A (a blip is retried and `observed`), B (retried every 2 minutes, then a freeze at the carry deadline, across starts or within one) and C (a start to the window end waits for the carry deadline, never `partial`); constituents outages of 22, 25, 40 and 55 minutes `observed`; a late start freezing from stored rows; a resume after the carry deadline not held up by a hanging constituents attempt; the auth latch before a slow auth row; no request starts after the window end however late its rate-limit slot |
| `net::interrupt` | I4 and I12 in process: a capture interrupted during membership leaves a whole plan and no freeze; the rerun completes with the planned keys and index set |
| `net::exit_state` | dangling runs on record (all of them) through a later failure; a dangling-only `partial` with its failure; a lock I/O error on record; a completed run clearing the alert dedupe |
| `capture_cli` | `capture` as a subprocess: a kill during the fetch (I4), I9, a hanging token refresh, I10, the calendar refresh. The membership phase cannot be reached in real time at every hour, so its kill and I12 run in `net::interrupt` |
| `capture_secrets` | I11 for `capture` and `notify`; the alert's dedupe and environment; `notify` alerting when `capture` refused its environment |
| `units` | the systemd templates' directives and placeholders |
