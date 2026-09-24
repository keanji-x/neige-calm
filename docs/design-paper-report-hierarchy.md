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

Introduce the first-class `view.live` report block, declared as
`{source, version: 1, view: "overview" | "activity" | "cards" | "details"}`.
The kernel validates and persists this reference through the existing block
write paths; tool discovery publishes the same contract. Its overlay must
match both the declared version and view. Tables accept table data only.
No plugin-id dispatch, payload guessing, or compatibility alias for the
unreleased table-as-view experiment is retained.

Responsibility boundaries:

- Kernel: block identity/CAS, bounded reference validation, existing Track
  overlay transport, and read-only MCP hydration. A read never invokes a tool.
  Hydration reports source availability and matching envelope; frontend schema
  validation still checks the complete untrusted presentation payload.
  Summary returns status, source, version, view, resolved_at and
  `validation: "envelope-only"`; full adds `data` (untrusted presentation).
  Both reject payloads larger than 4 MiB of compact UTF-8 JSON. None performs
  no overlay query when all overlay blocks opt out. Storage failure is
  unavailable, not pending; pending means a successful lookup had no match.
- Core: closed, versioned presentation data schemas, without DOM or geometry.
- Report renderer: chart geometry, layout, disclosure, safe text, accessibility.
  No trade calculations, business copy, or sign-to-success inference.
- Plugin/App: ledger projections, labels, timestamps and their meaning, status
  tones, empty states, and card sections. A meter is numeric usage, not a risk
  policy; positive bars can be negative-toned costs.
- Recipe: section ordering and explicit typed view references.

The existing `overview` grouping stays deliberately small: metrics, notices,
and bounded bars/meters. Cards contain labeled prose sections, not a mandatory
trading review/next-action shape. This is not an arbitrary UI DSL, executable
component registry, HTML embed, or permission surface.

The coordinating owner approves additive changes to report domain kinds,
ReportDocument's injected overlay resolver naming, and kernel kind discovery
for this request. No global style contract or authority boundary is relaxed.
The new block needs the matching server/frontend build. Old clients display an
unsupported kind; old table Recipes keep their seven original projections.
No released migration is changed. Only this PR's isolated preview references
are explicitly replaced through ordinary report APIs; production is untouched.

The presentation contract accepts only bounded text, finite numbers and known
view types. It never accepts HTML, script, arbitrary styles or action URLs.
Invalid/unknown versions are visibly refused; no guessing from plugin IDs. The
renderer adds no data fetching, approval, order or configuration write surface.
Existing table views remain supported through the same exact validator.

Paper-specific labels, semantic tones and projections belong to the plugin, not the generic
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

Cover native block write/read/round-trip and discovery, strict table rejection
of views, declared-view mismatches, source dispatch, unknown/error/empty data, positive
and negative charts, budget overflow, disclosure keyboard behavior, malicious
text rendering, long/Unicode content, and preservation of authoritative values.
Run Python plugin tests, frontend lint/build/unit gates, focused browser tests,
and the real isolated preview at desktop/mobile widths in both relevant states.
Use a non-financial operations fixture (positive cost is unfavorable, negative
cost favorable) to pin the platform/App boundary. Update the same PR, obtain
two fresh independent reviews, and update only the isolated preview.
