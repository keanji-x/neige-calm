# Longbridge paper portfolio

One investment Area, one long-lived strategy Track, one dedicated paper account.
Weekly reports are versioned inputs, not new accounts or new execution Tracks.
The plugin records decisions, reconciles broker orders and executions, maintains
a durable trading journal and renders native Report tables. AI research and
review run in the existing Neige agent, not inside another model client.

**This is supervised paper execution, not unattended trading or a shadow
portfolio.** The installed Longbridge CLI has a two-step native preview and user
confirmation contract. No MCP tool submits/cancels orders or reveals confirmation
codes. The separate interactive operator command preserves that contract.

## Initial scope

- A verified AP-region `lb_papertrading` account; the configured account number
  must match on every reconciliation and immediately before operator actions.
- US-listed stocks/ETFs, USD, whole shares, long only, regular-session DAY limit
  orders, prices >= USD 1 with cent increments. No margin, shorts or derivatives.
- One initial entry per trade, optional partial exits, no overlapping open trades
  in the same symbol. One active entry per source prediction; renewed research
  is needed for a new thesis rather than silently replaying the previous entry.
- Polling broker reconciliation and stop/target **alerts**. Alerts are NOT
  protective orders; a human must review and confirm an exit. No guarantee of
  maximum loss at the recorded stop. This plugin must not be mistaken for an
  unattended risk-management service.
- Gross realized P/L from broker fills and weighted entry cost. Fees and net P/L
  are deliberately unavailable. Account equity includes activity outside the
  strategy's tracked fills; it is not presented as attributed strategy return.

## Install and bind

Runtime: Linux, Python 3.11+ at `/usr/bin/python3`, the Longbridge CLI, and an
operator-created paper login. The plugin has no third-party Python dependencies.
Protect installed code, configuration, ledger and broker HOME from unintended
writers; local plugins share the service OS identity, not an OS sandbox.

1. Create an investment Area. Save `recipe.md` as a user Recipe, and create one
   strategy Track from it. Record the actual Track ID. No core migration needed.
2. Install this directory via Settings -> Plugins -> Server directory. Leave it
   disabled until fully configured. Installing files alone does not enable it.
3. Configure all required fields. Example values below are illustrative limits,
   not defaults, investment advice or an authorization to place orders:

```json
{
  "account_no": "YOUR_VERIFIED_PAPER_ACCOUNT",
  "owner_track_id": "YOUR_STRATEGY_TRACK_ID",
  "broker_home": "/home/operator/paper-home",
  "research_root": "/home/operator/financial_agent",
  "symbols_json": "[\"SOXX.US\",\"GLD.US\"]",
  "max_order_usd": "500",
  "max_portfolio_usd": "1000",
  "max_trade_risk_usd": "20",
  "cli_path": "/usr/local/bin/longbridge",
  "poll_seconds": 60,
  "quote_max_age_seconds": 180,
  "max_price_deviation_bps": 100
}
```

`symbols_json` is a JSON-encoded array because the host config schema supports
scalar fields only. The plugin strictly validates the parsed list and numeric
limits. Monetary amounts are decimal strings. Use a dedicated broker HOME and
do not switch its login or modify Longbridge authentication during a session.
The child environment inherits only PATH/LANG and an explicit proxy allowlist;
HOME is pinned. Longbridge endpoint overrides and unrelated credentials are not
inherited. A broker failure is never replaced with synthetic prices or fills.

4. Enable the plugin, then start a new agent conversation to discover its tools.
   Confirm a successful account snapshot in the Report before recording orders.
5. Retain the exact same operator-managed configuration JSON for the interactive
   command below. The plugin data directory is
   `<plugins-data-dir>/dev-neige-paper-trading`; do not use a second ledger for
   the same account. This first version assumes exclusive use of that account.

The account/Track ledger binding cannot be changed. Rebinding requires a new
portfolio after all old orders and holdings are resolved, not deleting its data.
Do not point this at a shared account or a pre-existing untracked position:
external positions and unknown active orders block trading.

## Run the first real paper cycle

Ask the Track agent to ingest the requested week, analyze the report alongside
current market data and account status, and record an explicit decision. The
recipe gives the agent this sequence:

1. `paper.ingest {"week":"2026-09-21"}` snapshots
   `weekly/2026/09_21/weekly_market_analysis_cn.md` and that week's rows from
   `weekly/2026/predictions.jsonl`. It returns a content-addressed `source_id`.
   Reading does not modify the source repository or prediction scores.
2. `paper.refresh {}` queues reconciliation; `paper.status {}` reports the
   outcome and snapshot time. The ordinary read-only Longbridge connector can
   supply additional market research without receiving order authority.
3. `paper.decide` records a unique decision ID, cycle ID, source/prediction IDs,
   trade ID, symbol, buy/sell/hold action, rationale and a timezone-aware
   `valid_until` no more than 24 hours ahead. Buy requires quantity, limit,
   stop and target; sell requires quantity and limit; hold contains no order
   fields. Quantities are JSON integers; prices are decimal strings.
4. Wait for `ready`. This means preflight succeeded, **not** that an order has
   been sent. Short or watch-level research cannot silently become an entry.
   A stale price or an expired research horizon prevents readiness. Cash,
   outstanding reservations, cost exposure and initial price risk are checked.
5. The human operator runs the following from this plugin's release directory:

```sh
python3 -m paper_trading.operator \
  --config /private/path/paper-config.json \
  --data-dir /private/path/plugins-data/dev-neige-paper-trading \
  --decision YOUR_DECISION_ID
```

The command revalidates the account and portfolio, prints the **native** broker
preview, then waits for the user to type its confirmation code. Blank input
does nothing. Piped/non-interactive confirmation is refused. Agents must not run
this command, read the code or redeem it on the user's behalf. The request is
revalidated again before the exact previewed order is submitted. The code is
never written to the plugin ledger or exposed through tools/Report overlays.

6. The plugin polls orders and executions. A broker order ID is not a fill;
   partial executions are accounted separately and deduplicated by execution ID.
7. For exit, the agent records a sell decision against the existing `trade_id`.
   The human repeats the same operator confirmation. An entry must have settled
   or been canceled before an exit can be submitted. To cancel an owned pending
   order, run the operator command with `--cancel`; that also requires the
   native preview and explicit confirmation.
8. After the trade closes and reconciles, the agent calls `paper.review` with a
   unique review ID, the trade's current `evidence_revision`, analysis and next
   action. Reviews append interpretation; they cannot rewrite historical facts.

The agent may choose hold/no trade. A reusable Recipe is not a periodic agent
scheduler. Polling and report refresh continue in the plugin, but new research
decisions/reviews require invoking the Neige agent. No hidden automation or
unattended order authorization is installed by this plugin.

## Recovery and operational limits

- Decisions are immutable. Retry the same ID and identical payload to retrieve
  state; a changed payload is refused. Never create a new ID merely to retry an
  uncertain broker submission.
- Before submitting, the ledger commits `submitting`. A crash or lost response
  leaves `unknown`; startup and polling reconcile instead of resubmitting. Known
  order IDs are queried directly; unknown IDs are recovered only from an exact
  unique broker remark and matching request fields. If the CLI omits remarks,
  unknown outcomes remain blocked for operator investigation, not heuristic
  matching or an unsafe retry. Do not clear unknown state by editing SQLite.
- A conflicting execution ID, incomplete fills for a filled order, mismatched
  position, unsupported status, or unknown active order rolls back that whole
  snapshot. The previous snapshot remains visible with an explicit error.
- At most 500 broker order identities are reconciled per pass. A larger history
  fails visibly; this first slice is not an unlimited historical broker archive.
- Risk exposure uses entry cost plus outstanding buy requests, not mark-to-market
  exposure or a VaR model. Reservations are conservative, including the entire
  notional of partially filled active buy orders; they can temporarily refuse
  additional trading rather than under-reserve cash.
- Gross P/L ignores commissions, financing, FX, dividends, corporate actions and
  tax. Splits or manual trades cause position mismatch and require investigation.
- Native confirmation can expire, fail or be mistyped. A broker-call failure is
  conservatively unknown unless its outcome is proven through reconciliation.
- `paper.pause` stops new entries but does not cancel existing broker orders,
  liquidate holdings or stop reconciliation. Disabling/removing the plugin or
  archiving/deleting the Track is NOT a broker kill switch. Resolve orders and
  positions before retirement; retain `ledger.sqlite3` and research snapshots.

`paper.journal` and the Report display the latest 200 journal events; the full
append-only history remains in SQLite. Publication failures retry projections,
not broker writes. Back up the ledger using SQLite's backup API while stopped or
with appropriate snapshot consistency; do not copy a live database arbitrarily.

## Verification

```sh
python3 -m pytest plugins/paper-trading/tests -q
```

Tests run production Engine and Broker code against a deterministic external CLI
fixture, plus real stdio and pseudo-terminal operator flows. No real credentials
or broker orders are needed. The fixture only returns prescribed broker records;
it does not implement the application's strategy, risk or P/L calculations.

For an isolated real-host and desktop/mobile browser check:

```sh
python3 plugins/paper-trading/tests/smoke_host.py \
  --server /path/to/calm-server --frontend /path/to/fe/dist \
  --browser-package /path/to/neige-calm/fe \
  --output /tmp/unique-paper-host-check
```

This creates fresh data, a fixture-only account, disabled real agent binaries,
a random loopback port and six native Report tables; Playwright captures both
viewports. The temporary server is stopped afterwards. It does not touch 4140
or prove that an actual Longbridge account has filled an order.
