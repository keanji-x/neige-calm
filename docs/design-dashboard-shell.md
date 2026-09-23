# Native Report Shell

## Boundary

The app derives `ReportPresentation` from the report layer's declared block
kind. A `view` block selects `dashboard`, including an unreadable `view` whose
declaration survives parsing. Everything else defaults to `document`. Titles,
plugin identifiers, payload fields, and row data never select a page layout.
No persisted schema, projection contract, or global CSS changes.

## Presentation

TrackPage accepts an explicit `reportPresentation` with a document default.
Dashboards start with the desktop inventory collapsed. A header columns icon
with a tooltip opens the ordinary Cards, Tasks, backlinks and Conversations
panel. Its mounted nodes and disclosure state are retained when collapsed.
An explicit choice wins over report refreshes and is scoped to this track mount.

The app omits the dashboard's desktop outline rail; mobile Outline remains in
the existing navigation menu. TrackPage sets its own document start to zero,
giving native rows the full available width while prose keeps its normal
reading measure. Report content is neither filtered nor hidden.
The dashboard uses the existing theme's card surface as an unframed page canvas,
including its sticky header; no global palette or financial styling is added.

Planner continues to use the existing conversation drawer and reserved column.
The report stays mounted through panel, conversation and board changes. Mobile
panels remain URL-owned navigation destinations; the desktop toggle does not
modify those URLs. Notifications remain outside the collapsible inventory.

## Acceptance

- At 1440px, a declared native view gets at least 1080px with the ordinary rail.
- Empty inventory consumes no desktop column; its explicit opener is accessible.
- Panel disclosure, report selection, and report node identity survive round trips.
- Planner does not cover the native report; board and mobile navigation still work.
- Ordinary document tracks retain their default panel and bounded prose.
- Tests use production routes/components with in-memory transport, never services.
