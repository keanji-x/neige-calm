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
`tools/styles/repository-check.mjs`). Every desktop group composes `SidebarGroup`; groups with Tracks also use
`SidebarTrackGroup` for the same row renderer, active-Track retention and
`Show N more` control. `AreaGroup` supplies only Area membership and actions. Desktop Area colour arrives as inline `style`
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
its selection action. Desktop keeps its original rename control and closed
badge; on the phone the Track rows say Closed.

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

- The rail is a `<nav aria-label="Workspace">`; each section has an `<h2>`
  containing its disclosure button. All groups have an accessible group label.
  Waiting on you, Pinned, Unread, Running and Areas share the same disclosure;
  Area rows share its geometry and retain their own actions.
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
- An expanded desktop Area lists its five most recent Tracks plus the open one,
  which keeps its sorted position. When more are left out, one button in a fixed
  slot after the rows reads `Show N more` / `Show less`; its name adds the Area
  (`Show N more in <area>`) so several of them stay distinguishable, and it has
  no `aria-expanded` because its text already flips. Focus stays on it, and
  `Show less` scrolls it into view. The choice is component memory: collapsing
  the Area keeps it; collapsing the rail or reloading resets it.
- Before that limit, the desktop Area leaves out closed Tracks (`railAreaTracks`),
  except one that is unread or open in the view, so `Show N more` never counts a
  hidden closed Track. The Area actions menu's `Show closed` / `Hide closed` item
  lists every Track again. Shortcut groups and the phone Area page still list closed Tracks. All desktop
  Track groups share the five-row limit, active-Track retention and reveal control.
  Only the canonical Area row carries `aria-current`; shortcuts retain the active
  Track past the limit without introducing duplicate current-page markers.
- **Intentionally not done:** no skip-to-main link (INV-A11Y-058). The rail is
  short and this has never been raised as a pain point; re-evaluate if a second
  long section lands. "There is no skip link" is a decision, not a defect.
- **Intentionally not done:** no `<a href>` navigation (INV-A11Y-061).

## Persistence

Every sidebar group has the same Move up, Move down and Hide group actions.
Movement swaps visible siblings at the same level; boundary actions are disabled.
Area editing, closed-Track display and deletion remain Area-owned menu entries.
The top-row menu provides Hidden groups with Show actions for every hidden
workspace group, including Pinned, Waiting on you, Areas, Unread and
Running (initially off). Restoring an Area also reveals and expands its Areas
parent. Area entries say `Show area <name>` to distinguish same-named workspace
groups. Hiding a group returns focus to the top-row menu and never navigates,
marks work read or changes running work.

The full registered order retains hidden slots, removes stale/duplicate IDs and
appends newly created Areas. Layout is a browser-local per-origin/per-user choice
that survives server restarts; it never writes the shared Area sort value. Hidden
Area choices also apply to the collapsed strip. Shortcut membership still uses
all user-visible Areas, so hiding an Area's tree does not hide its work from Pinned,
Unread or Running. Unread uses completion receipts; Running independently reads
the kernel working verdict. Empty shortcut groups remain omitted. Waiting on you,
Pinned and Areas start visible and can also be hidden or moved.

Area disclosure, each Area's `Show closed` choice (`area-closed:<id>`, off by
default) and the manual sidebar width choice are browser-local display
preferences, injected through `app/providers/ui-preferences.tsx`. Explicit
collapse survives Track navigation and refresh. Activating an Area initial
still expands that Area and restores focus to its disclosure. With unavailable
storage the choices remain in memory for the app instance.

## Test contract

`getByRole`. `sidebar.contract.test.tsx` locks the invariants, `sidebar.test.tsx`
the behaviour; every invariant below was mutation-verified (break the production
line, watch the named test go red) before landing.

- **INV-SIDEBAR-007** — default sections render **Waiting on you → Pinned → Areas**
  (enabled Unread and Running appear between Pinned and Areas), and
  **pinning is not relocation**: a pinned track appears under Pinned *and* in its
  area's list, and if it also needs attention it appears in all three.
- **E2E-INV-SHELL-003** — the kernel system area must never reach the rail. The
  server filters it, `areaListQueryOptions` filters it again, and `Sidebar`
  filters it a third time with `visibleAreas`.
- **INV-SIDEBAR-012** — sidebar pin actions are hidden at rest on desktop,
  including pinned Tracks repeated across groups. Hover or keyboard focus reveals
  them; unpinned actions point up and pinned actions point down to unpin. Touch
  displays both actions directly because it has no hover. The controls remain in
  the focus order and carry their `aria-pressed` state. Other TrackRow variants
  retain the persistent pinned mark. Browser checks cover the sidebar's actual
  opacity, glyph direction and hover/focus behavior.
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

- Area drag-reorder is not in the rail; per-group menus provide personal sibling movement. Edit from the row actions menu opens one
  Dialog for name, default template, and default folder. Delete lives in the
  same menu and still uses typed confirmation.
- The AppShell Area-editor flow is the sole consumer of `AREA_PALETTE`; it picks
  a colour at random and sends it to the kernel (INV-DUP-006). Hosting it above
  desktop Sidebar and mobile Areas makes both trigger the same Dialog and state.

## Responsive Settings ownership

Settings remains one shell-owned route surface. On a phone `/settings` is a
vertical category index built from Astryx List rows; the pane stays hidden from
paint and the accessibility tree. Each category has its own URL, including
`/settings/network`, and shows just its content beneath one page header. The
header's Back returns to the index; the index's Back returns to the workspace.
Desktop `/settings` opens Network beside the existing SideNav.

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
