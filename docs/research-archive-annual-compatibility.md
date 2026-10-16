# Annual archive compatibility (#2659)

L2: this change adds a released persistence version and an explicit migration.
Formal independent reviews and semantic acceptance belong to the Planner.

## Decision and scope

Retain `upgrade-annual-catalog` permanently as a narrow, explicit v1-to-v2
operator command for offline copies and remaining v1 installations. It takes the
capture lock, requires the exact released v1 schema/catalog, never creates a
missing archive, and needs no credentials or network. Ordinary capture/plugin
opens never migrate. Future catalog changes need a new explicit policy; this is
not a drift-repair interface.

The original `schema::DDL` bytes are unchanged (SHA256
`9ae5109e876689be664a50c1c2d1a68e35633e3a06e22107700d39bd9b2611c4`).
`MIGRATION_V2` adds `run_endpoint(run_id, endpoint_id)`, its composite primary
key, foreign keys, STRICT/WITHOUT ROWID shape and all three append-only guards;
backfills every old run from the old catalog; then sets `user_version=2`.
The annual endpoint is appended only after backfill, within the same transaction.
The catalog row is GET `/v1/quote/financial-consensus-detail`, ordered query
`[["symbol",null],["period_type","af"]]`, headers
`[["Accept-Language","en"]]`, NULL `dc_restrict`, security kind.

The Store owns the persistence rule: `plan_run` snapshots endpoints atomically;
`PLANNED_PAIRS` is shared by coverage, write eligibility, completeness and
`ever_planned`. Capture schedules those pairs, and the plugin uses ReadOnlyStore
and the same point-in-time query. There is no new policy in the kernel/adapters.
The released v1 seven-endpoint contract is frozen independently in the test-only
`annual_catalog::V1_ENDPOINTS`, including method, path, ordered query, headers,
data-center restriction and key kind. It does not derive from `compiled()` or
inherit constructor defaults. Future catalog additions must explicitly assess
upgrade policy; they must not regenerate this historical input. Production
upgrade acceptance is unchanged. This fixture is intentionally a released
protocol, not an extensible list maintained in a generic layer. Existing test
counts now derive from the catalog where appropriate. Ordinary additions change the catalog, its contract
golden and manifest; no new CI list or extra permanent check is added. New tests
run once in the package replay; broader workspace coverage remains CI-owned.

## Independent binary proof (2026-10-16)

All data below is fabricated under `/tmp/research-archive-2659`; no live archive,
configuration, token, deployment, systemd or installed plugin was touched.
Only the supplied patch, deployment manifest and stage release binaries/manifest
were used from the read-only materials. No stage backup/config/auth file was read.

The supplied `archive-annual.patch` SHA256 was verified before use:
`a000efb5b45769a8d9acff374dda4f112f0c5f33e32148b8858ed5b689295b97`.
Stage artifact hashes matched `DEPLOYMENT.json`:

| Artifact | SHA256 |
|---|---|
| capture | `f2f3ca4b7dacc8159d38dacb4b3b724e5349c73b58057238d0b6b70577d74a5c` |
| plugin | `6f27a234ff5066672e459224227841aa9a31ba0487a09418cefc58379e00c527` |
| manifest | `182083b0040ada9e7c8c3c9d235e08dc2fc652a250ee75bebb3f7ada5b1756e2` |

Local evidence driver: `/tmp/research-archive-2659/compat.py`. These `/tmp`
materials are temporary host evidence, not permanent reproducibility assets or
an immutable audit archive. In particular, the compatibility driver is a mutable
temporary helper; its current contents alone do not establish past execution.
The original invocation was from the checkout:
`python3 /tmp/research-archive-2659/compat.py seed` on the pre-change build, then
`python3 /tmp/research-archive-2659/compat.py verify` on the PR build. `seed`
requires a fresh synthetic directory. Binaries were built by the focused nextest
commands below (which build the production CLI/plugin executables).

1. Execute the unchanged v1 DDL in Python SQLite, insert all seven legacy catalog
   rows with complete canonical fields, one run/config key and quarterly attempts:
   precise decimal body at +100 ms, empty object at +150 ms, failure at +200 ms,
   later value at +250 ms. Read through the original plugin binary at offsets
   99, 100, 149, 150, 199, 200 and 250 ms. Save complete JSON responses.
2. Run the verified stage capture binary's `upgrade-annual-catalog --config
   /tmp/research-archive-2659/synthetic.toml` with an empty environment and `/tmp`
   cwd. Output: `annual catalog ready`, exit 0. Stage plugin responses equal all
   seven original responses. Snapshot every `sqlite_schema` object, every column
   of `endpoint`, user_version (2), and legacy run endpoint count (7).
3. Original repository capture ordinary open fails with `archive user_version is
   2, this build reads 1` (exit 70); original plugin returns the same refusal.
4. PR capture ordinary open on that independently upgraded file passes schema and
   catalog checks and reaches expected `auth_unavailable` (75): no token supplied.
   PR plugin ordinary open answers all seven quarterly reads identically,
   including `as_of`, `fetched_at`, value/empty/failure visibility and no front-run.
   `quarterly-before.json`, `quarterly-stage-v2.json` and `quarterly-pr-v2.json`
   preserve the complete comparison. Store tests additionally pin raw body bytes.
5. A fresh PR archive's entire schema object list and all endpoint columns equal
   the stage-produced v2 snapshot exactly, including all guard SQL text and
   user_version. PR manifest is byte-identical to the stage manifest.
6. Seed only a synthetic fresh calendar (yesterday/tomorrow trading days) and run
   PR `capture` normally against the stage-upgraded file. It plans run 2 before
   stopping for the absent token: its eight saved endpoints include
   `consensus_annual`; run 1 still has seven, without annual. Separately,
   `capture_cli::annual_capture_cli_plans_and_fetches_both_consensus_views` uses a
   local fake server and fabricated token to complete an actual CLI capture,
   verifying annual/quarterly request selection, saved coverage and distinct bodies.

These establish functional compatibility of the deployed persistence/request/read
contracts. Byte-identical executables are not claimed: stage is a release build
from the supplied source base, PR evidence uses current-tree debug executables.
Production source changes match the supplied patch; differences are repository
integration, additional tests and documentation. No upstream live request is part
of this proof. Logs: `compat-baseline.log`, `compat-pr.log`, `compat-final.log`,
`stage-schema.json`, `stage-catalog.json`, `pr-binaries.sha256` in the evidence dir.

## Focused red/green and mutation evidence

Every Cargo command below used prefix
`env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6` from the checkout root.
The gate owns ratchet/contract/quick and package replay; they are not duplicated
as worker commands. `cargo fmt -p research-archive` and `git diff --check` ran.

- `cargo nextest run --locked -p research-archive annual_reproduction
  --no-tests=fail --test-threads 8`: both tests red before production changes:
  `annual_endpoint_is_available`, `annual_new_archive_uses_v2` (found 1, expected
  2). Log: `reproduction-red.log`. These two temporary reproduction tests were
  removed in the 2026-10-16 review cleanup: the existing catalog contract tests
  and `open::new_file_gets_the_catalog_and_reopens_untouched` retain their
  assertions without duplicate permanent CI executions.
- `cargo nextest run --locked -p research-archive -E 'test(annual) |
  test(upgrade) | test(guards) | test(plugin_read_at) | test(reads::)'
  --no-tests=fail --test-threads 8`: 70/70 green, `focused-green.log`.
- `cargo nextest run --locked -p research-archive -E 'test(annual) |
  test(upgrade)' --no-tests=fail --test-threads 8`: 12/12 green, including new
  actual CLI capture, `annual-cli-green.log`.

Mutation driver: `/tmp/research-archive-2659/mutations.py`; run with `python3`
from the checkout, exclusively, without concurrent writers/reviewers. Each case
writes its predicted complete red set before editing one production expression,
confirms the edit, runs the entire observable filter with `--no-fail-fast`,
restores exact source bytes in `finally`, and reruns that filter green. Command:
`cargo nextest run --locked -p research-archive --test suite -E '<filter>'
--no-tests=fail --no-fail-fast --test-threads 8`, with the prefix above.

| Mutation | Whole filter | Complete predicted = actual red set | Restored green |
|---|---|---|---|
| Replace snapshot join in `PLANNED_PAIRS` with current catalog join | `test(annual_catalog)` | `annual_catalog::explicit_upgrade_only_appends_and_preserves_historical_bytes`; `annual_catalog::upgraded_open_legacy_run_keeps_index_members_and_can_complete_without_annual` | 7/7 |
| Bypass `fetched_at <= t` only for annual rows | `test(plugin_read_at) \| test(reads::)` | `plugin_read_at::annual_and_quarterly_bodies_are_independent_point_in_time_values` | 17/17 |
| Disable v2 duplicate guard predicate with `WHEN 0 AND EXISTS` | `test(guards)` | `guards::i2_run_endpoint_insert_or_replace`; `guards::i2_run_endpoint_on_conflict_do_update` | 43/43 |

Evidence: `mutation-results.json`, `mutations.log`, and per-case
`mutation-{history,future,guards}-{prediction.json,red.log,green.log}` plus diff
files. All mutation exits were 100; all restored exits 0 and original/restored
source hashes equal. Initial fail-fast red runs were discarded and all three
cases rerun with `--no-fail-fast`; only the complete reruns count.

Additional focused nextest command:
`cargo nextest run --locked -p research-archive -E 'test(source::every_catalog_entry)
| test(runner::a_full_run) | test(runner::the_attempt_timeout) |
test(schedule::a_throttle_wait)' --no-tests=fail --test-threads 8`.
All four refused before running because the checkout ancestor has `.env` in
reach (`source-resumption-green.log` records the refusal, not a pass). No `.env`
was read or changed. Safely reran the exact built `target/debug/deps/net-*`
executable from `/tmp/research-archive-2659`, once per named test with
`--exact <name> --nocapture`; all four passed. Exact argv/cwd/output are in
`source-resumption-tmp-green.log`. The hermetic harness still pins proxies and
SDK variables. The full kernel replay uses the safe cwd arrangement below;
the dotenv fence remains unchanged.

## Historical full-package verification (2026-10-16)

Candidate `2797ca8e51338fe23156b7c17bed104a02aa65db` passed all six kernel gate
steps in `annual-v2-safe-gate-r3`, run 1: ratchet, contract, rust-quick,
archive-safe-metadata, archive-package-replay and server-tool-names. The archive
replay at that candidate ran **312 tests: 312 passed, 0 skipped** (a historical
result, not the test count or verification result for later heads); the tool-name
filter ran
**10 tests: 10 passed, 1588 skipped** (the latter are outside the selected
tool-name scope). Full output is in
`/home/kenji/.local/share/neige-next/data/gate-logs/1501af2fba10480089c64e6c059b2353:annual-v2-safe-gate-r3-r1.log`.

The gate extracts `git archive HEAD` into a fresh `/tmp` source snapshot, then
uses nextest 0.9.140's official reuse-build interface. With the Cargo prefix
above, it builds metadata using `cargo nextest list --locked -p research-archive
--list-type binaries-only --message-format json` and dependency metadata using
`cargo metadata --locked --format-version 1`. Replay uses
`cargo nextest run -E 'package(research-archive)' --binaries-metadata
<snapshot>/binaries.json --cargo-metadata <snapshot>/cargo-metadata.json
--workspace-remap <snapshot> --no-tests=fail --no-fail-fast --test-threads 8`.
The replay does not accept build-selection arguments `--locked` or `-p`.
`RUSTC_WRAPPER` is explicitly cleared for both metadata commands as well.

Run 1's snapshot is `/tmp/neige-2659-gate.VnHAWL`; its `binaries.json` retains
the original checkout's `target` directory and absolute test-binary paths.
There is no `--target-dir-remap`: workspace remapping changes the test cwd to
the snapshot's `plugins/research-archive`, while reusing the original build
artifacts. The earlier minimal SDK probe recorded that cwd with `strace` in
`/tmp/research-archive-2659/safe-remap-corrected-cwd.strace`; the full replay
also passed the SDK tests and the dotenv-refusal regression with protections
intact. No ancestor `.env` or actual token was read, removed or edited.

Before this gate, all 39 production source files matched the r2 frozen probe
snapshot; the three mutation source hashes matched `mutation-results.json`.
The prior compatibility/mutation logs and drivers still existed, and the three
saved quarterly response files were equal. No compatibility or mutation rerun
was needed. Snapshot, metadata and `/tmp/research-archive-2659` paths are
temporary audit evidence; retain them externally if long-term access is needed.
No production source or SDK-test protection was changed. Formal L2 review
remains the Planner's responsibility.

## Review cleanup evidence (2026-10-16)

L2 remains in force for this persistence work; the Planner arranges both review
channels. This cleanup changes tests and documentation only. Compared byte for
byte with reviewed baseline `060e08aafd2537990d4daf7f42e43dde4e7c0908`, 38 of
39 production Rust source files are identical. The sole exception, `src/lock.rs`,
has comment changes only; removing comment lines makes its bytes identical too.
The manifest is byte-identical. This includes unchanged catalog, released DDL,
`MIGRATION_V2`, upgrade acceptance, request and read implementations. Therefore
the original stage binary compatibility proof remains applicable.

The three mutation source SHA256 values still equal both `original_sha256` and
`restored_sha256` in `mutation-results.json`:

| Source | SHA256 |
|---|---|
| `src/store/write.rs` | `48935079d1dbd4dbb450e0495a96af272522e657721a70295f2ab2cb94912242` |
| `src/store/read.rs` | `741d8897a40a900f6bf36d10fff35200a1269a0397a5c44d3f0cc0016150969f` |
| `src/store/schema.rs` | `0fdb6bd3f91bc7be667f1d843282c91bb7686666bba189ced3fa26a4d2a56fb3` |

The prior complete mutation results were independently checked by both review
channels. No production mutation rerun is needed for this cleanup. As disclosed
above, discarded fail-fast initial runs preceded the complete no-fail-fast runs.
The frozen v1 fixture adds no test execution or evolving maintenance list; it
lives in the existing upgrade test module and must remain historical when new
endpoints ship. Readable endpoint error expectations now derive from the
catalog's security endpoints, including annual.

Focused verification uses a fresh `/tmp` source snapshot with the changed files
overlaid, builds nextest binary/dependency metadata from the checkout, then uses
the same official reuse-build/workspace-remap method described above. Filter:
`package(research-archive) & (test(annual_catalog) | test(catalog::) |
test(open::) | test(plugin_read_at))`, with `--no-tests=fail --no-fail-fast
--test-threads 8`. Result: **31 passed, 279 skipped** (outside the filter), log
`/tmp/research-archive-2659/cleanup-focused.log`. `cargo fmt -p research-archive`
and `git diff --check` also passed. The kernel owns the new full gate replay;
the historical 312 result above does not stand in for that gate.

## Capacity and configuration decision

With approximately 505 distinct securities after config/member deduplication,
one new endpoint adds 505 planned pairs per trading day. This number is an
assumption, not inferred from a production config: the example config contains
three research symbols and `.SPX.US`, whose membership size/overlap changes.
One index adds one constituents pair. At 2 starts/s, the rate-only estimates are:

- old: `(505 × 6 + 1) / 2 = 1515.5 s` ≈ 25 min 16 s;
- v2: `(505 × 7 + 1) / 2 = 1768 s` ≈ 29 min 28 s;
- incremental: `505 / 2 = 252.5 s` ≈ 4 min 13 s (16.7% security work).

These are rounded request-budget estimates (the first slot is immediate), not
completion guarantees. Four concurrent workers overlap latency; above roughly
2 s mean service time, concurrency can dominate 2 rps. Throttles, retries,
calendar/auth work and SQLite I/O add time. Bare HTTP 429 SDK retries bypass the
limiter; envelope throttles are scheduled. Six counted attempts can span 149 min
of backoff; throttles do not consume that cap.

Keep the example's 25-minute attempt timeout and 30-minute timer: exceeding a
start budget is intentionally resumable, and the next start surveys stored rows,
skips successful pairs and respects stored retry timing. In the ideal model,
about 268 seconds of request budget remain after the first start and finish on
the next tick (~34.5 min elapsed from an aligned first start, plus overhead).
Membership may consume up to the 60-minute carry deadline across starts, and a
late start near window end may still lose coverage. The normal 17:30–09:30 window
is 16 hours (shortest DST case 15), leaving multiple resumptions.

No configuration change is required for correctness or ordinary window capacity,
and no production configuration was changed. If an operator requires a single
invocation to finish, they should budget above 30 minutes with measured margins
and coordinate service timeout/staleness settings; simply raising rps is
unsupported by observed throttling. That operational preference is separate from
compatibility and is not needed for this patch.
