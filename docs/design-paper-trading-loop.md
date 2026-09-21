# Track-owned Longbridge paper trading loop

Issue: https://github.com/keanji-x/neige-calm/issues/1766

## Outcome and scope

One investment Area contains one long-lived strategy Track. A local app plugin
connects versioned weekly research, explicit agent decisions, supervised paper
orders, broker reconciliation, an append-only journal, and evidence-linked
reviews. This is an executable broker integration, not a shadow portfolio.

The installed Longbridge CLI requires a native preview and explicit user
confirmation. This implementation preserves that contract: MCP tools cannot
submit, cancel, obtain, or redeem confirmation codes. An interactive operator
command displays the native preview and accepts the user's code. Unattended
execution is NOT claimed. AI research and review use Neige's existing agents;
the plugin does not create another model client or read model credentials.

Initial trading scope is US-listed, USD, integer-share, long-only, regular-session
limit orders. Short sales, paired execution, leverage, options, order replacement,
and unattended protective orders are explicitly unsupported. Reports containing
short theses remain research inputs, not executable orders. No silent proxy
instruments or simulated local fills. The user can cancel an owned broker order
through the same native confirmation flow. Stop/target alerts require a reviewed
exit; an alert is not an installed broker stop order.

## Domain and ownership

- Area: organization and default workspace, not a financial authority boundary.
- Portfolio: one operator-configured paper account and one owner Track, stored
  in plugin data. The Track survives report weeks and holding periods.
- Source: an immutable report plus the selected week's structured predictions,
  identified by a content hash. Source input files are never modified.
- Decision: immutable request identity, cycle, source, prediction, trade, action,
  symbol, sizing, price levels, rationale, and validity deadline. Reusing an ID
  with different bytes is refused; retrying identical content returns its state.
- Trade: one long thesis for a symbol, with one initial entry and possible
  partial exits. No overlapping open trades in the same symbol. Order and trade
  lifecycles are not Neige Track lifecycles.
- Order: exact broker request, durable pre-submit state, broker ID and status.
- Fill: immutable broker execution ID, quantity, price, time, and owning order.
- Journal: append-only facts (including refusal, ambiguity, and reconciliation).
- Review: append-only agent interpretation referencing a reconciled trade and
  its evidence revision. It cannot change decisions, orders, or fills.

Neither account identity, owner Track, nor risk limits are accepted from tool
arguments. Configuration comes from the owner-managed plugin configuration.
Track identity comes exclusively from host MCP metadata. This follows the
existing trusted-local-plugin model; filesystem/OS access is NOT an isolation
boundary against another process with the same service identity.

## Integration

Add `plugins/paper-trading`, using the existing Python stdio app/overlay pattern.
Use the standard library for SQLite, JSON, Decimal, subprocesses, and transport.
There are no core schema changes or modifications to released migrations.

Configuration pins an absolute CLI path, CLI HOME, expected account number,
owner Track, research root, symbol allowlist, order/portfolio/risk limits,
quote freshness and price-deviation limits, and reconciliation interval.
One SQLite database lives in NEIGE_PLUGIN_DATA_DIR. A per-operation file lock
serializes the plugin and interactive operator; SQLite transactions durably
record intent BEFORE a submission. Broker child environments are explicitly
constructed, excluding inherited Longbridge endpoint/auth overrides and
unrelated credentials. No shell invocation or model-controlled CLI flags.

Ordinary MCP tools ingest research, queue decisions/refresh, inspect status and
the journal, pause new entries, and append reviews. Network work occurs in a
documented background reconciliation worker. Local queue tools do not falsely
claim their downstream work is a read-only operation. Operator commands, not
an MCP tool, perform native CLI preview/confirmation.

Report integration uses native tables for portfolio status, decisions, orders,
trades, journal and reviews. Overlays are projections, never the source of
financial truth. Failed publication is retried without repeating broker writes.
The recipe explains to the agent how to ingest the requested week, create an
evidence-linked decision, request operator confirmation, reconcile and review.
It does not claim that a Recipe is a recurring scheduler or that Track archival
closes positions. A background polling loop reconciles and produces alerts;
periodic AI decision-making is outside this first supervised slice.

## Broker and persistence contract

1. Verify `auth status` is valid, `account_channel == lb_papertrading`, and the
   account number matches configuration before accepting a broker snapshot or
   preparing/submitting/canceling an order. Missing identity fails closed.
2. Fresh broker positions and active orders must agree with the plugin ledger.
   Unknown positions/orders, conflicting fill IDs, or mismatched broker request
   fields block trading instead of being silently attributed to the strategy.
3. Entry validation covers source/prediction identity, explicit long direction,
   validity, positive finite decimal prices, stop < entry < target, integer
   sizing, allowlisted symbol, fresh quotes, cash, and outstanding reservations.
   Exit validation covers owned available quantity, pending sell reservations,
   a fresh quote, and exact existing trade identity.
4. Each request carries a stable broker remark for recovery. Persist
   `submitting` before invoking `--execute`. Timeout, malformed success, crash,
   or ambiguous match becomes `unknown`; never automatically resubmit.
5. Reconcile today's and relevant historical orders/executions. A request is
   accepted only when order identity and payload match; a successful submission
   is not a fill. Partial fills are durable and deduplicated by execution ID.
   Filled quantities must agree with broker order totals before settling.
6. Realized gross P/L is calculated with Decimal from actual fills and weighted
   entry cost. Costs/net P/L are not invented when fee data is unavailable.
   Initial price risk is distinct from target-distance progress. Open positions
   are not reported as realized profit. Broker account equity is explicitly
   labeled as account equity, not attributed strategy return.
7. Pausing stops new entries, not reconciliation or authorized exits/cancels.
   Plugin shutdown never implies liquidation. A closed Track must not be relied
   upon as a broker kill switch. Never delete the ledger as rollback with live
   paper positions or unresolved orders.

## Acceptance and verification

- Test production parser, ledger, runtime and real stdio/CLI subprocess entry
  points with a deterministic external broker fixture, not copied domain logic.
- Cover complete entry/fills/restart/partial exit/final exit/review; idempotent
  retries; conflicting requests/fills; missing and wrong Track; non-paper and
  switched accounts; stale quotes; sizing; expiry; pause; unavailable broker;
  timeout-after-submit and history recovery; external activity; native approval.
- Validate the manifest, recipe and overlay integration against the existing
  host. Use isolated host data, fake provider binaries and fixture brokerage.
  Do not touch port 4140, real accounts, or shared-host real Codex E2E.
- Mutation-verify the paper-account fence and no-repeat-submit assertion in an
  exclusive recoverable worktree. Predict the complete failing test set first,
  restore with a patch, and rerun the original tests.
- Two independent complete-diff reviews in separate immutable review worktrees;
  fix actionable in-scope findings and rerun both reviews after each fix.
- Run focused plugin tests and relevant integration checks, inspect final diff
  and status, and record exact results. No workspace-wide Rust suite for this
  plugin-only change. Deploy and real paper order acceptance require a later
  explicit operator action; implementation tests must not claim broker fills.

## Delivery and rollback

Install disabled, configure, enable, and create one strategy Track from the
recipe (or create the Track first to obtain its configured owner ID). Mount
research read-only. Keep release code and configuration outside agent-writable
workspaces when deploying. The CLI login belongs to the configured HOME.
Disable new entries first on rollback; reconcile/cancel outstanding orders and
manage existing paper positions before disabling the plugin. Preserve all data.

## Verification record

- Initial complete suite: `python3 -m pytest plugins/paper-trading/tests -q`,
  191 passed. This includes real stdio app startup/host callbacks, external CLI
  fixture calls, and a pseudo-terminal operator waiting for explicit input.
- Initial isolated host smoke with the installed `2fdd018d3` binary and its
  matching frontend: install/configure/enable, all six overlays, and desktop
  1366x1000/mobile 390x844 browser assertions passed. No real agents or orders.
  The first install exposed the scalar-only config schema; `symbols_json`
  uses JSON parsing and runtime validation rather than widening host schemas.
- Account-fence mutation in the broker-only exclusive worktree removed only
  the `account_channel != lb_papertrading` predicate. Predicted and actual red
  set: `test_identity_rejects_wrong_account_or_channel[account_channel-lb_live]`,
  `[account_channel-paper]`, `[account_channel-None]`, and
  `test_identity_requires_each_field[account-account_channel]`. Actual result:
  4 failed / 145 passed; restored 149 passed. Two later cancellation-shape tests
  brought the broker suite to 151 passing tests without changing that fence.
- No-repeat mutation at `85f2e8e2a`, in a separate exclusive mutation worktree,
  disabled only the `queued/ready` preflight state predicate. The predicted
  complete red set was `test_no_repeat_submit_after_lost_ack`. Running the full
  191-test suite produced exactly 1 failed / 190 passed; restoring via patch
  produced 191 passed and `git diff --exit-code` confirmed zero residue.
- Independent review round 1 at `85f2e8e2a` found six unique in-scope defects
  (both channels reported the invalid horizon): entry allowlist revalidation,
  schema-supported numeric prices, canonical research dates, repeating weighted
  fill averages, partial-withdrawal fill consistency, and double-counted sell
  reservations. Each has a red-before-fix regression. Fixes also sweep buy-side
  reservations, remaining quantities and numeric broker serialization. The
  updated complete suite passes 207 tests. Fresh full-diff review is pending.
- No production deployment or actual Longbridge broker order acceptance test
  has been performed. The host/browser harness uses the real deployed binary
  only as a separately spawned executable with isolated data and fake providers.
