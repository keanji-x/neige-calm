# `app/shell`

The layout every route renders inside: the workspace rail plus the matched
route's outlet.

`AppShell` owns the workspace read (`useWorkspace`) **and** the area/wave
mutations, and hands `Sidebar` plain callbacks — the rail stays presentational,
so a jsdom test drives it without a `QueryClient`.

It no longer owns a New wave dialog (#1211): starting a wave is the route
`/area/{id}/new`, owned by `app/router`, and the two `+` surfaces — every area
row's in the rail, and the area page's WAVES module head — both just navigate.
What the shell kept is the seam. The rail gets the opener as a prop
(`onNewWave`); the route gets it through `useRequestNewWave()`, the one context
this module publishes, because there is no prop path across `<Outlet />`. `onOpenSettings` /
`onSignOut` are injected: the shell never signs out itself. `nowMs` exists so a
test can pin the `pinned_at` stamp.

## The wave-create body (#1131, #1147 S3, #1211)

The create moved to `app/router`'s `NewWaveRoute` with the page (#1211); this
section stays because the rail is still one of the two `+` surfaces. The
new-wave page's Folder chip is optional and decides the request shape — and the
body carries **no `title`** since #1211: the kernel stores the empty string and
the spec agent names the wave through `calm.wave.rename`.

| Folder | `POST /api/waves` body | Kernel branch |
| --- | --- | --- |
| not chosen | `{ area_id, theme }` | *managed* — the kernel derives, creates and owns a workspace under the workspace root |
| chosen | `… + { cwd, attach_folder: true }` | *attached* — the user's own directory, never created, moved or deleted by the kernel |

Both keys travel together, and absence is the signal — `cwd: null` or
`attach_folder: false` are different kernel paths, not equivalents. `true`
rather than a pre-flight `GET /api/areas/resolve`: with the flag omitted the
kernel refuses any path no area has already claimed, and `true` is a no-op when
this area already covers the path (`routes/waves.rs`, same-area arm), so a
second wave in the same repository does not conflict with the first.

The failure that branch can produce is a **structured 409** (`FolderConflict`)
with no `error` key, which the generic normaliser can only report as the bare
word "Conflict". The shell decodes it (`folderConflictOf` +
`folderConflictMessage`) and names the path, the owning area, and the remedy.
The area *name* comes from `useWorkspace`, which is the second reason this lives
here rather than in the form.

The picker's `listDirectory` port is created here too
(`app/providers/directory.ts`): `ui/directory-browser` must not know a transport
exists, and `features/**` may not import `app/**`, so the composition layer is
the only place that can bind them.

## Visual contract

`shell.module.css`, `@layer features` — app composition sits at the same cascade
position as the features it wraps (see the comment in
`tools/styles/repository-check.mjs`). Area colour arrives as inline `style`
because it is per-row data. The running pulse is a token-timed animation
(`--motion-pulse`) with a `prefers-reduced-motion` opt-out.

## Accessibility contract

- The rail is a `<nav aria-label="Workspace">`; each section has an `<h2>`.
- Rows are `<button>` with `aria-current="page"` when their URL is open.
- The area count badge is `aria-hidden`: the row's accessible name already
  carries the area name, and a bare number read after it is noise.
- The chevron is a **sibling** of the area row, not a child — nesting
  interactive elements is invalid HTML (`nested-interactive`). Same for the
  per-area delete `×` and for the row's pin/delete (owned by `WaveRow`).
- **Intentionally not done:** no skip-to-main link (INV-A11Y-058). The rail is
  short and this has never been raised as a pain point; re-evaluate if a second
  long section lands. "There is no skip link" is a decision, not a defect.
- **Intentionally not done:** no `<a href>` navigation (INV-A11Y-061).

## Persistence

**Nothing in the rail is persisted, deliberately.** Collapse state and per-area
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

- **INV-SIDEBAR-007** — sections render **Waiting on you → Pinned → Areas**, and
  **pinning is not relocation**: a pinned wave appears under Pinned *and* in its
  area's list, and if it also needs attention it appears in all three.
- **E2E-INV-SHELL-003** — the kernel system area must never reach the rail. The
  server filters it, `areaListQueryOptions` filters it again, and `Sidebar`
  filters it a third time with `visibleAreas`.
- **INV-SIDEBAR-012** — the pin button is hover-revealed while a wave is
  unpinned and permanently visible once it is pinned (touch has no hover, so a
  hover-only unpin would be unreachable). The reveal itself is CSS in
  `features/wave/row/row.module.css`: jsdom does not apply CSS Modules, so the
  contract test proves only that the control is in the accessibility tree with
  its `aria-pressed` state in both cases. **The visual half is a `browser`-tier
  concern and is not covered here.**
- **INV-SIDEBAR-013** — every area row carries a **permanently visible** `+`
  whose accessible name is per-area (`New wave in <area>`), plus a `title`; the
  rail has one per area, so a shared `"New wave"` name would be N
  indistinguishable controls (§4.4 also forbids the tooltip standing in for the
  name). It sits at the trailing edge with the hover-revealed `×` one
  control-step inboard, and `.areaRow` reserves both gutters at rest, so neither
  control moves on hover. Both marks are stroked `ui/icon` glyphs, not literal
  characters — an icon box with bare text is a source-contract violation. The
  collapsed strip gets no `+`: one glyph per area, and that glyph is the area.
  As with INV-SIDEBAR-012 the *visual* reveal is CSS and `browser`-tier; jsdom
  pins the names and that the two controls do not share a class.
- **INV-CONFIRM-001** — both destructive confirms always keep Cancel enabled.
  Closing during the await aborts the request, dismisses its owning dialog and
  releases pending immediately.

## Deliberate gaps

- Area rename and drag-reorder are not in the rail; renaming lives on the area
  page (`features/area/page`).
- The sidebar's new-area flow is the sole consumer of `AREA_PALETTE`; it picks
  a colour at random and sends it to the kernel (INV-DUP-006).
