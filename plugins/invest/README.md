# Invest portfolio

A multi-instrument US paper portfolio on one dedicated Longbridge paper account (#2104, design
`docs/architecture/2104-invest.md`). One portfolio Track is the only broker writer: its Planner
saves target weights, an ordinary Worker task requests their execution, and the App's background
loop reconciles, sizes and submits official-SDK paper orders and publishes native Report data units.

Slice P1 is the ledger, decisions and executions over many symbols, the held limit, the portfolio
units and recipe; P2 adds covered instruments with one research Track each (issued keys, the lease),
theses and their board, the research recipe and the plugin's standing Planner instructions; P3 adds
`series_show`, the chart series tool. `plugins/paper-trading` and `plugins/market` keep running until
the cut-over (§3.8).

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
| `instrument_add` | portfolio Planner | Covers a symbol (`symbol`, `message`): `pending` until verified |
| `instrument_set` | portfolio Planner | Renews a live symbol's research key (`expected_version`, `message`); returns the new `track_add` |
| `instrument_rm` | portfolio Planner | Drops a symbol and retires its open theses; refused while held |
| `thesis_add` | portfolio Planner | Raises a thesis on a live symbol: `thesis_id`, `stance`, `title` ≤ 110, `summary` ≤ 500, `body` ≤ 6000, `source_refs`; at most 3 open per symbol |
| `thesis_rm` | portfolio Planner | Retires one thesis under `expected_version` |
| `instrument_status` | attested research Planner | Its symbol's state, key, position, target, theses and `portfolio_track_id`; refreshes the research units |
| `thesis_set` | attested research Planner | Assesses one open thesis of its own symbol: `assessment`, `summary`, `source_refs`, `expected_version` |
| `series_show` | the chart resolver only | Daily, weekly or monthly bars for a report's `chart.series` block |

The App reads the Track, its creator provenance and the caller from host metadata
(`dev.neige/track` = `{id, creator_track_id, creator_key}`, `dev.neige/caller`). Every portfolio tool
refuses any Track but `portfolio_track_id`; research Tracks never trade. Refusals start with the served tool
name (see Errors).

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
loop sees a broker quote for it, then `live`; without a quote, or after `instrument_rm`, it is
`dropped`. Each `opening_positions` symbol starts `pending`. Only `pending` and `live` count: a
counted instrument is **held** when the latest decision weights it above 0 or it has a position,
otherwise **watched**. `instrument_add` needs watched < `max_watched`.

`decision_add` requires every weighted symbol to be covered, a weight above 0 to be `live`, each
weight ≤ `max_weight_bps`, the sum ≤ 10000 − `cash_buffer_bps`, and the symbols weighted above 0
plus those still in a position ≤ `max_held`. A symbol left out, or at 0, is sold. Rotating a full
book takes two decisions: sell, then buy once the sells settle.

## Research Tracks

Going live issues the symbol's research key `invest-<VENUE>-<CODE>-<n>`, and `instrument_set` issues
the next one; `n` never decreases, so no key is reused. The ledger stores the exact `neige_track_add`
arguments under the current key (`track_add` in `portfolio_status`), which the portfolio Planner
passes verbatim, so a retry replays the same Track.

A research call is **attested** for symbol S exactly when the kernel's provenance names the
portfolio Track as its Track's creator, its creator key is S's current key, and S is live. Nothing is
stored at binding time. An older key of S, or a dropped S, is refused with JSON-RPC error -32409
(`superseded: close this Track`); every other caller with -32403, the portfolio Track included.
`thesis_set` writes only theses of the attested symbol.

Every attested call sets S's `last_seen_at`: access metadata that bumps no version, writes no journal
row and grants nothing. `portfolio_status` marks a live S **stale** when its current key was not seen
within 120 minutes of issue, or has not been seen for `lease_days`. The portfolio renews a stale S
with `instrument_set`; the superseded Track closes itself on its next call. Until it is closed, a
superseded Track's `instrument.position` and `thesis.records` keep the state of its last attested
call: the App refreshes them only for attested callers.

The two kinds of report link each other: the portfolio's 研究论点 section lists one
`neige://wave/<track_id>` link per live research Track, and each research report's 研究笔记 starts
with a link to the portfolio Track (`portfolio_track_id` in `instrument_status`).

## Errors

Every refusal is a JSON-RPC error whose message starts with the served name
(`plugin_invest_<tool>: …`), per `docs/conventions/agent-commands.md` §5: -32602 invalid argument,
-32403 wrong role, Track or provenance, -32404 unknown entity, -32409 state conflict (a stale
`expected_version`, a full limit, a superseded research key), -32601 unknown tool, -32603 ledger
failure. Only `series_show` answers a request-level failure as an `isError` result, because that is
the `market.series` contract the kernel's chart resolver validates.

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
  "instrument_recipe_id": "YOUR_SAVED_INSTRUMENT_RECIPE_ID",
  "max_held": 10,
  "max_watched": 20,
  "max_weight_bps": 3000,
  "opening_positions": "[{\"symbol\": \"US:SPY\", \"shares\": 13}]"
}
```

`instrument_recipe_id` is the stored research recipe (`instrument-recipe.md`) every research Track
is added from; `lease_days` (default 8, 1-90) is the research lease. `max_held + max_watched` is at
most 255, and `opening_positions` lists at most `max_held` symbols.
It must equal the broker's holdings at the first reconciliation, which pins it; a changed value is
refused on an existing ledger. The ledger is `ledger.sqlite3` in the plugin data directory, bound
to the account, the portfolio Track, the OAuth client and the broker HOME. Build the SDK
interpreter from `requirements-sdk.txt` and authorize it as the paper plugin's README describes,
running `invest/sdk_bridge.py … login`.

## Data units

`portfolio.{nav, nav_history, account, weights, weight_history, holdings, decision_log, fill_log}` and
`thesis.board`, placed by `portfolio-recipe.md` and republished every tick; `instrument.position` and
`thesis.records`, placed by `instrument-recipe.md` and set on the calling research Track by each
attested call. The board has one record per counted symbol, held ones first by market value: the top
99, then one 其他 record counting the rest's theses by assessment; each open thesis is a section
labeled `<assessment> · <title>`. A research Track's records list its open theses and the 20 latest
retired ones. Any number of held symbols fits the unit contracts by aggregation:
`weights` shows the top 10 + 其他 + 现金, `weight_history` the top 4 now + 其他 + 现金, and each
decision record its top 11 weights + 其他 and its top 19 legs by filled amount + 其他. 其他 is always
the exact sum of what it replaces. Amounts are apportioned in cents and shares of equity in 0.0001%
by largest remainder, so slices sum exactly to equity and every history point to 100%. Valuation
history keeps one sample per trading session, dated by the newest quote's New York date, so a
weekend or holiday read re-values the last session.
