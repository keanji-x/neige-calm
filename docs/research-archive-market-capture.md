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
a versioned wrapper and only publish a complete window. Every page is paced and
checks cancellation/deadline. WS bars are an explicitly versioned typed
projection (not raw JSON), with decimal-string prices, NY trade dates, regular
sessions, and no adjustment. Reject invalid/nonpositive/inconsistent OHLC. The
SDK drops unknown protobuf fields; this limitation is documented rather than
claiming wire fidelity. No inferred prices, dates, or financial actuals.

## Upgrade and verification

Add a frozen v3 migration; retain v1/v2 SQL and the narrow v1→v2 annual command.
The new command explicitly upgrades v2→v3 under archive.lock. Ordinary opens
refuse older versions. Schema rebuilds preserve every existing column value;
new tables have update/delete/duplicate guards. New request definitions never
enter historical plans. Before first successful capture, read_at returns the
existing not_archived/no_value results without fabricating past observations.

Acceptance checks: exact legacy row/body/coverage equality across upgrade;
unfrozen old-run resume; immutable v3 requests across config/date changes;
calendar pagination/truncation/nonprogress/error; WS error codes and OHLC/date
validation; benchmark-only bar plans; plugin entry-point reads and manifest
contract. Mutation-check historical-plan isolation and pagination completeness.
Run package tests, text/contract gates, and quick Rust gates, then two independent
L2 reviews on the frozen candidate. No real Codex E2E on the shared host.

At 505 research securities and five additional benchmarks, 10N+B+I logical pairs
is about 5,056 plus calendar pages. At 2 starts/s the ideal first pass is about
42 minutes plus pagination, latency, and retries. Preserve 25-minute attempts
and the 30-minute timer; successful pairs are skipped on resumptions. Do not
claim one invocation completes the run.
