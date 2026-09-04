# Design QA — Track lifecycle Resume work

## Evidence

- Source visual truth: `/home/kenji/.codex/visualizations/2026/09/04/01a06d1e-6d25-7503-b36f-825ecead8179/neige-next-lifecycle-resume-source-normalized.png`
- Rendered implementation: `/home/kenji/.codex/visualizations/2026/09/04/01a06d1e-6d25-7503-b36f-825ecead8179/neige-next-lifecycle-resume-implementation-final.png`
- Full comparison: `/home/kenji/.codex/visualizations/2026/09/04/01a06d1e-6d25-7503-b36f-825ecead8179/neige-next-lifecycle-resume-compare-final.png`
- Focused header comparison: `/home/kenji/.codex/visualizations/2026/09/04/01a06d1e-6d25-7503-b36f-825ecead8179/neige-next-lifecycle-resume-header-compare-final.png`
- Viewport and CSS size: `1440 × 1024`.
- Device scale factor: `1`.
- Source pixels: `1487 × 1058`, normalized to `1440 × 1024`; the source and target aspect ratios differ by less than 0.1%.
- Implementation pixels: `1440 × 1024`.
- State: light theme, expanded workspace rail, Track lifecycle `Done`, lifecycle menu open, one `Resume work` action.

The source mock predates the final product decision. Its extra current-state heading and Planner attribution are intentionally absent: the user narrowed the menu to exactly one lifecycle action, `Resume work`. The shallow status button, placement, surface, radius, tone, and anchored-menu interaction remain the selected visual target.

## Findings

- No actionable P0, P1, or P2 mismatch remains.
- The final menu is deliberately shorter than the source mock because it contains only the approved action and its one-line consequence.
- The final status button is slightly quieter than the generated mock and uses the real Next tokens rather than image-inferred colors. This is the intended implementation of the user request: `--surface-card`, `--radius-md`, no border, and lifecycle-tone text.

## Full-view comparison

- The left workspace rail, page header, document column, outline, and right panel retain the current Next proportions and hierarchy.
- The lifecycle control stays attached to the Track title and does not create another header row or displace the right panel.
- The menu opens below the status control without covering the delete action or changing document geometry.

## Focused comparison

- The status control is a compact, shallow button beside the title, with a small chevron only when `Resume work` is available.
- `Resume work` and `Set this track back to Working.` are legible at the compact menu density.
- The ordinary title `Planner lifecycle recovery` renders in full. Long titles retain the existing ellipsis behavior before reaching the status button or delete action.

## Required fidelity surfaces

- Fonts and typography: existing Next sans/serif tokens are unchanged; the status control is browser-verified at 11px and preserves the lifecycle tone hierarchy.
- Spacing and layout rhythm: browser tests pin a 4–12px title/control gap, no ordinary-title clipping, and no overlap with trailing actions. The status button and side-panel card resolve to the same radius.
- Colors and visual tokens: the status button and side-panel card resolve to the same painted background; the button has a zero-width border. Running text contrast is at least 4.5:1 against the button surface in light and dark themes.
- Image quality and asset fidelity: no raster, logo, illustration, or custom icon asset was introduced. The existing Astryx chevron is used.
- Copy and content: the lifecycle label comes from the shared domain vocabulary. The menu exposes exactly `Resume work` with the consequence `Set this track back to Working.`

## Interaction and browser evidence

- Opening the lifecycle control exposes one menu item.
- Activating `Resume work` updates the real API-backed Track from a terminal state to `Working` and clears `terminal_at`.
- The action is driven by the server-derived `can_resume` capability, so a terminal child track whose parent verdict cannot be rolled back keeps the status visible without advertising a failing action.
- Escape closes the menu and restores focus to the lifecycle button; an outside click light-dismisses it. Both are covered in real Chromium.
- While Resume is pending, both desktop and compact actions are disabled and duplicate requests are deduplicated. After keyboard Resume succeeds, focus moves to the stable lifecycle fact with a visible focus ring instead of falling back to the document body.
- Compact Track actions expose the same `Resume work` callback.
- Browser console inspection found no errors. One development-only WebSocket warning was recorded during an explicit page reload because the old connection closed before the replacement connected; the subsequent lifecycle events and Resume action completed normally.

## Comparison history

1. Round 1 found a P1 title-clipping regression: an ordinary title rendered as `Planner lifecycle recov…` despite abundant row space.
2. The title control intrinsic width was corrected and a real-browser regression added.
3. The final capture shows the full title, unchanged page geometry, matching status/card surface and radius, and no remaining P0/P1/P2 difference.

## Implementation checklist

- [x] All lifecycle values remain visible beside the title.
- [x] Only recoverable states expose `Resume work`.
- [x] Non-recoverable status buttons do not advertise a popup.
- [x] Desktop and compact access paths reach the same mutation.
- [x] Focus, Escape, outside-click, color, radius, border, spacing, and truncation are verified in Chromium.

final result: passed
