# `features/settings`

Workspace settings. Every surface here is presentational: it never calls an API.
The bag, the in-flight flag, the error strings and the callbacks all arrive as
props; the router owns fetching and mutating.

Four surfaces, four routes:

| Route | Component | File |
| --- | --- | --- |
| `/settings` | `SettingsPage` | `public.tsx` |
| `/settings/templates` | `TemplateListPage` | `templates.tsx` |
| `/settings/templates/$templateId` | `TemplateEditorPage` | `templates.tsx` |
| `/settings/plugins` | `PluginsPane` | `plugins.tsx` |

Real routes rather than page-local state: Back leaves the template editor
instead of leaving Settings, and one template's editor is a URL.

## The frame: `SettingsSurface`

All four render inside `SettingsSurface` — a nav column naming the three groups
(General / Templates / Plugins) beside the pane for the current one. The nav
rows **navigate**: each is a route, so the column is `<nav>` + `<button>`s
carrying `aria-current="page"`, never tabs holding page state.

The app layer puts that frame inside `ui/dialog`, so Settings is an overlay you
step into from wherever you were and leave with Escape, the `×`, or the scrim.
Neither the frame nor any pane renders a page header or a breadcrumb: the dialog
supplies the title and the one close affordance, and the nav column already
names the group. The template *editor* keeps a back control, because it is a
level below the list that the nav column cannot express.

## Plugins (`plugins.tsx`)

The installed list from `GET /api/plugins`, with one write: the enable/disable
switch. Enabled is a *state of the plugin*, so it is a switch and not a pair of
buttons whose label inverts what they report.

`enabled` and `state` are both on the row because they answer different
questions and routinely disagree — `enabled` is what the operator asked for and
the kernel persisted, `state` is what the supervisor achieved. Enabled +
`crashed`, or enabled + `unavailable` (a connector whose upstream never
answered, and nothing will retry it), is the case a reader opens this screen
for. `unavailable` is painted **warning, not error**: it is a connector's normal
terminal state, and red would blame the kernel for an upstream's silence.

Install, uninstall, config editing, log tailing and token rotation are
deliberately **not** here. Each is its own screen with its own failure modes,
and the kernel routes for them (`routes/plugins.rs`) exist whenever they are
picked up.

## Built from `@astryxdesign/core`

`SegmentedControl`, `TextInput`, `Button`, `Banner`, `Heading`, `MetadataList`,
`VStack`/`HStack`. The CSS module is down to the page frame, the form measure,
the saved-notice colour and the template list rows — the old field/label/input/
action-row/segmented-control/About rules were **deleted, not commented out**,
because astryx owns that styling now.

Three places astryx does not fit, each with the reasoning at its call site:

- **`Button` renders a native `disabled`** unless the button is interruptible,
  which would break CR-6 (below). Saves pass `isLoading` + `isInterruptible`.
- **Every `Button` renders an unconditional `role="status"` live region**, so
  `getByRole('status')` can never be unique on a page with buttons. The
  `Saved.` assertion finds the node by text and then checks its role.
- **`Spinner` calls `window.matchMedia` unguarded** and jsdom has none. Stubbed
  per test file, never globally: `app/theme` deliberately branches on
  `matchMedia` being absent, and a global polyfill would hide that path.

## Sections

- **Network** — HTTP / HTTPS proxy inputs seeded from the settings bag.
- **Appearance** — a `SegmentedControl` labelled `Appearance`, Light / Dark /
  System, reporting through `onThemeModeChange`.
- **About** — build-time `version` / `build` in a `MetadataList`.

## `null` clears a key (INV-SETTINGS-001)

A field the user blanked is sent as `null`, **never `''`**. `core/domain/settings`
notes that the kernel deletes a key for either value, so the two converge on the
same state — sending `null` states the intent instead of relying on that
equivalence, and keeps the wire shape honest if the kernel ever stops treating
`''` as a delete.

A key the user did not touch is **absent** from the patch. A save therefore never
rewrites a value nobody edited, so two tabs editing different keys cannot clobber
each other.

Re-seeding compares the incoming bag **by value**, not by object identity: a
parent that hands back a fresh object every render must not wipe out what the
user is typing. A genuine server-side change does re-seed. The template editor
does the same thing with a serialized comparison, and seeds unconditionally on
first sight — otherwise arriving with a save already in flight would leave the
draft empty and the editor stuck on "Loading…".

## Save is busy, not disabled (CR-6)

While a save is in flight the button is **busy**, not `disabled`. Focus is on it
at exactly that moment, and a native `disabled` element is not focusable, so
disabling it would throw focus to `<body>` mid-action. The re-entry guard is in
`onClick`, where it can be read.

## Appearance is deliberately not server-persisted

Theme is a per-device preference, so it never goes through `onSave` and never
enters the settings bag. `ThemeMode` is declared here rather than imported from
`app/theme` because `features/**` may not import `app/**`; the app layer adapts
to this union at the router seam.

## Loading is not an empty form (INV-SETTINGS-002)

While `settings === undefined` the Network section renders a loading line and
**no text input at all**. An empty form would let the user save blanks over real
values before the bag has landed. The template list and editor hold the same
rule for their own reads.

## The template editor's ceiling

**No rename, no delete, no reorder** — and that is a product limit, not an
oversight. A template's tasks live in the template wave's report, so
`wave_report_edit_guard` (#1179) governs them: a task `key` is immutable for the
life of its block, and a live task may only leave a document as a tombstone that
`prepare_fork_report` then copies into every wave forked afterwards. Both come
back from the server as a 400, so the affordances are absent and the reason is
printed on the page instead of discovered by failing. The full argument is in
`crates/calm-server/src/routes/wave_templates.rs`.

**Sections are not editable here and never will be.** The four report sections
(概要 / 待你定 / 已完成 / 决策) come from `CONTRACT_SECTION_RULES`, which every
wave report shares — template-forked or not. They are not a per-template fact,
and a "sections" control on this screen would say it edits one template while
editing every wave. Making them configurable is its own issue.

**Whole task objects round-trip.** The editor reads `key` and `goal`; every other
field is opaque cargo handed back untouched, because the server stores exactly
what it is given.

## Accessibility contract

- Errors render in `role="alert"`; `savedAt` drives a transient `Saved.` in
  `role="status"`.
- Each template's Edit button is named after its template — three buttons all
  called "Edit" is a list a screen reader cannot navigate.
- **Intentionally not done:** no `<a href>` anywhere (INV-A11Y-061).

## Test contract

`getByRole` / `getByLabelText` only — never a CSS class selector.
`public.test.tsx` and `templates.test.tsx` hold behavior;
`public.contract.test.tsx` holds invariants.
