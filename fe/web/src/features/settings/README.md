# `features/settings`

Workspace settings. Every surface here is presentational: it never calls an API.
The bag, the in-flight flag, the error strings and the callbacks all arrive as
props; `app/shell/settings-overlay` owns fetching and mutating.

## The standard

**One nav entry per group. One pane per entry. One row shape.**

A settings group that wants a heading is a nav entry — not a block stacked on a
pane. That is why Network, Appearance and About are three entries and not three
headed sections of one "General" page: a pane holding three headed groups is a
pile, and the reader has to scroll to discover what is in it.

A row is:

```
title                                                            [ control ]
one sentence
```

Left-aligned text, right-aligned control, both flush with the pane's two edges,
a hairline between rows and nothing else. Every control is the same width
(`CONTROL_WIDTH`), so a pane has exactly one trailing edge whatever sits on it —
a text field, a dropdown, a toggle, a chevron.

That row is `SettingRow`, and it is the **only** way to put anything on a pane.
A row is either something you set (`control`) or somewhere you go (`onOpen`),
never both — the props type makes the pair unrepresentable, and astryx's list
guidance rejects an interactive control inside an interactive row for the same
reason: two targets for one intent.

### Hierarchy

Three levels, and only three:

1. the dialog title — `Settings`
2. the pane — a heading plus **one sentence** saying what the group is for
3. the rows

The lede is required by the props. A group that cannot be said in one sentence
is two groups, and the nav column is where the second one goes.

### Choosing a control

| Kind of setting | Control |
| --- | --- |
| free text | `TextInput`, `isLabelHidden` (the row's title is the label) |
| one of several | `Selector` — a dropdown that states the current value |
| on / off | `Switch` |
| opens a screen | nothing: `onOpen`, and the row ends in a chevron |
| read-only fact | the value as plain text |

Theme is a `Selector` and **not** a segmented control: three fixed segments
spend the row's whole trailing edge showing two options nobody picked, and they
cannot grow — a fourth theme would have to change the control.

### Icons

Astryx's built-in registry only. That set has 26 semantic names and none of them
is "network" or "appearance", so each nav entry takes the nearest available
sense and says so where it is chosen (`externalLink` for traffic leaving the
machine, `viewColumns` for how the app is painted, `copy` for the thing a new
wave is copied from, `wrench` for tooling, `info` for read-only facts). The app
deliberately does not draw one-off glyphs for this.

## Sections and routes

| Route | Section | Component |
| --- | --- | --- |
| `/settings` | Network | `NetworkPane` (`public.tsx`) |
| `/settings/appearance` | Appearance | `AppearancePane` (`public.tsx`) |
| `/settings/templates` | Templates | `TemplateListPage` (`templates.tsx`) |
| `/settings/templates/$templateId` | Templates › one template | `TemplateEditorPage` |
| `/settings/plugins` | Plugins | `PluginsPane` (`plugins.tsx`) |
| `/settings/about` | About | `AboutPane` (`public.tsx`) |

Real routes rather than pane-local state: Back leaves the template editor
instead of leaving Settings, and every pane can be linked to. The dialog around
them is owned by `app/shell` so it survives navigation between its own sections
— see `app/shell/settings-overlay.tsx`.

**Going back from a second level** is a `‹ Parent` ghost button above the title,
never a filled button beside it: a filled button beside a title reads as an
action *on* the thing, not as the way back out of it.

**The overlay is top-aligned, not centred.** `.dialog-overlay-wide` hard-codes
`align-items: start` and lives in `styles/`, which is frozen; changing it needs
an `OWNERSHIP-CHANGE` trailer against an issue. The pane's `min-block-size`
floor is the mitigation available from this layer, and it is not centring.

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

While `settings === undefined` the Network pane renders a loading line and
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
- A template row is named after its template — the row *is* the drill-in
  target, so there is no "Edit" button to name.
- **Intentionally not done:** no `<a href>` anywhere (INV-A11Y-061).

## Test contract

`getByRole` / `getByLabelText` only — never a CSS class selector.
`public.test.tsx` and `templates.test.tsx` hold behavior;
`public.contract.test.tsx` holds invariants.
