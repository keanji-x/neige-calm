# `app/shell`

The layout every route renders inside: the workspace rail plus the matched
route's outlet.

`AppShell` owns the workspace read (`useWorkspace`) **and** the area/track
mutations, and hands `Sidebar` plain callbacks — the rail stays presentational,
so a jsdom test drives it without a `QueryClient`.

It no longer owns a New track dialog (#1211): starting a track is the route
`/area/{id}/new`, owned by `app/router`, and every Area group's permanent `+`
navigates there. `onOpenSettings` / `onSignOut` are injected: the shell never
signs out itself. `nowMs` exists so a test can pin the `pinned_at` stamp.

## Visual contract

`shell.module.css`, `@layer features` — app composition sits at the same cascade
position as the features it wraps (see the comment in
`tools/styles/repository-check.mjs`). Desktop Area colour arrives as inline `style`
because it is per-row data. Phone Area rows use a monochrome folder icon;
Track rows and Track-switcher choices use the matching file icon. The running pulse is a token-timed animation
(`--motion-pulse`) with a `prefers-reduced-motion` opt-out.

## Phone layout

Below the shared compact breakpoint the shell renders the same `MobileHeader`
frame as the secondary pages: 56px plus the top safe area, 16px inline insets,
44px controls, 20px principal glyphs, and 16px medium titles. The main Track
header names the current Track; new-track pages retain the Area selector.

The left button opens an Areas index. Each row has a monochrome folder icon,
its name, and a chevron at the far right; there are no colour dots or Track
counts. Choosing an Area opens its separate Track list without changing the
underlying route. Back returns to Areas, then to the original page and opener.
The Areas index offers Settings and New area; its Track page offers Settings
and New track, scoped to that Area. Area editing stays in the header's
three-dot menu.

Areas and Tracks have stable, full-viewport page shells with independent scroll
positions. Their header, action group and list switch immediately on forward/back.
Entering Tracks starts at the top, and returning restores the Areas list position.
The inactive page is unpainted, inert and hidden from accessibility and the focus
cycle. No animation completion or remount controls navigation, so a quick
Back/forward cannot close a newer page.

Navigation and the Settings category index share borderless, rounded
`MobileListGroup` surfaces. Native Lists keep their dividers disabled and
preserve rounded hover/selected rows.

The main Track title is a selector for visible Tracks in the current Track's
exact Area. Shell owns the shared workspace read, while the route supplies its
known `track.areaId`; loading and errors never borrow another Area's choices.
Edit track and Delete track share the final group in the three-dot menu;
execution actions appear in the preceding group. A feature-owned read-view
adapter registers the original EditableTitle's begin-edit action and clears it
when the read view leaves; the item is omitted while that editor is already
open. The menu closes before focusing the input, and the title itself keeps
its selection action. Desktop keeps its original rename control and lifecycle
badge; phone lifecycle information remains on the Track rows.

TrackPage owns a stable title container and preserves the editor, draft,
selection and focus across viewport changes. Only synchronous relocation blur
is suppressed; ordinary blur, Enter and Escape retain their existing behavior.
The custom read view also keeps Enter's synthesized-click guard, so committing
cannot accidentally open the selector. Read-mode focus hands off to the new
viewport's corresponding control, while focus outside the title stays outside.
No duplicate mobile title is placed above the report.

The two app headers compose `MobileHeader` with leading/title/action slots;
Cards, Conversations, Chat, Source and Settings use its standard title/Back
slots. Hosts keep content padding separate from the header's own insets. A
marked module title stays a plain heading; custom title content cannot carry
`titleFieldMarker`. Geometry and actual title font/weight are checked through
production AppShell routes at 320px and 390px, alongside rename no-write resize
and normal user-commit checks.

The right slot belongs to the matched route. `MobileHeaderActionsContext` carries
its DOM host to the app router, which passes `mobileHeaderActionsHost` to
`TrackPage`. That feature portals its existing menu into the phone header and
continues owning the menu actions, panel navigation callbacks and return focus.
At desktop widths the host is absent and the existing desktop presentation wins.
A new-track draft shows the short Cards/Conversations labels; both entries
remain disabled until a Track has been created.

`responsive.contract.test.tsx` and `mobile-report-navigation.test.tsx` exercise
real shell composition, Area routing, modal focus boundaries and panel history.
`mobile.browser.test.tsx` checks real menu geometry and portal cleanup on resize.

## Accessibility contract

- The rail is a `<nav aria-label="Workspace">`; each section has an `<h2>`.
- Track rows are `<button>` with `aria-current="page"` when their URL is open.
- On the desktop rail, an Area is a muted disclosure button with `aria-expanded` and no page URL.
  Click, Enter, Space and assistive activation all toggle it immediately;
  editing belongs to the permanently visible actions menu. Controlled open
  state comes from Astryx `useCollapsible`; the product-specific row DOM keeps
  `…` and `+` as non-nested sibling controls.
- The chevron is decorative inside that button. The permanent
  new-Track button and permanent Area actions menu are siblings, so no
  interactive element is nested inside another.
- The Area actions menu is permanently visible with every pointer type.
  Activating an Area initial in the collapsed rail focuses and scrolls to the
  disclosure revealed by expansion.
- **Intentionally not done:** no skip-to-main link (INV-A11Y-058). The rail is
  short and this has never been raised as a pain point; re-evaluate if a second
  long section lands. "There is no skip link" is a decision, not a defect.
- **Intentionally not done:** no `<a href>` navigation (INV-A11Y-061).

## Persistence

Area disclosure and the manual sidebar width choice are browser-local display
preferences, injected through `app/providers/ui-preferences.tsx`. Explicit
collapse survives Track navigation and refresh. Activating an Area initial
still expands that Area and restores focus to its disclosure. With unavailable
storage the choices remain in memory for the app instance.

## Test contract

`getByRole`. `sidebar.contract.test.tsx` locks the invariants, `sidebar.test.tsx`
the behaviour; every invariant below was mutation-verified (break the production
line, watch the named test go red) before landing.

- **INV-SIDEBAR-007** — sections render **Waiting on you → Pinned → Areas**, and
  **pinning is not relocation**: a pinned track appears under Pinned *and* in its
  area's list, and if it also needs attention it appears in all three.
- **E2E-INV-SHELL-003** — the kernel system area must never reach the rail. The
  server filters it, `areaListQueryOptions` filters it again, and `Sidebar`
  filters it a third time with `visibleAreas`.
- **INV-SIDEBAR-012** — the pin button is hover-revealed while a track is
  unpinned and permanently visible once it is pinned (touch has no hover, so a
  hover-only unpin would be unreachable). The reveal itself is CSS in
  `features/track/row/row.module.css`: jsdom does not apply CSS Modules, so the
  contract test proves only that the control is in the accessibility tree with
  its `aria-pressed` state in both cases. **The visual half is a `browser`-tier
  concern and is not covered here.**
- **INV-SIDEBAR-013** — every area row carries a **permanently visible** `+`
  whose accessible name is per-area (`New track in <area>`), plus a `title`; the
  rail has one per area, so a shared `"New track"` name would be N
  indistinguishable controls (§4.4 also forbids the tooltip standing in for the
  name). It sits at the trailing edge with a permanently visible actions menu
  one control-step inboard, and `.areaRow` reserves both gutters, so neither
  control moves on hover. Both marks are stroked `ui/icon` glyphs, not literal
  characters — an icon box with bare text is a source-contract violation. The
  collapsed strip gets no `+`: one glyph per area, and that glyph is the area.
  Their visual permanence and alignment are CSS and `browser`-tier; jsdom pins
  the names and that the two controls do not share a class.
- **INV-CONFIRM-001** — both destructive confirms always keep Cancel enabled.
  Closing during the await aborts the request, dismisses its owning dialog and
  releases pending immediately.

## Deliberate gaps

- Area drag-reorder is not in the rail. Edit from the row actions menu opens one
  Dialog for name, default template, and default folder. Delete lives in the
  same menu and still uses typed confirmation.
- The AppShell Area-editor flow is the sole consumer of `AREA_PALETTE`; it picks
  a colour at random and sends it to the kernel (INV-DUP-006). Hosting it above
  desktop Sidebar and mobile Areas makes both trigger the same Dialog and state.

## Responsive Settings ownership

Settings remains one shell-owned route surface. On a phone `/settings` is a
vertical category index built from Astryx List rows; the pane stays hidden from
paint and the accessibility tree. Each category has its own URL, including
`/settings/general`, and shows just its content beneath one page header. The
header's Back returns to the index; the index's Back returns to the workspace.
Desktop `/settings` still opens General beside the existing SideNav.

The shell records the workspace and index history entries it actually observed.
Category selection pushes one detail entry; Back pops to the observed index,
including section pushes made on desktop before resizing. Cold category links
replace themselves with the index, and a cold index uses a safe in-app
workspace replacement. Repeated visits never add duplicate workspace entries.

The pane subtree is a portal into one stable app-owned DOM container. Ref
attachments move that container between the inline page and the existing
desktop Dialog slot when the viewport changes. This preserves plugin config
and install drafts without copying them or giving the generic Dialog page
semantics. Current Settings panes do not consume Dialog child-view context;
a future consumer must explicitly bridge that context rather than infer it
from DOM ancestry. The real browser test verifies DOM identity, retained draft,
desktop forward/reverse Tab wrapping and mobile removal of inert/modal state.

## Mobile secondary surfaces

Cards, Conversations and Drawer pages use the same 16px title and 44px Back
control in the shared flat mobile header. Mobile chat remains a full viewport
page while its composer and message surfaces keep the 16px card radius.

Mobile Cards, Conversations, Chat and Source pages open and close immediately.
Returning from Planner exposes Conversations without a temporary Report; the
route still marks any obscured panel inert and aria-hidden while a foreground
Drawer is open. Compact Drawer closing removes its retained frame in the same
commit, including when resizing during a desktop exit, and restores focus to
its opener. Desktop Drawer entrance/exit and focus behavior remain unchanged.
Control feedback and progress animations are unaffected.
