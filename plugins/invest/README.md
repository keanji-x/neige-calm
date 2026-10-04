# Invest portfolio

A multi-instrument US paper portfolio on one dedicated Longbridge paper account (#2104, design
`docs/architecture/2104-invest.md`). One portfolio Track is the only broker writer: its Planner
saves target weights, an ordinary Worker task requests their execution, and the App's background
loop reconciles, sizes and submits official-SDK paper orders and publishes native Report data units.

Slice P1 is the ledger, decisions and executions over many symbols, the held limit, the portfolio
units and recipe; slice P3 adds `series_show`, the chart series tool. Research Tracks, theses,
issued keys and the lease (P2) build on them. `plugins/paper-trading` and `plugins/market` keep
running until the cut-over (§3.8).

## Verification

```sh
python3 -m pip install 'pytest>=8,<10' 'jsonschema>=4.18,<5'
python3 -m pytest plugins/invest/tests -q
```

Tests run the production App, its stdio entry point and its SDK subprocess runner against a
fixture executable that only returns prescribed broker records, plus an in-process simulated account
for wide histories. No credentials, network or broker orders are used.

## Tools

| Tool | Caller | Effect |
|---|---|---|
| `portfolio_status` | portfolio Planner or Worker | Persisted snapshot, positions, targets, instruments, limits, decisions with their legs, fills, errors |
| `decision_add` | portfolio Planner | An immutable decision: `weights: [{symbol, bps}]`, `message`, `source_refs`, `valid_until` (≤ 24 h) |
| `execution_add` | portfolio Worker | Marks the queued decision `requested` and wakes the loop; never contacts the broker |
| `series_show` | the chart resolver only | Daily, weekly or monthly bars for a report's `chart.series` block |

The App reads the Track and the caller from host metadata (`dev.neige/track`, `dev.neige/caller`).
Every portfolio tool refuses any Track but `portfolio_track_id`. Refusals start with the tool name.

## Chart series

`series_show` answers a report's `chart.series` block, for example
`{"source": "neige://plugin/invest/series_show", "series": ["US:SPY"], "range": "1Y"}`, on any
Track. It follows the `market.series` contract for US symbols: all seven keys are required, a
request past `deadline_ms` is refused before any SDK call, and each asset gets its own `ok`,
`unknown_asset` (not `US:<CODE>`) or `unavailable` entry. The bars are the official SDK's
forward-adjusted, regular-session daily candlesticks, read through the quote context alone
(`sdk_bridge.py … series`) on a thread of their own, so a slow chart never delays a portfolio tool.
US is never relaxed: a bar or period is included only when the source already lists a later daily
bar. A candlestick is dated by the New York date of its timestamp, and each window request spans at
most 1000 calendar days; a request answering 1000 rows or more is refused as possibly truncated.

Only the kernel's background resolver may call it: it accepts a call that carries the Track and no
`dev.neige/caller`, and refuses every agent call with JSON-RPC error -32403 before contacting the
SDK. It declares `openWorldHint: true`, which agents running under `approval_policy: never` cannot
approve anyway.

## Instruments and limits

A symbol is `VENUE:CODE`, US only, canonicalized to upper case. An instrument is `pending` until the
loop sees a broker quote for it, then `live`; without a quote it is `dropped`. Each
`opening_positions` symbol starts `pending`. A counted instrument is **held** when the latest
decision weights it above 0 or it has a position, otherwise **watched**.

`decision_add` requires every weighted symbol to be covered, a weight above 0 to be `live`, each
weight ≤ `max_weight_bps`, the sum ≤ 10000 − `cash_buffer_bps`, and the symbols weighted above 0
plus those still in a position ≤ `max_held`. A symbol left out, or at 0, is sold. Rotating a full
book takes two decisions: sell, then buy once the sells settle.

## Execution invariants

Carried over from the paper plugin, per symbol: one unresolved decision at a time; one order per
symbol and decision, each ≤ `max_order_bps` of account value with a 1% price reserve; intent
committed before the broker write; an uncertain submission is never resubmitted and is recovered
only by its exact remark; unowned active orders and holdings that the owned executions do not
explain block execution and roll back the observation. Active orders are read account-wide every
snapshot and before every submit: all of today's orders (any non-terminal status, `Unknown`
included, blocks), and US orders from earlier days (GTC/GTD) placed within the last 400 days. SDK
5.2.0 documents neither a server default nor a range limit for `history_orders`' `start_at`
(`openapi.pyi:7555,7566`), so the window is explicit. **Residual:** an active order placed before the
window, or one from an earlier day that the server reports as `Unknown` (the earlier-days query keeps
the server's active-status filter), is invisible until it fills; its fill then breaks the per-symbol holdings equality, and
reconciliation refuses execution (fail closed on fill). Decision and order amounts are summed over
every persisted fill; only the fill table shows the latest 500.

New: at most one leg in flight; sells before buys; buys use settled cash only, so a buy waits for
settled proceeds and ends without an order if still unfunded at `valid_until`. Each leg carries
the remark `nc-inv-` + 32 hex digits of a digest of (account, decision, symbol), and the full digest
as its client request ID. A decision ends `done` (some leg was sent), `noop` (no leg needed) or
`expired` (its validity ended before any leg was sent); a `done` decision that filled nothing is
shown as 已结束 · 未成交, not as a success.

## Configuration

`opening_positions` is a JSON string, because the kernel's config schema holds scalars only:

```json
{
  "account_no": "YOUR_VERIFIED_PAPER_ACCOUNT",
  "broker_home": "/private/path/sdk-home",
  "oauth_client_id": "YOUR_REGISTERED_OAUTH_CLIENT",
  "sdk_python_path": "/private/path/sdk-venv/bin/python",
  "access_region": "cn",
  "portfolio_track_id": "YOUR_PORTFOLIO_TRACK_ID",
  "max_held": 10,
  "max_watched": 20,
  "max_weight_bps": 3000,
  "opening_positions": "[{\"symbol\": \"US:SPY\", \"shares\": 13}]"
}
```

`max_held + max_watched` is at most 255, and `opening_positions` lists at most `max_held` symbols.
It must equal the broker's holdings at the first reconciliation, which pins it; a changed value is
refused on an existing ledger. The ledger is `ledger.sqlite3` in the plugin data directory, bound
to the account, the portfolio Track, the OAuth client and the broker HOME. Build the SDK
interpreter from `requirements-sdk.txt` and authorize it as the paper plugin's README describes,
running `invest/sdk_bridge.py … login`.

## Data units

`portfolio.{nav, nav_history, account, weights, weight_history, holdings, decision_log, fill_log}`,
placed by `portfolio-recipe.md`. Any number of held symbols fits the unit contracts by aggregation:
`weights` shows the top 10 + 其他 + 现金, `weight_history` the top 4 now + 其他 + 现金, and each
decision record its top 11 weights + 其他 and its top 19 legs by filled amount + 其他. 其他 is always
the exact sum of what it replaces. Amounts are apportioned in cents and shares of equity in 0.0001%
by largest remainder, so slices sum exactly to equity and every history point to 100%. Valuation
history keeps one sample per trading session, dated by the newest quote's New York date, so a
weekend or holiday read re-values the last session.
