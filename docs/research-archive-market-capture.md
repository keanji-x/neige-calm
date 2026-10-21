# Research archive market capture (#2988)

Review tier: L2, because this changes the released persistence and capture contracts.

Outcome: archive the US earnings calendar, earnings snapshots, reported financial
summaries, and dated regular-session daily bars, accessible through read_at.
No deployment, trading, outbound messaging, or production-store migration is part
of this PR. The owner approves merge and performs redeployment separately.

## Probe and limitations

The pinned SDK v5.2.0 successfully returned seven daily bars each for MSFT.US,
QQQ.US, SOXX.US, KBE.US, SPY.US, and SMH.US. The read-only probe held the archive
lock, required >1h token life, disabled refresh, and never saved credentials.
This proves sample access, not monthly universe capacity. Historical-bar symbol
quotas are account dependent; permission/quota failures remain visible failures,
without an alternate source or smaller silently substituted universe.

Raw HTTP samples returned earnings snapshot, financial reports, and calendar.
Financial reports are standardized summaries, not SEC filings: sampled MSFT
includes CapEx, but not separately identified finance-lease additions or segments;
sampled JPM includes BPS, not a verified TBVPS field. Missing fields are not zero
and these endpoints alone do not solve every research question.

## Plan and ownership

Store owns one authoritative planned-pair relation. New v3 runs persist each
resolved request in run_pair; plan creation and member freeze append pairs in the
same transaction as their corresponding keys. A v3 marker separates these plans
from legacy runs. Legacy runs retain the released run_key/run_endpoint join,
including members acquired by an old unfrozen run after upgrade. Dates derive
from the run's as_of and are frozen, never from invocation time.

Benchmark keys are tagged benchmark and only receive daily bars; market:US is a
market key with one logical calendar pair. Securities receive the existing seven
fundamental endpoints plus snapshot, reports, and bars. Reports are daily in this
first implementation: this covers weekly and post-earnings needs without a
second, calendar-dependent scheduling policy. A successful daily snapshot is an
observation, not a promise to catch later same-night upstream updates.

HTTP bodies retain exact data bytes. Calendar captures preserve raw pages inside
a versioned wrapper and only publish a completed bounded traversal. Every page is paced and
checks cancellation/deadline. WS bars are an explicitly versioned validated protobuf
projection (not raw JSON), with decimal-string prices, NY trade dates, regular
sessions, and no adjustment. Reject invalid/nonpositive/inconsistent OHLC. Unknown protobuf fields are not projected; this limitation is documented rather
than claiming wire fidelity. Decimal parsing is strict, without SDK zero fallbacks. No inferred prices, dates, or financial actuals.

## Upgrade and verification

Add a frozen v3 migration; retain v1/v2 SQL and the narrow v1→v2 annual command.
The new command explicitly upgrades v2→v3 under archive.lock. Ordinary opens
refuse older versions. Schema rebuilds preserve every existing column value;
new tables have update/delete/duplicate guards. New request definitions never
enter historical plans. Before first successful capture, read_at returns the
existing not_archived/no_value results without fabricating past observations.

Acceptance checks: exact legacy row/body/coverage equality across upgrade;
unfrozen old-run resume; immutable v3 requests across config/date changes;
calendar normalization/bounded progress/error; WS error codes and OHLC/date
validation; benchmark-only bar plans; plugin entry-point reads and manifest
contract. Mutation-check historical-plan isolation and pagination completeness.
Run package tests, text/contract gates, and quick Rust gates, then two independent
L2 reviews on the frozen candidate. No real Codex E2E on the shared host.

At 505 research securities and five additional benchmarks, 10N+B+I logical pairs
is about 5,056 plus calendar pages. At 2 starts/s the ideal first pass is about
42 minutes plus pagination, latency, and retries. Preserve 25-minute attempts
and the 30-minute timer; successful pairs are skipped on resumptions. Do not
claim one invocation completes the run.

## Independent review corrections

Both independent channels found that the pinned QuoteContext queues detached
commands: its cancellation and limiter fences apply before enqueue, not wire
writes. A delayed first response reproduced three post-cancellation sends and
burst starts. Production bars therefore use a narrowly scoped owned quote-v1
socket adapter with the pinned official protobuf messages. Existing SDK OAuth,
snapshot HttpClient, OTP parsing and connection-limit validation remain the
only bootstrap authority. Every actual handshake/auth/business/control write is
paced and fenced. A connection is taken out of its cache while awaited; errors,
cancellation and timeout close it, with no background queue/reconnect tasks.
This small framing implementation is an intentional source-adapter exception,
needed because the existing SDK boundary cannot enforce archive cancellation.

Review initially treated count as event cardinality. Further controlled probes
showed this is not the provider contract. A red-before/green-after regression
pins the corrected conservative offset walk across normalized duplicates, empty
raw positions and later companies. Failed HTTP pages remain failures; empty
well-formed raw positions are not fabricated events.

The corrected owned transport was also exercised read-only against MSFT.US and
all five benchmarks: each returned six closes ending on 2026-10-20. The probe
held archive.lock, disabled credential refresh/storage writes, and did not open
or migrate the production database. This validates the protocol path, not the
full universe's monthly quota.

## Calendar counts, normalization, and verified coverage

The initial 55-versus-23 comparison mixed different cardinalities. Controlled
read-only probes for upstream group 2026-10-14 established:

| query | reported count | returned positions | unique event ids | companies |
|---|---:|---:|---:|---:|
| report, count=1, offsets 0–55 | 55 | 46 nonempty | 36 | 22 |
| financial, count=1, offsets 0–45 | 46 | 46 | 46 | 22 |
| report, count=1000, offset=0 | 55 | 22 | 22 | 22 |

All three company sets match. ASML has separate qf/3q records for the same
quarter; other companies have semiannual/annual or other fiscal variants.
The report query also maps some raw positions onto identical latest-event IDs:
MS offsets 4/5 are identical, while financial-only exposes its qf/3q IDs.
Single-symbol ASML reports count3 for report versus count2 for financial, yet
both broad pages display one company event. These observations establish that
reported count is not published infos/unique-event/company cardinality. They do
not establish an exact private backend schema or a universal 55=46+9 mapping.

The calendar contract is therefore API-published **upstream group/company date**
coverage, not exhaustive hidden fiscal variants or independently verified NY
earnings dates. Preserve every observed event/raw page and source count; do not
invent missing events or fill their actuals from the separate report endpoints.
Group labels may disagree with ext.local_date and datetime: WSE's group label
2026-10-16 accompanies local_date2026-10-15 and a NY timestamp on the15th. All
these fields remain unchanged; clients must not reinterpret group keys as NY
event dates.

A short normalized page is not by itself a termination proof. The conservative
walker uses stable reported count as an upper offset bound, requesting at most
100 published rows per page and advancing by max(1, returned current-group infos
length). The stride cannot exceed represented raw positions when normalization
does not expand records, so grouping/caps produce overlap rather than gaps.
Continue through the bound even across short, identical, or empty pages. Strictly
increasing offset and count/page/byte/deadline caps establish progress; identical
pages are legitimate, while conflicting event versions or changed counts fail.
HTTP/parse/cancellation failures still publish no partial calendar value.

Output declares coverage=upstream_group_company_dates, retains reported_counts,
company_counts and raw pages with their exact request offsets/counts and fetch
times. Empty published groups are explicit. Calendar dates are source labels;
statement variants and actuals belong to the fundamental endpoints.

A real >100 boundary was checked on group2026-10-28: reported count181,
count1000 returned181 companies, conservative count100 took2 pages and returned
the identical set, and exhaustive count10 took19 pages with the same181 IDs and
companies. The one-event path timed out at offset57; it is not claimed as a
complete independent reference. The completed ten-event scan is that reference.
The corrected production source then captured its full2026-10-13 through
2026-11-19 window:199 pages,38 groups/days,2,246 company/date keys,5,534,482 raw
bytes (below8MiB),8,692,635 serialized wrapper bytes,139.0 seconds at2rps. Its
2026-10-14 and2026-10-28 company sets exactly matched the completed exhaustive
small-page references. This fits the25-minute attempt bound without checkpoints
or a production configuration change. The large wrapper is read through JSON
pointers under the unchanged40KiB tool answer cap.

## Pre-merge revision: configurable benchmarks and core health

The optional TOML `benchmarks` list sits beside `research` and `indexes`, accepts
security symbols (absent or empty means no extra benchmark symbols), and is frozen into the
run's existing benchmark origins. Store receives that list rather than owning
research choices. Resuming a run uses its stored list even if config changes.

Endpoint catalog metadata declares Core versus Supplemental; the classification
is frozen per pair along with its request, so later catalog policy cannot
reinterpret an old day. The released eight
endpoints remain Core; the four additions are Supplemental. One metadata entry
owns dispatch phase and status/report classification; adding an endpoint does
not require editing a second list. Endpoint request SQL literals stay unchanged.
After membership/freeze, every start settles Core work before dispatching any
Supplemental work, including when Core retries are waiting. Capped Core failures
are settled failures, not successful data.

The unreleased v3 migration adds an append-only Core completion checkpoint. It
records that Core work settled without ending the overall run: supplemental
capture can resume until its existing window/deadline. Store validates frozen
membership and Core row coverage before checkpointing. Health, exit codes and
OnFailure alerts judge Core counts/checkpoint; Report and capture_status show
both tiers, including auxiliary failures/untried pairs and the overall outcome.
Missing timer starts, inaccessible storage and genuine Core auth/data failures
retain their existing health semantics. Auxiliary HTTP, quota, auth and timeout
failures do not turn a completed healthy Core into failing. Legacy histories
retain their released run-end semantics and migration never backfills checkpoint
rows. Released v1/v2 migration bytes remain untouched.

Acceptance: red regressions for benchmark configuration/old-config loading, core-before-aux wire
order and auxiliary-only failure health; frozen benchmark list across resume;
core failure/degraded and carry warnings still visible; aux-only timeout, auth,
resume and dangling closure do not alert; report exposes both tiers. Mutation
verification pins priority and health isolation, followed by required gates and
two fresh independent L2 reviews. No production config/store edits or deployment.

Deployment compatibility correction: `benchmarks` is optional and defaults to
empty. Missing this new opt-in scope must not prevent an existing config from
starting or running explicit upgrades. All previously required fields remain
required; invalid explicit benchmarks remain errors. A production-entry test
loads the old config, captures Core and stock bars, and plans no extra ETFs.
