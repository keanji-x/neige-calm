# Barra-style US research plugin

A local, daily-running Neige app plus an ordinary Recipe. No kernel changes,
forge trust, external scheduler, trading credentials or always-running Agent.
This is an **openly specified price-factor subset, not MSCI Barra**.

## Install

Requires Linux, Python 3.11+ and outbound access to Yahoo Finance. In this
directory:

```sh
python3 -m venv .venv
.venv/bin/pip install -r requirements.txt
```

In Neige Settings, install this directory as a local plugin, then enable it.
Create a **My recipe** named `US Barra-style research` using `recipe.md` as its
body. Create a Track from that Recipe and tell its Planner to start the study.
It discovers and calls `barra.start` in the current Track. No `template_id`,
`templates[]` ownership, or trusted-forge configuration is needed. Installation
and Recipe creation are separate; this package does not register either itself.

`barra.start {}` uses 30 illustrative US stocks, SPY as the benchmark, 07:00 UTC
daily refresh and 126 evaluation sessions. This is a fixed convenience universe,
not an index replication or recommended portfolio. Set `symbols` (12-40),
`benchmark`, `update_hour_utc` (0-23) and `history_days` (60-252) to override.
Yahoo tickers are uppercase, without venue suffixes; use `BRK-B`, not `BRK.B`.
The caller is responsible for selecting US-listed instruments.

Tools:

| Tool | Effect |
| --- | --- |
| `barra.start` | Store configuration and queue initial/daily calculation |
| `barra.status` | Read configuration, phase, last success/error and risk summary |
| `barra.refresh` | Queue one recalculation; duplicate in-flight requests coalesce |
| `barra.stop` | Disable daily updates; keep existing results |
| `barra.series` | Read stored research time series through native chart.series |

The host supplies Track identity. None of these tools accepts a Track ID.
The report starts with three risk measures, then cumulative factor curves,
concise exposure leaders, predicted/realized volatility and a gross reference
equity curve. Healthy operational status is not part of the reader-facing page;
the overview only adds a notice for an update in progress, failure or pause.
Three compact native tables use overlays; three native charts read stored
results through `barra.series`, with no custom iframe bridge or kernel changes.
The shared chart renderer supplies value/time axes, displays unpriced metric
series as absolute values rather than price-return percentages, and keeps
technical details collapsed. These are generic presentation changes, not
financial logic in the host.
The charts inherit the host's read-side caching/freshness policy (they may lag a
newly published overview until refreshed by that policy); each chart exposes its
actual coverage. Opening a chart never downloads prices or starts a calculation.
Factor curves sum daily regression coefficients within the requested window
in percentage points, not compounded strategy returns. The volatility comparison
uses prior-information forecasts against trailing 21-session realized volatility;
the latter is not forward realized risk. Detailed raw tables remain published
for older reports but the new Recipe no longer displays them.

**Stop the study before archiving/deleting its Track.** There is no automatic
archive/delete hook in this version. Disabling the plugin stops all its studies.
Do not delete the installation while its process is running.

## Method (v0.1.0)

- Input: split/dividend-adjusted daily closes via `yfinance.download` with
  `auto_adjust=True`, no fallback source. Excludes the current New York calendar
  date even after the close. The default UTC refresh occurs on the following
  US calendar day. Dataset dates and freshness are displayed, not inferred from
  the wall-clock refresh time. Data more than seven calendar days behind fails.
- The benchmark's dates define the session grid. Every requested stock must
  have positive, finite closes on that grid; no forward-fill, zero-fill or silent
  exclusion. About 1,800 calendar days are requested, keeping at most
  `252 + history_days + 65` sessions. Newly listed or missing stocks fail visibly.
- Beta: OLS slope of 252 simple daily stock returns on benchmark returns, with
  an intercept. Residual volatility: OLS residual standard deviation, using
  250 residual degrees of freedom, annualized by `sqrt(252)`.
- Momentum at session t: `log(P[t-21] / P[t-252])` (skip 21 recent sessions).
- Each descriptor is clipped at its cross-sectional mean +/- 3 population
  standard deviations, then equal-weight standardized to mean 0 and variance 1.
  Degenerate descriptors or ill-conditioned factor regressions fail.
- Attribution: cross-sectional equal-weight OLS of return t on **exposures
  observed at t-1**, with an intercept plus the three standardized descriptors.
  `market` is this intercept, not SPY's return. The style coefficients are not
  returns from executable factor portfolios. Attribution R-squared is in-sample.
- Risk: sample factor covariance and per-stock specific return variance over
  up to 252 prior regressions, at least 60. Specific variance receives the simple
  N/(N-4) degrees-of-freedom correction; specific cross-covariances are omitted.
  Portfolio variance is `b' F b + sum(w_i^2 * specific_variance_i)`.
  The equal-weight reference portfolio is style-neutral by construction after
  equal-weight standardization, so its calibration does not validate all style
  covariance directions or an arbitrary concentrated portfolio.
- Validation: each day's variance forecast uses only prior regression outcomes
  and t-1 exposures. Report RMS return/forecast-volatility and the fraction
  within +/-1.96 sigma; these are diagnostics, not a pass/fail validity claim.
  A daily-rebalanced, equal-weight portfolio supplies gross cumulative return,
  realized volatility and drawdown. No costs, market impact or trade execution.
- Not implemented: official descriptors/weights, size/value/industry factors,
  fundamentals, covariance shrinkage, proprietary specific-risk adjustments,
  historical index membership, portfolio imports or point-in-time data vintages.
  A current fixed universe has selection and survivorship bias. Adjusted prices
  may be revised retrospectively. Backtest results are not investment advice.

Background reading (the formulas above are this implementation's definition,
not claims to reproduce the provider's methodology):

- MSCI equity factor models: https://www.msci.com/data-and-analytics/factor-investing/equity-factor-models
- yfinance API and data-use terms: https://github.com/ranaroussi/yfinance

Yahoo availability and terms apply; yfinance is not a data entitlement or an
official Yahoo API service. No paid data access or new key is provisioned.

## Runtime and storage

One plugin process, one background calculation at a time. MCP reads and control
requests remain responsive while the worker computes. The worker scans each
minute, runs queued work immediately, and tries each enabled study at most once
per UTC date after its configured hour. A failed attempt waits for manual
refresh or the next date, avoiding an automatic retry storm.

Configuration, recent successful result and current status are saved in
`NEIGE_PLUGIN_DATA_DIR/state.json`; latest input and result are under
`studies/<hash-of-track-id>/prices.csv` and `result.json`. File replacement is
atomic, but there is no multi-file transaction, archive, checkpoint, retry queue
or power-loss durability guarantee. `result.json` is the latest computed result;
state's last success is advanced only after all overlays are acknowledged.
The result includes model version, configuration, data hash and cutoff date.

After restart, enabled configurations remain enabled. In-flight work is marked
interrupted and not resumed; missed dates are not backfilled. A due study may
compute one fresh result. Stop is cooperative: an in-progress fetch/calculation
can finish, but a stop observed before publication discards it. Publication
already begun may finish. Configuration changes during calculation/publication
are refused, so old computations cannot replace a newly configured study.

The tables are separate overlay writes, not an atomic page transaction. Compact
tables show cutoff/short run markers; the overview identifies CSV replay versus
live-source provenance. `barra.status` succeeded means all tables were accepted.
Publication failure may leave mixed table versions, explicitly shown as failed
until a refresh succeeds. Calculation failures leave old successful tables intact.

This is a trusted native app, not an OS sandbox. There are no extra CPU/memory
hard limits, background daemon independent of Neige, or arbitrary-code execution
tools in this plugin. NumPy/BLAS thread configuration is an operator concern.

## Replay and local verification

An explicit offline source is useful for reproducibility and tests. Set plugin
Settings `prices_csv` to an absolute CSV path, then reload the plugin. Columns
are `date`, each configured ticker, then the benchmark, with already-adjusted
closes. The cutoff/freshness checks still apply. Empty `prices_csv` uses Yahoo;
CSV data is never an automatic fallback or relabeled as live market data.

Run the same production calculation without Neige, from this directory:

```sh
.venv/bin/python -m barra --output /tmp/barra-us-research
.venv/bin/python -m barra --prices-csv /path/to/prices.csv --output /tmp/barra-replay
.venv/bin/pip install pytest
.venv/bin/python -m pytest tests -q
```

The command produces `prices.csv`, `result.json` and native report payloads in
`tables.json`. For an exact older snapshot, call the pure `calculate` function
on the recorded ordered adjusted-close frame; the online/CSV loader's freshness
guard deliberately refuses stale daily refreshes.

Tests cover independent OLS comparisons, lagged exposures, no future-price
leakage into historical risk predictions, missing data, daily scheduling, stop,
failed publication and Track separation. Process tests run the real stdio entry
point with an explicit synthetic CSV and a callback host, not a real Agent.

Optional full-host smoke test (requires an existing compatible server/frontend
build and a recent real-price CSV from the command above):

```sh
.venv/bin/python tests/smoke_host.py --server /path/to/calm-server \
  --frontend /path/to/fe-dist --prices /tmp/barra-us-research/prices.csv \
  --output /tmp/barra-host-smoke --hold
node tests/browser_smoke.cjs /path/to/repo/fe /tmp/barra-host-smoke/metadata.json /tmp/barra-screenshots
```

This uses a fresh loopback server, isolated directories and `/bin/false` for
both real Agent binaries. It installs/enables the actual manifest, saves and
instantiates the actual Recipe, and verifies callback-published results. Only
the initial study configuration is seeded through `Runtime.start` (there is no
Agent in the test); the stdio tests separately cover the start tool. The browser
test uses the real report renderer at desktop/mobile widths and checks overlay
updates without reloading. Stop the held smoke process after browser checks.
