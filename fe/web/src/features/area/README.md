# `features/area`

Two presentational surfaces plus the shared area palette.

| Module | What it is |
| --- | --- |
| `palette.ts` | INV-DUP-006 — the canonical area colour table (`AREA_PALETTE`), consumed only by the sidebar's random new-area picker. Not redeclared anywhere. |
| `page/public.tsx` | `<AreaPage>` — the area route shell: swatch, rename, track count, `+ New track`, delete-with-confirm, and a body slot. |
| `new-track/public.tsx` | `<NewTrackForm>` — the create form: a task, plus an optional folder (#1131, #1147). Local form state only; never calls an API. |

## Composition contract

`AreaPage` **does not render the track list**. `features/**` may not import a
sibling feature domain, so the track rows live in `features/track/row` and
`app/router` composes them:

```tsx
<AreaPage area={area} trackCount={tracks.length} trackList={<TrackList … />} … />
```

The empty state ("no tracks yet") therefore belongs to the list slot. The page
must never add a second one — two empty states on one screen is the bug this
sentence exists to prevent.

## Behaviour contracts

- **INV-A11Y-061** — no `<a href>` anywhere; navigation and destructive actions
  are `<button>` + callback. Locked by a contract test that counts `<a>` in the
  container *and* in the portalled confirm dialog.
- **INV-CONFIRM-001** — the delete confirm always keeps Cancel enabled (the
  user keeps an exit). Closing during the await dismisses the dialog while the
  request is aborted and pending is released; a `finally` clears pending. A *rejected* `onDeleteArea` must
  therefore still close the dialog and leave a reopened Confirm usable —
  otherwise the second attempt is dead on arrival. The rejection is swallowed
  here on purpose: surfacing it is the caller's job, not stranding the dialog is
  this component's.
- Rename goes through the shared `ui/editable-title` (INV-DUP-008), which trims
  and skips no-op commits; `AreaPage` only wires `onRenameArea` into it.

## Create: a sentence, and optionally a template and a folder (#1131, #1147 S3, #1211)

The surface is the route `/area/{id}/new`, not a dialog (#1211). `area_id` comes
from the URL, not from a form field; the two `+` controls (area page, rail row)
navigate there.

Submit is enabled iff the composer is non-empty — template and folder are never
required and both default to nothing.

`NewTrackDraft` is `{ message, template_id?, template_input?, cwd? }`. **`message`
is the track's intent, not its title**: `NewTrackRoute` posts `POST /api/tracks`
with *no* `title` at all — the kernel stores the empty string and the spec agent
names the track later through `calm.track.rename`.

`message`'s destination is the new track's spec card as its first message, and
**that delivery is not implemented here** (#1299). Doing it from this page takes
three writes and cannot be made sound from a component — an unmount mid-flight
loses the sentence silently, and `/spec/input` has no idempotency key so any
retry can double-send. It moves into the create request instead. Until then the
page says so on screen and the route opens the track's spec conversation on
arrival, so saying it again is one keystroke. See `NewTrackRoute`'s doc comment.

**The `cwd` key is absent, not empty, when no folder was chosen.** That
distinction is the whole workspace contract: no `cwd` / `attach_folder` is the
kernel's *managed* branch, where it creates and owns a workspace under the
workspace root; `{ cwd, attach_folder: true }` *attaches* the track to a directory
the kernel never creates, moves or deletes.

Create time is the only entry into that choice: the kernel offers no
`managed → attached` conversion (`docs/1147-workspace-design.md` §更换与冻结),
which is why the control is here and why clearing it back to the managed default
is a control on this page.

The folder control is **not** `DirectoryField` here, and that is deliberate:
`DirectoryField` renders `ui/directory-browser` inline when no dialog is above
it, and on a route that fallback is what fires — the #1211 regression. This page
owns its own `Dialog` around `DirectoryBrowser` instead
(CAP-TRACKWORKSPACE-003); `features/track/new-card`, which *is* inside a dialog,
keeps `DirectoryField` and the child-view push (CAP-TRACKWORKSPACE-006). Both
call sites are registered in `tools/architecture/directory-picker-hosts.mjs`,
which fails closed on a new one. Either way the `listDirectory` port arrives as
a prop; `features/**` never reaches a transport (see
`app/providers/directory.ts`).

Legacy `web/` `NewTaskForm` is unchanged and still sends a full body.

## Deliberately deferred (not "missing")

Cut from the legacy NewTaskForm on purpose; do not re-add without a slice:

- The GitHub issue-dev template variant.
- The raw `template_input` JSON escape hatch.
- The debounced `GET /api/areas/resolve` auto-match that pre-selects an area from
  the typed directory. **Not** re-added by #1147 S3: its only effect was
  choosing `attach_folder: false` when some area already covered the path, and
  the kernel's in-transaction claim scan reaches that same answer without the
  round trip — atomically, which a client-side pre-check cannot be.
- A free-text path input beside the picker. The picker's own combobox already
  accepts a typed absolute path; a second one would be two sources of truth for
  the same value.

## Test contract

`getByRole` / `getByLabelText` only — never a CSS class selector (a lint rule
rejects class selectors in `querySelector`). `public.test.tsx` holds behavior;
`page/public.contract.test.tsx` holds the invariants above, one `it` each, all
four mutation-verified.
