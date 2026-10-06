# Longbridge paper portfolio

One dedicated Longbridge paper account, one owning Track and one SPY/cash
target allocation. The Planner researches and persists a target, an ordinary
Worker task requests its execution, and the App's background loop sizes,
submits and reconciles official-SDK paper orders and renders native Report
data units. AI research runs in the existing Neige agent, not inside another model
client. The required `spy_cash` profile is the explicit opt-in to automatic
paper execution without per-order confirmation; its setup and boundaries are
documented below.

## Verification

```sh
python3 -m pip install 'pytest>=8,<10' 'jsonschema>=4.18,<5'
python3 -m pytest plugins/paper-trading/tests -q
```

`jsonschema` validates the published data units and the recipe's template views
against the crate-owned native view schema; it is a test-only dependency. Tests run the production App, its stdio
entry point and its SDK subprocess runner against deterministic fixture
executables that only return prescribed broker records. No real credentials or
broker orders are needed.

`examples/native-demo.json` holds `views`, the template views of
`spy-recipe.md`, and `overlays`, every data unit the App publishes for example
data, by overlay kind. `examples/build_native_demo.py` drives the production
`Allocation` with a fixed clock and a scripted, simulated paper account. The
account supplies about three months of New York trading-day SPY quotes, cash,
shares, orders and executions. Planner targets and Worker requests enter through
`Allocation.call`, and the background pass, sizing, reconciliation, valuation and
units are production code. The builder replaces only each view's description,
which marks the data as an example and not a real account. Regenerate the file
with `python3 plugins/paper-trading/examples/build_native_demo.py`. The `--check`
flag verifies that the committed file is byte-identical to a fresh run.

## Automatic SPY/cash profile

This profile runs a daily SPY/cash target allocation with the official
Longbridge paper account. The 总览 Planner reads the SPY price and the trading day
from `spy.status`; its SPY 研究 Track reads research evidence from Wisburg.
Neither uses the Longbridge CLI or holds broker credentials. The research Track captures source references and the 总览
Planner persists a basis-point target; an ordinary Codex Worker task requests execution of that
immutable decision. The App's background loop calculates integer shares,
submits one DAY market order and reconciles actual broker fills. The App never
creates targets or submits an additional order to eliminate residual drift;
new targets come only from the Planner. Sources are durable citation references
supplied by the Planner; the App does not verify their content.

Use a fresh data directory and a dedicated paper account with no active orders.
If the account already holds SPY, set `opening_shares` to that exact share
count; it must match the broker's holding at the first reconciliation, which
then pins it to the ledger, and these shares become part of the allocation. Any
other unexplained holding blocks execution, and a changed `opening_shares` is
refused on an existing ledger. Preserve any previous ledger and resolve its
orders and holdings before changing account use. Install
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

Save `spy-recipe.md` and `spy-research-recipe.md` as user Recipes. Enable the
App first, then create the owner Track (the 总览) from `spy-recipe.md` on a
current kernel, managed or attached: a Planner thread that starts before the
App is enabled cannot call its tools (#2014). Codex tasks run in the Track's checkout:
a managed Track gets its own Git workspace and an attached Track gets its
`neige/track-<id>` worktree. An attached Track created before per-track
worktrees refuses Codex tasks with `track-without-worktree`; create a new
Track instead. The Worker task is `access: "read_only"`, so the checkout must
only be clean; the App ledger lives in the plugin data directory, not in Git.
Only the user closes the strategy Track.

The Recipe runs the Track unattended. The 总览 Report is only the account
dashboard: the three live views and one link to the SPY 研究 Track. Research
lives in that Track's report. On the first user message, which gives the
research Recipe's id, the 总览 Planner creates the SPY 研究 Track with
`neige_track_add` and four weekly Calendar entries from the 总览 Track, in America/New_York:
weekday pre-market research 08:45, execution 09:45 and post-close review
16:30, and a Saturday weekly review at 10:00. Each entry wakes the Planner at
its start; the kernel needs Calendar wake and weekly recurrence (#1967), and
it does not start a Planner that never ran. Pre-market and post-close refresh
and stop unless the `spy.status` snapshot's `calendar_date` is today's New York
date and `trading_day` is true. The snapshot's `calendar_date`, `trading_day`,
`half_day` and `regular_close_at` come from the broker calendar the App's SDK
reads for that date; a failed or incomplete calendar read fails the whole
reconciliation, so the Planner stops instead of guessing. On a confirmed
trading day pre-market mails the research Track (`neige_mail_send`), which
researches, rewrites its report and replies with a suggested ratio and its
sourced reasons. The reply wakes the 总览 Planner, which before 09:30 decides:
either a hold or `spy.plan` with decision ID `spy-YYYYMMDD` (the App accepts 1-55
lowercase letters, digits or hyphens and a validity of at most 24 hours; the
Recipe ends it no later than the snapshot's `regular_close_at`). Without a reply
in time there is no decision that day. Execution acts only on a queued decision,
which the pre-market decision saves only on a confirmed trading day. The
Saturday weekly review also asks the research Track for its review by mail. At the execution step the Planner
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
broker calendar.

The App publishes eight data units, each one cell with its own labels, units,
tones and empty text: `spy.nav` (total equity, the previous New York trading
day's valuation and the day's change), `spy.nav_history` (equity and return
history against the SPY price), `spy.weights` and `spy.weight_history`
(current and historical SPY/cash weights), `spy.holdings`, `spy.decision_log`
(the latest 50 decisions with up to 20 actual fills each), `spy.fill_log`
(every fill in the status list) and `spy.account` (reconciliation time or
error, quote time, order step, cash reserve and available cash). The recipe's
template views place them: 组合表现, 资金投向 and 调仓决策 open the Report, and
the template, not the App, owns their headings, rows and layouts. Every successful
reconciliation upserts one valuation sample (date, reconciliation time, equity,
cash, shares, price; exact decimal strings) per America/New_York quote date in the ledger's
`valuations` table, so the latest observation of each date wins. The history
units read the latest 260 samples; agent tool responses omit them. Values are reconciled paper-account valuations;
P&L includes no fee or deposit adjustment. The Planner rewrites the
research sections (结论, 待你定, 核心逻辑, 关键数据, 风险与证伪, 催化剂与跟踪, 复盘,
来源与边界) to its current judgment rather than appending dated notes, and never
rewrites the template views. 执行记录 is the last section and is never rewritten:
each upserted execution task block is appended there at the end of the Report.

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
