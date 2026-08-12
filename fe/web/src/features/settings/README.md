# `features/settings`

Workspace settings. `SettingsPage` is presentational: it never calls an API.
The bag, the in-flight flag, both error strings and the save callback all
arrive as props; the router owns fetching and mutating.

## Sections

- **Breadcrumb** — `Today` is a `<button>` that calls `onOpenToday`, followed by
  a non-interactive `Settings` crumb (`aria-current="page"`).
- **Network** — HTTP proxy and HTTPS proxy text inputs, seeded from
  `settings[HTTP_PROXY_KEY]` / `settings[HTTPS_PROXY_KEY]` (empty string when the
  key is absent). `Save` is disabled unless the draft differs from the seed and
  while `saving` (the label flips to `Saving…`); `Reset` restores the fields from
  the last `settings` prop.
- **Appearance** — a `role="radiogroup"` named `Appearance` with Light / Dark /
  System, reporting through `onThemeModeChange`.

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
user is typing. A genuine server-side change does re-seed and discards the draft.

## Appearance is deliberately not server-persisted

Theme is a per-device preference, so it never goes through `onSave` and never
enters the settings bag. `ThemeMode` is declared here rather than imported from
`app/theme` because `features/**` may not import `app/**`; the app layer adapts
to this union at the router seam.

## Loading is not an empty form (INV-SETTINGS-002)

While `settings === undefined` the Network section renders a loading line and
**no text input at all**. An empty form would let the user save blanks over real
values before the bag has landed.

## Accessibility contract

- `loadError` and `saveError` render in `role="alert"`; `savedAt` drives a
  transient `Saved.` in `role="status"` (`savedNoticeMs` shortens the window in
  tests).
- **Intentionally not done:** no `<a href>` anywhere (INV-A11Y-061).

## Test contract

`getByRole` / `getByLabelText` only — never a CSS class selector.
`public.test.tsx` holds behavior; `public.contract.test.tsx` holds invariants.
