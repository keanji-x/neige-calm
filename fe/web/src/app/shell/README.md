# `app/shell`

The layout every route renders inside: the workspace rail plus the matched
route's outlet.

`AppShell` owns the workspace read (`useWorkspace`) **and** the cove/wave
mutations, and hands `Sidebar` plain callbacks — the rail stays presentational,
so a jsdom test drives it without a `QueryClient`.

It also owns the **New wave dialog**, for the same reason: two surfaces open it
— every cove row's `+` in the rail, and the cove page's WAVES module head — and
the rail is a sibling of the outlet, so a dialog owned by the cove route was
reachable from exactly one of them. The rail gets the opener as a prop
(`onNewWave`); the route gets it through `useRequestNewWave()`, the one context
this module publishes, because there is no prop path across `<Outlet />`. The
dialog reads `GET /api/coves/{id}/folders` for whichever cove its select shows —
see `features/cove/README.md` for the shape that read decides. `onOpenSettings` /
`onSignOut` are injected: the shell never signs out itself. `nowMs` exists so a
test can pin the `pinned_at` stamp.

## Visual contract

`shell.module.css`, `@layer features` — app composition sits at the same cascade
position as the features it wraps (see the comment in
`tools/styles/repository-check.mjs`). Cove colour arrives as inline `style`
because it is per-row data. The running pulse is a token-timed animation
(`--motion-pulse`) with a `prefers-reduced-motion` opt-out.

## Accessibility contract

- The rail is a `<nav aria-label="Workspace">`; each section has an `<h2>`.
- Rows are `<button>` with `aria-current="page"` when their URL is open.
- The cove count badge is `aria-hidden`: the row's accessible name already
  carries the cove name, and a bare number read after it is noise.
- The chevron is a **sibling** of the cove row, not a child — nesting
  interactive elements is invalid HTML (`nested-interactive`). Same for the
  per-cove delete `×` and for the row's pin/delete (owned by `WaveRow`).
- **Intentionally not done:** no skip-to-main link (INV-A11Y-058). The rail is
  short and this has never been raised as a pain point; re-evaluate if a second
  long section lands. "There is no skip link" is a decision, not a defect.
- **Intentionally not done:** no `<a href>` navigation (INV-A11Y-061).

## Persistence

**Nothing in the rail is persisted, deliberately.** Collapse state and per-cove
disclosure live in component state only. `core/keys/storage.ts` is frozen and
holds exactly three keys — `SYNC_CURSOR_KEY`, `DB_INSTANCE_ID_KEY`, `THEME_KEY`
— none of which mean "sidebar layout", and its `StorageAdapterPort` is
`Promise`-based, so it could not seed the first render synchronously anyway.
Inventing a key here would have been a workaround; the rail simply opens in its
default shape each session.

## Test contract

`getByRole`. `sidebar.contract.test.tsx` locks the invariants, `sidebar.test.tsx`
the behaviour; every invariant below was mutation-verified (break the production
line, watch the named test go red) before landing.

- **INV-SIDEBAR-007** — sections render **Waiting on you → Pinned → Coves**, and
  **pinning is not relocation**: a pinned wave appears under Pinned *and* in its
  cove's list, and if it also needs attention it appears in all three.
- **E2E-INV-SHELL-003** — the kernel system cove must never reach the rail. The
  server filters it, `coveListQueryOptions` filters it again, and `Sidebar`
  filters it a third time with `visibleCoves`.
- **INV-SIDEBAR-012** — the pin button is hover-revealed while a wave is
  unpinned and permanently visible once it is pinned (touch has no hover, so a
  hover-only unpin would be unreachable). The reveal itself is CSS in
  `features/wave/row/row.module.css`: jsdom does not apply CSS Modules, so the
  contract test proves only that the control is in the accessibility tree with
  its `aria-pressed` state in both cases. **The visual half is a `browser`-tier
  concern and is not covered here.**
- **INV-SIDEBAR-013** — every cove row carries a **permanently visible** `+`
  whose accessible name is per-cove (`New wave in <cove>`), plus a `title`; the
  rail has one per cove, so a shared `"New wave"` name would be N
  indistinguishable controls (§4.4 also forbids the tooltip standing in for the
  name). It sits at the trailing edge with the hover-revealed `×` one
  control-step inboard, and `.coveRow` reserves both gutters at rest, so neither
  control moves on hover. Both marks are stroked `ui/icon` glyphs, not literal
  characters — an icon box with bare text is a source-contract violation. The
  collapsed strip gets no `+`: one glyph per cove, and that glyph is the cove.
  As with INV-SIDEBAR-012 the *visual* reveal is CSS and `browser`-tier; jsdom
  pins the names and that the two controls do not share a class.
- **INV-CONFIRM-001** — both destructive confirms always keep Cancel enabled.
  Closing during the await aborts the request, dismisses its owning dialog and
  releases pending immediately.

## Deliberate gaps

- Cove rename and drag-reorder are not in the rail; renaming lives on the cove
  page (`features/cove/page`).
- The sidebar's new-cove flow is the sole consumer of `COVE_PALETTE`; it picks
  a colour at random and sends it to the kernel (INV-DUP-006).
