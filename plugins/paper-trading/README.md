# Longbridge paper portfolio

One investment Area, one long-lived strategy Track, one dedicated paper account.
Weekly reports are versioned inputs, not new accounts or new execution Tracks.
The plugin records decisions, reconciles broker orders and executions, maintains
a durable trading journal and renders native Report views. AI research and
review run in the existing Neige agent, not inside another model client.

**The default profile is supervised paper execution.** The installed Longbridge CLI has a two-step native preview and user
confirmation contract. In the supervised profile, no MCP tool submits/cancels orders or reveals confirmation
codes. The separate interactive operator command preserves that contract.

The explicit `spy_cash` profile supports Planner targets executed through an
ordinary Worker task without per-order confirmation. Its setup and boundaries
are documented below.

## Initial scope (supervised profile)

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

5. Verify the approved revision/settings under the Report's native strategy
   details before starting a paper cycle. The Recipe prioritizes account/return
   KPI cards, per-trade gross P/L bars and a cost-budget meter, followed by readable
   activity and review cards. Strategy, order and trade details use native table components.
   Existing paper tools retain their signatures.
   `paper.status` and `paper.journal` can inspect setup before approval; all
   other existing tools require an approved strategy for the host-provided owner Track.

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

Deploy this Recipe with the matching server and frontend supporting the
first-class `view.live` block. Every reference declares only its source and
version; legacy table sources remain table-only. The renderer accepts bounded data, not HTML,
script, styles or action URLs. The plugin owns labels, business calculations,
and semantic tones; the platform owns validation, layout and interaction.
The original seven table source IDs remain available to previously saved
Reports; seven additional source IDs carry the new visual views. Updating a saved
Recipe does not rewrite existing Track reports or approved strategy settings.

Native table cells and activity summaries show at most 2,048 Unicode code points;
long values end with `[truncated]`. Review cards retain the complete validated
review text. Full original facts remain in the ledger and tool responses.
Journal text is presented as readable event summaries instead of serialized JSON.
Report rows contain only their declared columns. Charts use existing executions
and cost accounting: account equity is not attributed strategy return, gross
P/L excludes fees, and no historical balance curve is fabricated.
Every holding alert remains accessible in the native alert table. Review
cards are ordered by their recorded journal sequence, not their arbitrary IDs;
missing or inconsistent review audit evidence is refused instead of silently
dropping a review. Expiration, rejection and confirmed cancellation remain in
the activity feed alongside fills and proposals.

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
adopting the visual report references and updated instructions from `recipe.md`.

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

`paper.journal` and the legacy journal table expose the latest 200 journal events.
The primary activity feed selects up to 100 important events from that window;
the full append-only history remains in SQLite. Publication failures retry projections,
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
a random loopback port and native Report views (plus the seven legacy table
projections); Playwright checks overview, unknown/approved states, native
tables and account settings at both viewports. The temporary server is stopped
afterwards. It does not touch 4140
or prove that an actual Longbridge account has filled an order.

The unmerged native view contract uses one composition for both inline Demo and
live overlays: rows of metrics, bars, meter, tables and generic records. Paper
trading owns all semantic badges, risk/approval labels and budget calculations.
The host only validates and renders these inert fields. Live references have no
preset selector; existing saved recipes on this development branch must be
updated from `recipe.md` before release. Snapshot identity hashes the projection
and its persisted reconciliation timestamp. `observedAt` records that actual
reconciliation time, or explicitly null when unavailable. The pure projection
has no publication clock, so live `producedAt` is explicitly null even when
reconciliation or journal times are known. Rendering never fabricates a refresh
or generation time. Legacy table source IDs and their payloads are unchanged.

## Automatic SPY/cash profile

This profile runs a daily SPY/cash target allocation with the official
Longbridge paper account. Planner research comes through existing Longbridge
and Wisburg connectors. The Planner captures source references and persists a
basis-point target; an ordinary Codex Worker task requests execution of that
immutable decision. The App's background loop calculates integer shares,
submits one DAY market order and reconciles actual broker fills. The App never
creates targets or submits an additional order to eliminate residual drift;
new targets come only from the Planner. Sources are durable citation references
supplied by the Planner; the App does not verify their content.

Use a fresh data directory and a dedicated paper account with no existing
positions or active orders. This profile and the supervised profile cannot share
a data directory or switch on an existing portfolio. Preserve any previous
ledger and resolve its orders and holdings before changing account use. Install
on a kernel that sends the `dev.neige/caller` identity to local plugins; without
it every SPY tool is refused. Local plugins share the service OS identity; this
is not an OS sandbox against another trusted local process.

Configure the scalar fields below. Replace every placeholder; the order cap is
a configurable fraction of current cash plus SPY market value:

```json
{
  "profile": "spy_cash",
  "account_no": "YOUR_VERIFIED_PAPER_ACCOUNT",
  "broker_home": "/private/path/sdk-home",
  "owner_track_id": "YOUR_SPY_TRACK_ID",
  "oauth_client_id": "YOUR_REGISTERED_OAUTH_CLIENT",
  "access_region": "cn",
  "sdk_python_path": "/private/path/spy-venv/bin/python",
  "max_order_bps": 1000,
  "poll_seconds": 60,
  "cash_buffer_bps": 200,
  "drift_bps": 100,
  "quote_max_age_seconds": 60
}
```

`max_order_bps`, `cash_buffer_bps`, `drift_bps`, and `quote_max_age_seconds` have the shown values
when omitted. The App uses the whole dedicated account's USD cash plus SPY
market value as the allocation base. It caps target exposure at the configured
cash reserve, rounds whole shares down, and sizes buys with an additional 1%
price reserve. Each decision executes at most one step: by default 10% of current account
value, including the price reserve. A distant target is approached in that
step, and the actual remaining drift is reported for the next decision. Market
orders have no guaranteed execution price. It does not use margin buying
power, spend positive unsettled proceeds or sell unavailable shares. The
App computes a smaller step when the target requires more than the cap; a
step too small for one whole share produces a no-op. The
snapshot shows the achieved ratio, which can differ from the target because of
rounding, cash availability, price movement and the no-trade band.

The official SDK is separate from the CLI's encrypted login cache. Build the
pinned SDK in a private virtual environment using `requirements-spy.txt`. The
published legacy 0.2.x Python package lacks the required OAuth and paper-enforcement
APIs. Register an OAuth client at Longbridge with the local callback URI
`http://localhost:60355/callback`, then perform its one-time account authorization
with the SDK interpreter, in the same configured broker HOME:

```sh
/private/path/spy-venv/bin/python -I paper_trading/sdk_bridge.py   --client-id YOUR_REGISTERED_OAUTH_CLIENT --account YOUR_VERIFIED_PAPER_ACCOUNT login
```

The login command displays the official authorization URL. A service/background
call never initiates interactive authorization or exports/decrypts CLI tokens.
No per-order confirmation is needed after configuring this explicit execution
profile. Every SDK operation uses `enable_papertrading=True`, which the broker
rejects for real-money credentials. Account proof uses a daily broker statement
from that same SDK login; absent or mismatched proof blocks execution. Protect
SDK HOME and installed files from unintended writers. Child environments use the
existing explicit PATH/LANG/proxy allowlist, with HOME pinned and no inherited
Longbridge endpoint overrides, model keys or arbitrary Python import paths.

### Track and Worker setup

Save `spy-recipe.md` as a user Recipe and create the owner Track from it on a
current kernel, managed or attached. Codex tasks run in the Track's checkout:
a managed Track gets its own Git workspace and an attached Track gets its
`neige/track-<id>` worktree. An attached Track created before per-track
worktrees refuses Codex tasks with `track-without-worktree`; create a new
Track instead. The Worker task is `access: "read_only"`, so the checkout must
only be clean; the App ledger lives in the plugin data directory, not in Git.
Only the user closes the strategy Track.

The Recipe runs the Track unattended. On the first user message the Planner
creates four weekly Calendar entries from the Track, in America/New_York:
weekday pre-market research 08:45, execution 09:45 and post-close review
16:30, and a Saturday weekly review at 10:00. Each entry wakes the Planner at
its start; the kernel needs Calendar wake and weekly recurrence (#1967), and
it does not start a Planner that never ran. The Planner skips a day the
Longbridge trading calendar marks closed. Pre-market research ends in either a
hold or `spy.plan` with decision ID `spy-YYYYMMDD` (the App accepts 1-55
lowercase letters, digits or hyphens and a validity of at most 24 hours; the
Recipe ends it at that day's 16:00 close). At the execution step the Planner
declares one `codex`, `access: "read_only"` task `spy-exec-<decision_id>`;
Claude Workers receive no plugin MCP tools. Only one unresolved decision is
permitted, and every blocked or uncertain state stays in the Report for
reconciliation instead of a retry.

All four tools declare `destructiveHint: false` and `openWorldHint: false`, so
Codex agents running with `approval_policy = never` can call them without an
approval prompt. These annotations are truthful because no tool call touches the
broker: `spy.plan` writes the local ledger, `spy.refresh` only wakes the
background loop, and `spy.execute` durably records "execution requested by this
Worker for decision D" (state `requested`, journal event
`allocation_execution_requested` with the caller) and wakes the loop. The loop
then reconciles a fresh broker snapshot and, for the requested decision only,
commits the exact order intent before the SDK submission. It never needs an
operator approval override.

The App reads the kernel-resolved caller from `_meta["dev.neige/caller"]`
(`role`, `card_id`, `session_id`); tool arguments and the request's own `_meta`
cannot supply it. `spy.plan` requires the Planner role and `spy.execute` the
Worker role, both on the owner Track; `spy.status` and `spy.refresh` accept
either. Execution requests are refused for an unknown, expired or no longer
queued decision; repeating a request for a `requested` decision records nothing.
The Worker then polls `spy.status` until the decision is `settled`, `noop`,
`rejected`, `canceled`, `expired` or `unknown`, or until its stated time limit,
and reports that outcome. A `requested` decision's `error` explains why the
loop is still waiting: outside the regular session, a stale quote, insufficient
settled cash or unavailable shares. The loop retries each poll until the
decision expires. Market holidays and half trading days are checked against the
broker calendar. `spy.portfolio`, `spy.decisions` and `spy.fills` are native live
tables that can be bound in the Track Report.

**Known gap:** the App cannot prove that the Planner created the requesting
Worker task. Any Worker-role caller on the owner Track may request execution of
the current queued decision. The Planner alone chooses targets, the App alone
sizes and submits, and each decision executes at most one order.

### Execution invariants

Intent and exact quantity are committed before submission under the operation
lock. The broker request has a stable remark and client request ID; its 10-minute
server idempotency cache is an extra guard, not the recovery source of truth.
A completed SDK preflight refusal returns an exact `not_submitted` outcome and
resolves the decision as rejected, allowing a fresh target. After the broker
write may have started, timeout or restart results stay unknown; later loop
passes reconcile only and never resubmit. The loop rechecks the decision deadline
immediately before committing intent, and the SDK preflight refuses the order
(`not_submitted`) once the decision's `valid_until`, carried as `not_after`, has
passed. Broker history is read only from the oldest unresolved order; settled,
canceled, rejected and expired orders keep their reconciled fills locally, so each
poll stays bounded for an unattended account. With no unresolved order, the unowned-active-order
check sees only the broker's orders for the current day: a multi-day order placed
outside the App on an earlier day stays invisible until its fills make the holdings
check fail closed. Queued and requested decisions expire locally at `valid_until`
even when the broker cannot be read; uncertain submissions stay reconciliation-only. Recovery requires one exact remark/payload match. Unknown active orders,
external positions, conflicting execution IDs and incomplete fill totals block
execution and roll back the observation; the snapshot `error` names the cause.
Reconciliation can be retried; deleting
or editing SQLite is not recovery. Disabling this plugin does not cancel orders
or liquidate positions. Fees, financing, dividends, tax and net returns are not
calculated in this first allocation slice.

`access_region` selects a fixed official SDK network entry: `global` (default)
or `cn`. It never accepts a custom URL or retries an uncertain write through a
second entry. A previous host verified SPY reads with `cn`; `global` quote reads
were reset by its network. OAuth/account identity is unchanged by the entry choice.
