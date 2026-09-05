# Design QA — Track lifecycle recovery

## Review basis

- Product source of truth: the existing Next Track page, its shared design tokens, and its production `PageHeader`, `MobileHeader`, `MoreMenu`, and workspace-rail components.
- Desktop verification: real Chromium at `1200 × 800`, expanded workspace rail, Track lifecycle `Done`, with Track actions both closed and open.
- Mobile verification: real Chromium at `390 × 844`, Track root page with lifecycle `Working` and compact Track actions.
- Automated evidence:
  - `fe/web/src/features/track/page/header-lifecycle.browser.test.tsx`
  - `fe/web/src/app/shell/mobile.browser.test.tsx`
  - `fe/web/src/app/router/track-lifecycle-resume.test.tsx`
  - `fe/e2e/track-lifecycle-resume.spec.ts`

No developer-local screenshot path is part of the review contract. The browser tests above carry the durable geometry, visibility, focus, and interaction assertions.

## Accepted presentation

- Every lifecycle is rendered as inert status text beside the Track title. It has no background, border, chevron, or click behavior.
- The status uses the shared lifecycle phrase and three existing tone ranks: attention, running, and neutral.
- Desktop Track mutations live behind the far-right three-dot action. A recoverable Track shows `Resume work`, a divider, and `Delete track`; a non-recoverable Track shows only `Delete track`.
- The three-dot action keeps its accessible name but has no redundant visible tooltip. Its resting ink uses the secondary text rank and darkens on hover, focus, and open state.
- Mobile renders the same lifecycle fact in the centered title group and exposes recovery from compact Track actions.
- The expanded workspace entry says `Today` in semibold text while preserving the Neige mark and its navigation behavior.

## Desktop findings

- The lifecycle text shares the title cluster's vertical center and remains attached to the title with the existing 4–12px gap contract.
- A long title truncates before either the lifecycle text or the trailing Track action can overlap.
- The 24px lifecycle line box is visually subordinate to the 18px title while remaining readable.
- The right-side action replaces the ambiguous destructive cross; deletion is expressed inside the menu and still requires the shared confirmation dialog.
- The open menu aligns to the three-dot trigger and does not change document or panel geometry.

## Mobile findings

- The visible `MobileHeader` carries both the Track title and lifecycle; the desktop header remains hidden at the compact breakpoint.
- The title group stays centered between the 44px Back and Track-actions targets.
- The status remains text-only and does not create a second header row.
- Recovery and deletion remain reachable from the existing compact Track-actions menu.

## Accessibility and interaction

- Lifecycle uses `role="status"` with the accessible name `Track lifecycle: <label>`.
- The status itself is not focusable because it performs no action.
- Escape and outside click close Track actions. Focus returns to the current three-dot trigger, including after `Resume work` removes itself from the menu.
- A successful Working PATCH is written through to the Track-detail cache before the best-effort refetch. A failed refetch therefore cannot leave stale Done text or a permanently disabled Resume action.
- The server-derived `can_resume` includes lifecycle permission, child-track integrity, and the retired area-chat authority fence, so the UI does not advertise an action the route must refuse.
- Running lifecycle text meets 4.5:1 contrast in both themes; the full token contrast gate remains green.

## Implementation checklist

- [x] All lifecycle values are visible on desktop and mobile.
- [x] Lifecycle is a non-interactive fact; edits live in Track actions.
- [x] Only server-authorized Tracks expose `Resume work`.
- [x] Desktop and compact access paths use the same mutation.
- [x] Delete remains behind the shared destructive confirmation.
- [x] Tooltip, focus, Escape, outside-click, color, spacing, truncation, and responsive visibility are covered.
- [x] The document contains no machine-local evidence links.

Final result: passed.
