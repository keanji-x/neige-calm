# Portfolio template workflow review — baseline and follow-up

Reviewed on 2026-09-09. Implementation: `4b33f0b10`, based on main
`b7bdad97c`. Scope: `prototypes/stock-journal` and its production Report,
Recipe, Track creation, and market overlay call paths.

## Baseline decision (4b33f0b10 only)

**Historical result: changes required.** The prototype demonstrates the visual layout and market
read path; it does not implement a reusable, chat-maintained dashboard. Do not
merge it as that feature. The findings below describe that baseline. Implementation cef4cbbe1 replaces
the read-only projection with native persisted layout components. Two independent
agents confirmed all four baseline findings resolved; their new P2 findings
(selection identity, market label clarity, nullable captions) are being fixed
and re-reviewed before the final convergence record is appended.

The acceptance outcome is: a user saves a dashboard Recipe, creates two Tracks
from it, changes one dashboard through chat, and can reload both in the normal
Neige application without losing changes or sharing portfolio data.

## Findings

### P1 — The displayed Report is not the persisted Report

`prototypes/stock-journal/src/live-market.ts:57–69` constructs a replacement card
on every Track read, substitutes its blocks and summary, fixes `docRev` to `1`,
and returns only that card. A saved dashboard layout, AI-written commentary,
task blocks, and other cards on this selected Track disappear from the displayed
response. The database is not overwritten, but its content is hidden.

Runtime reproduction supplied a saved Report titled `AI 保存的表盘`, with
`docRev: 42` and the instruction `交易日志优先展示。`. Two reads returned the
fixed portfolio layout and `docRev: 1`; neither preserved the saved body.

Required correction: render the actual persisted Report and its revisions.
Resolve live values at the individual block boundary. Do not synthesize a
replacement Track response or invent document/block identity.

### P1 — The preview prevents the requested authoring workflow

`prototypes/stock-journal/src/live-market.ts:23–26` rejects all non-GET requests
except login/logout. The actual native application uses this transport, so
template creation and ordinary authoring requests cannot reach their endpoints.

Runtime reproduction called `POST /api/track-recipes` through the real transport:
it returned `403 / readonly_view`, and the underlying transport received no
write. The same condition covers other non-authentication writes. Existing
browser tests assert that no writes happen; they do not prove chat authoring.

Required correction: integrate the widgets into the normal authenticated app.
Do not remove the guard in isolation: the synthetic Report in the preceding
finding has invalid authoring identity and revision semantics.

### P1 — A Recipe cannot reproduce this dashboard in the normal application

`prototypes/stock-journal/src/portfolio-framework.ts:110–137` owns section order,
table columns, headings, and iframe placement in code. The figure resource is
emitted only by the prototype's `chart-bundle.ts`/`vite.config.ts`, and its data
bridge is installed only by the prototype's `live-app.tsx`. Saving an `app` fence
with `/next/portfolio-demo.html` does not install either dependency in Neige.

Production `fe/web/src/features/report/app/public.tsx` deliberately has no such
bridge. Recipes persist report bodies and instantiate fresh blocks through
`prepare_initial_report_payload`; they do not package frontend code, workspace
metadata files, or arbitrary static resources.

Required correction: ship the reusable rendering capability in the normal
Report renderer, then persist its declarative configuration in Recipe bodies.
Do not create a second template store or use a hidden Track as a template.

### P2 — Dashboard activation depends on browser selection

`prototypes/stock-journal/src/live-market.ts:20,37,43` and `src/live-app.tsx:21–24`
select one portfolio using browser storage, independently of the saved Report.
In the reproduction, reading a second Track with valid market overlays created
no chart snapshot for it. Returning its original Report protects research
content, but does not implement independently usable dashboard instances.

Required correction: the presence of chart/table blocks in each saved Report
determines what renders. Resolve each block against its current Track. Market
overlays alone must not turn a research Report into a dashboard.

## Evidence and limits

Two checking methods were used: source/caller review and an isolated Chromium
runtime reproduction against the fixed implementation commit. These are not
two independent human/agent reviews. The runtime reproduction calls the actual
`createLiveMarketTransport`, with fixture data only at its API boundary; it does
not duplicate its projection logic or contact a private account.

Review worktree: `/tmp/neige-portfolio-review-20260909`.
Reproduction script: `/tmp/neige-portfolio-workflow-review.mjs`.
Observed output: `/tmp/neige-portfolio-workflow-review.json`.
Run with `node /tmp/neige-portfolio-workflow-review.mjs` while that worktree's
Vite server listens on port 5196. These are local review artifacts, not a
portable regression suite. The script verifies the observed defects; its zero
exit status does not mean the target feature passes acceptance.

Existing checks rerun on the unchanged implementation:

- `npm run test:model`: 8 passed.
- `npm run test:market`: 8 passed.
- `npm test`: 7 browser tests passed.

The first sandboxed model run failed to spawn its importer child process
(`EPERM`); rerunning with the required process permission passed all 8 tests.
No product assertion remained unexplained. These green tests establish the
prototype's existing read-only behavior, not the new template workflow.
No real AI execution, template persistence, or private account integration was
exercised. No runtime implementation changed during this review. At the baseline review date, this had not converged: the four findings were
open. See the follow-up status above for the subsequent implementation.

## Implementation design

### Ownership

| Concern | Owner |
| --- | --- |
| Reusable starting layout and instructions | Existing user Recipe body |
| Current layout, chart settings, user records, commentary | Current Track's persisted Report blocks |
| Holdings quantities, quotes, converted values, valuation history | Existing market plugin |
| Rendering and validation | Reusable native Report components and typed contracts |
| Session, writes, live invalidation | Existing Neige app infrastructure |

Store titles, section order, visible columns, chart field bindings, supported
time ranges, and format choices in validated block payloads. Keep shared
typography, transparent presentation, and default palette in the existing theme
and reusable components. Stable schema identifiers and allowed format enums
belong in code; individual securities, Track IDs, and a user's layout do not.

### Template and chat flow

1. Supply a portfolio Recipe with three sections: overview (line + allocation),
   holdings, and transaction log. No asset/return headline strip or exposure
   model. Blank instances contain no positions, trades, or research IDs.
2. The user saves it through the existing Recipe editor and selects it when
   creating a Track. Instantiation creates fresh Report block IDs and binds live
   sources to the new Track; it does not copy another portfolio's holdings.
3. Normal chat uses existing `calm.report.blocks.*` operations, with real
   revision checks, to change layout and records. Holdings registration uses
   the existing market tools. This is record keeping, never brokerage orders.
4. Refresh and another browser reconstruct the dashboard from persisted blocks
   and the current Track's overlays. No preview process or browser-selected
   portfolio ID is required.
5. A reusable Recipe retains structure and empty record tables. Saving an
   instance as a new template must strip instance-specific records and links;
   it must not be a blind copy of a personal Report. Until such an export exists,
   prepare a clean Recipe body for the user to save in the existing editor.

Recipe writes are intentionally human-only today:
`crates/calm-server/src/routes/track_recipes.rs:281–295`. Chat can edit the
current Report through the existing assistant/planner block tools; it cannot
silently write the shared Recipe using those credentials. Preserve that boundary
in the first implementation. An agent-facing Recipe tool would be a separate
authority change, not a transport workaround.

### Native rendering work required

The current typed vocabulary has tables, candles, tasks and embedded apps; it
does not have the configurable line/allocation chart capability demonstrated by
this preview. Add a small, typed native chart capability using shadcn/Recharts,
with explicit data source and field bindings. Keep configuration declarative:
no arbitrary JavaScript, SQL, HTML, or unrestricted expression evaluator in a
Recipe. Its exact payload is a contract change to design and validate in core
and the Rust block schema before implementation; this document does not invent
an already-supported fence format.

Extend native table presentation only as needed for selected/formatted columns,
research links and events. Keep manual records in persisted Report data rather
than requiring a separately initialized `.neige-portfolio/metadata.json` file.
Define the current-Report record binding and deterministic join on canonical
venue + asset before implementing enriched holdings. Reject ambiguous duplicate
keys. Do not make every dashboard implement its own parser/calculations.

The market adapter must retain producer-converted values, currency labels and
partial-data behavior. Missing daily change stays unavailable; do not derive it
from changes in total portfolio value. Do not infer executed trades from
holdings edits. Trade notes and holdings registration are distinct writes; an
incomplete pair must remain visible and recoverable rather than pretending to
be one atomic broker execution.

### Acceptance and delivery gates

First establish failing tests through the real Recipe-save → Track-create →
Report-render paths, then implement the native contracts and renderer:

- Save a Recipe, instantiate it twice, and render both in the normal app with
  the preview server stopped and browser storage cleared.
- Change section order, chart range, and a trade note through the real Report
  write entry point; reload and preserve values, block identity and revisions.
- A stale write conflicts without losing another writer's changes.
- Every instance reads only its own market overlays and records; research
  Reports with market overlays remain unchanged unless they contain widgets.
- Empty, denied, partial and stale data show explicit states; no demo fallback,
  fabricated daily changes or inherited personal records.
- Native build serves the chart assets; transparent layout and interactions
  work on desktop and mobile using shadcn/Recharts.

This affects persistence contracts and requires the repository's issue/design
workflow before runtime edits. Sweep Rust validators, MCP kind descriptors,
frontend schemas/renderers, fixtures, generated artifacts and callers together.
Keep released migrations unchanged. Run focused Rust/API tests, relevant frontend
gates and browser E2E; mutation-verify the load-bearing revision/isolation tests.
Review the final implementation through two independent channels and repeat
after fixes. The current read-only prototype tests cannot replace these gates.
