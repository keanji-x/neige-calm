# Paper report hierarchy and native live views

## Outcome

PR #1770's isolated preview should prioritize decisions and outcomes rather than
seven equally weighted tables. Production port 4140 remains unchanged.

The new Recipe orders four sections: overview, readable activity, review cards,
and collapsed reference details. Overview contains four KPI cards, prominent
reconciliation/pending-policy notices, per-trade gross realized P/L bars, and a
strategy cost-budget meter. Sparse data remains sparse: no fabricated equity
curve, historical balance backfill or unattributed account-return percentage.

## Contract and ownership

Add a versioned, closed, pure-data live-view schema in `fe/core/domain` and native
renderers inside the existing `features/report` boundary. The existing live
`table` source resolver can receive either its original table payload or an
explicit `{version: 1, view: ...}` payload (`overview`, `activity`, `cards`, or
`details`). Inline table/report write schemas stay unchanged, as do Rust/API
types. These views are plugin overlay payloads, not a new kernel block kind.

The presentation contract accepts only bounded text, finite numbers and known
view types. It never accepts HTML, script, arbitrary styles or action URLs.
Invalid/unknown versions are visibly refused; no guessing from plugin IDs. The
renderer adds no data fetching, approval, order or configuration write surface.
Existing table views remain supported through the same exact validator.

Paper-specific labels and projections belong to the plugin, not the generic
frontend renderer. Full strategy/ledger/tool evidence is unchanged. Legacy
overlay source IDs remain explicit supported projections for previously saved
Recipes; new source IDs carry the richer views. Both routes replace raw journal
JSON with bounded, human-readable event summaries.

## Data meanings

- Account equity/cash are broker account totals, not strategy-attributed returns.
- Realized gross P/L comes from existing trade accounting, including partial
  exits, and is explicitly labeled as excluding fees.
- Budget usage is existing entry-cost exposure plus remaining buy reservations;
  use the execution accounting helper for cost. Unknown snapshots show unknown,
  not zero. An over-limit meter retains the actual amount even if its bar clamps.
- The profit chart labels actual trades and presents a bounded recent subset;
  no time-series interpolation. Pending proposals do not change approved limits.
- Activity maps known ledger event types into plain-language titles/details and
  resolves symbols from recorded decisions. Technical transport churn is kept
  out of the primary feed, not removed from the ledger.

## Checks

Cover schema rejection and source dispatch, unknown/error/empty data, positive
and negative charts, budget overflow, disclosure keyboard behavior, malicious
text rendering, long/Unicode content, and preservation of authoritative values.
Run Python plugin tests, frontend lint/build/unit gates, focused browser tests,
and the real isolated preview at desktop/mobile widths in both relevant states.
Update the same PR, obtain two fresh independent reviews, and update only 4142.
