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

## Account-only setup

Runtime: Linux, Python 3.11+ at `/usr/bin/python3`, the Longbridge CLI, and an
operator-created paper login. The plugin has no third-party Python dependencies.
Protect installed code, configuration, ledger and broker HOME from unintended
writers; local plugins share the service OS identity, not an OS sandbox.

1. Install this directory via Settings -> Plugins -> Server directory. Leave it
   disabled until the account connection is configured. Installing files alone
   does not enable it. Existing 0.1.0 installations must first follow the explicit
   legacy migration below, retaining their original data directory.
2. Configure only the account connection. Replace the placeholders with the
   operator-verified paper account and its dedicated broker HOME:

```json
{
  "account_no": "YOUR_VERIFIED_PAPER_ACCOUNT",
  "broker_home": "/home/operator/paper-home",
  "cli_path": "/usr/local/bin/longbridge",
  "poll_seconds": 60
}
```

Only `account_no` and `broker_home` are required. The optional `cli_path` defaults
to `/usr/local/bin/longbridge`; `poll_seconds` defaults to 60 (range 5-3600).
There are no global strategy fields, risk defaults or owner Track input. Use a
dedicated broker HOME and do not switch its login or modify Longbridge
authentication during a session.
The child environment inherits only PATH/LANG and an explicit proxy allowlist;
HOME is pinned. Longbridge endpoint overrides and unrelated credentials are not
inherited. A broker failure is never replaced with synthetic prices or fills.

3. Retain the same account-only values in an operator-managed `ACCOUNT_JSON` for
   the interactive commands below. The plugin data directory is
   `<plugins-data-dir>/dev-neige-paper-trading`; do not use a second ledger for
   the same account. `EXISTING_ROOT` below means this exact plugin data directory,
   not a new directory or the parent `plugins-data-dir`.
4. Enable the plugin. Account-only initialization needs no strategy and grants
   no trading authority. A fresh installation remains unapproved until the
   following proposal and human approval steps are complete.

## Saved Recipe and strategy approval

1. Create an investment Area, save [recipe.md](recipe.md) as a user Recipe with
   its HTML comments intact, and create one long-lived strategy Track from it.
   This is the sole shipped Recipe source. Plugin installation does not add a
   selectable template: the host template roster is not dynamically extensible
   through this manifest, so no `templates` entry is declared.
2. Start a new agent conversation in that Track. Discuss the method and supply
   the required choices below. The Recipe contains the research, sizing,
   decision and review instructions; it contains no example risk limits to
   activate. Missing choices must be collected, not inferred.

| `paper.strategy` argument | Required choice or optional default |
| --- | --- |
| `research_root` | Absolute path to the read-only `financial_agent` repository. |
| `symbols` | JSON array of 1-30 unique allowed US stock/ETF ticker strings in `SYMBOL.US` form, not `symbols_json`. |
| `max_order_usd` | Positive decimal USD string; no default. Must not exceed `max_portfolio_usd`. |
| `max_portfolio_usd` | Positive decimal USD string; no default. Caps entry-cost exposure plus reserved buy notional, not market value or loss. |
| `max_trade_risk_usd` | Positive decimal USD string; no default. Caps entry-limit minus stop distance times shares, not actual maximum loss. |
| `quote_max_age_seconds` | Integer 30-300; 180 only if absent. |
| `max_price_deviation_bps` | Integer 1-500; 100 only if absent. |

3. The agent calls `paper.strategy`, which never calls the broker, and reads
   `state.strategy`: `phase` is `unconfigured`, `awaiting_approval`, `approved` or
   `migration_required`; `account_no` identifies the configured account;
   `active` and `proposal` are each null or contain `revision`, `track_id` and
   `settings`. Report the exact proposal revision, Track, effective settings
   (including optional defaults) and phase, distinguishing the proposal from
   active settings. Account connection comes from trusted plugin settings;
   Track identity comes exclusively from the host. No account, owner or approval arguments are
   accepted. Repeating identical settings is idempotent; changes create a new
   proposal revision. **A proposal is never approval.**
4. From the installed plugin's release directory, the human operator runs:

```sh
python3 -m paper_trading.operator \
  --config ACCOUNT_JSON \
  --data-dir EXISTING_ROOT \
  --approve-strategy PROPOSAL_REVISION
```

The interactive operator displays JSON with the exact account, Track, proposal
revision and limits, then requires the human to type literal `APPROVE`.
Blank or noninteractive input cannot approve.
Agents must never invoke this command or approve on the user's behalf. Chat
agreement, Recipe edits and Report edits are not approval, and there is no
browser confirmation route. Strategy approval is a local policy approval,
**not** the broker's native order confirmation and not an order submission.

5. Verify the approved revision/settings in the Report's first `Strategy` table,
   sourced from `paper.strategy`, before starting a paper cycle. The other six
   sections remain Paper portfolio, Decisions and orders, Trades, Attention,
   Trading journal and Reviews. All other paper tools retain their signatures
   and require an approved strategy for the host-provided owner Track.

One dedicated paper account can have only one approved Track. It cannot be
rebound to another Track, even after closing its trades; never delete data or
create another independent ledger to bypass this binding. Multi-strategy shared
accounts are not supported. Same-Track settings changes require a new proposal
and explicit approval, no unresolved decisions or open shares, and a broker
identity/position/order reconciliation before approval. External positions and
unknown active orders block operation. Stale previews cannot cross an approved
strategy revision.

Approved typed settings and proposal history persist in `strategy.sqlite3`
alongside the existing `ledger.sqlite3`, independently of mutable Recipe and
Report prose. Preserve both databases and research snapshots.

## Explicit legacy migration

For an existing 0.1.0 installation, do not replace or reset its ledger, infer
previous limits, create a new Track, or start a second ledger for the account.
Stop the old plugin process and make a consistent backup of its data and saved
full configuration before upgrading the installed files.

Keep the original full JSON as `LEGACY_FULL_CONFIG_JSON`, including its exact
`owner_track_id`, `research_root`, `symbols_json`, risk limits and optional
settings. Create a separate account-only `ACCOUNT_JSON` with the same verified
connection values, and use those account-only values in plugin settings.
From the upgraded plugin's release directory, the human runs:

```sh
python3 -m paper_trading.operator \
  --config ACCOUNT_JSON \
  --data-dir EXISTING_ROOT \
  --import-legacy LEGACY_FULL_CONFIG_JSON
```

This explicit interactive migration requires the human to type literal `IMPORT`,
matches the account connection and original account/Track binding against the
legacy settings, retains the existing ledger bytes/history and research
snapshots, and records strategy state separately. Missing or mismatched legacy
configuration must be resolved, never guessed or backfilled. Import is not an
agent action, browser action or an authorization to submit orders. Successful
import establishes the approved legacy strategy snapshot; inspect the operator's
result and approved settings before resuming. Later changes still need a new
proposal and exact-revision human approval as above.
Continue in the original Track, retaining its other Recipe customizations when
adding the `Strategy` section and updated instructions from `recipe.md`.

## Run the first real paper cycle

After strategy approval, ask the Track agent to ingest the requested week,
analyze the report alongside current market data and account status, and record
an explicit decision. The recipe gives the agent this sequence:

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
  --config ACCOUNT_JSON \
  --data-dir EXISTING_ROOT \
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
   order, the human runs the same `--decision YOUR_DECISION_ID` command with
   `--cancel`; that also requires the native preview and explicit confirmation.
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
- Risk exposure uses entry cost plus the unfilled quantity of outstanding buys,
  not mark-to-market exposure or a VaR model. Local proposals reserve additional
  cash/shares; broker-working orders are already reflected in broker availability
  and are not subtracted from it again. Both kinds count toward owned-quantity
  and portfolio-exposure limits.
- Gross P/L ignores commissions, financing, FX, dividends, corporate actions and
  tax. Splits or manual trades cause position mismatch and require investigation.
- Native confirmation can expire, fail or be mistyped. A broker-call failure is
  conservatively unknown unless its outcome is proven through reconciliation.
- `paper.pause` stops new entries but does not cancel existing broker orders,
  liquidate holdings or stop reconciliation. Disabling/removing the plugin or
  archiving/deleting the Track is NOT a broker kill switch. Resolve orders and
  positions before retirement; retain `ledger.sqlite3`, `strategy.sqlite3` and
  research snapshots.

`paper.journal` and the Report display the latest 200 journal events; the full
append-only history remains in SQLite. Publication failures retry projections,
not broker writes. Back up both databases while the plugin and operator are
stopped, or use SQLite's backup API with coordinated snapshot consistency; do not
copy live databases arbitrarily.

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
a random loopback port and seven native Report tables; Playwright captures both
viewports. The temporary server is stopped afterwards. It does not touch 4140
or prove that an actual Longbridge account has filled an order.
