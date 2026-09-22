# Native structured report composition

## Outcome

Replay the accepted three-row portfolio Demo using Neige-native components,
not an iframe, HTML, script, screenshot, or plugin-specific frontend branch.
Enable the normal Planner on the isolated 4143 instance and ask it to read and
critique the same report data. Production 4140 is unchanged.

## Contract and ownership

Add one first-class inline `view` block. Its version-1 payload contains title,
description, snapshot identity/timestamps, and at most six nonrecursive rows.
Each row declares one/two/three columns containing bounded native primitives:
metrics, time series (line/stacked), distribution, inline table, record browser.
One composition is one persisted report block and one CAS revision: UI and
Planner read the identical canonical payload atomically. There is no second
machine-facing snapshot and no inference from plugin ids. Existing `table`,
`app`, `chart.series`, and `view.live` contracts remain unchanged.

Numeric metrics preserve raw values, units, precision, and explicit unknown
states. Chart nulls are gaps, never zero. Samples are ordered UTC calendar dates;
stacked charts require complete nonnegative values. Palette choices encode
series identity, while publisher-supplied tones encode meaning. Neither signs
nor thresholds imply investment judgments in the renderer.

- `calm-types`: strict write validation and kind vocabulary; 256 KiB canonical
  report-block limit still applies. No migration or execution permission.
- `fe/core/domain`: matching closed read schemas and typed composition values.
- `ui/data-visualization`: domain-independent chart/metric drawing and controls.
  No import from `core/domain`, no fetching, no finance calculations.
- `features/report/native`: Neige layout, table reuse, record/evidence disclosure,
  and existing wide Dialog for dense compositions. No trading labels or rules.
- App/Recipe author: business calculations, semantic labels, evidence, snapshot
  identity, and explicit composition in a `neige-block view` fence.

The coordinating owner approves this additive report contract and its new
native renderer. The only frozen-inventory change is registration of two new
leaf owners (`ui/data-visualization`, `tools/report-view`); existing ownership,
readonly flags, and enforcement rules remain unchanged. No frozen runtime
interface or global style change is requested. The commit records the
`OWNERSHIP-CHANGE` trailer for this narrow registration under #1769.

## Scope

This is a native reading/inspection Demo, not an order or scheduling service.
Chart dataset selection, card/list choice and evidence disclosure are local
presentation only. Do not imitate persistence with browser-only decisions.
Editing state uses existing report authoring/CAS, not a new embedded action
protocol. Durable trading-thesis workflow enforcement remains App responsibility.

The old iframe staging service was stopped without switching 4143. It must not
be deployed. The prepared dedicated SQLite/HOME may be reused after removing
the abandoned iframe report/plugin through ordinary Neige APIs.

## Acceptance

- Shared valid/invalid conformance fixtures exercise Rust and frontend validators.
- Unknown fields/kinds, duplicate ids, invalid dates, mismatched samples,
  non-finite values, malformed tables, and size limits are rejected.
- Real report upsert/read/round-trip and stale CAS behavior retain the exact data.
- A non-financial fixture verifies no domain-specific semantics in primitives.
- No iframe, script, network fetch, or action capability in native rendering.
- Desktop/mobile/wide-dialog inspection and keyboard controls work; gaps and
  unknown values are visible. Planner actually reads the report before its critique.
