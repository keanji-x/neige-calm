# `features/cove`

Two presentational surfaces plus the shared cove palette.

| Module | What it is |
| --- | --- |
| `palette.ts` | INV-DUP-006 — the one cove colour table (`COVE_PALETTE`, `coveColorForIndex`). Not redeclared anywhere. |
| `page/public.tsx` | `<CovePage>` — the cove route shell: swatch, rename, wave count, `+ New wave`, delete-with-confirm, and a body slot. |
| `new-wave/public.tsx` | `<NewWaveForm>` — a small subset of the legacy 1166-line NewTaskForm. Local form state only; never calls an API. |

## Composition contract

`CovePage` **does not render the wave list**. `features/**` may not import a
sibling feature domain, so the wave rows live in `features/wave/row` and
`app/router` composes them:

```tsx
<CovePage cove={cove} waveCount={waves.length} waveList={<WaveList … />} … />
```

The empty state ("no waves yet") therefore belongs to the list slot. The page
must never add a second one — two empty states on one screen is the bug this
sentence exists to prevent.

## Behaviour contracts

- **INV-A11Y-061** — no `<a href>` anywhere; navigation and destructive actions
  are `<button>` + callback. Locked by a contract test that counts `<a>` in the
  container *and* in the portalled confirm dialog.
- **INV-CONFIRM-001** — the delete confirm always keeps Cancel enabled (the
  user keeps an exit). Closing during the await dismisses the dialog while the
  request continues in the background; a `finally` clears pending. A *rejected* `onDeleteCove` must
  therefore still close the dialog and leave a reopened Confirm usable —
  otherwise the second attempt is dead on arrival. The rejection is swallowed
  here on purpose: surfacing it is the caller's job, not stranding the dialog is
  this component's.
- Rename goes through the shared `ui/editable-title` (INV-DUP-008), which trims
  and skips no-op commits; `CovePage` only wires `onRenameCove` into it.

## `attach_folder` and the 409 contract

`NewWaveDraft.attachFolder` maps to the wave-create body's `attach_folder`.

Claiming a directory for a cove is a durable, cross-cove side effect. When the
working directory is **not already claimed**, creating a wave in it without
`attach_folder: true` makes the server answer **HTTP 409 with code `conflict`**
and a message naming the cove that would have to claim the folder. That sentence
is in the checkbox help text so the user can act on the error instead of
guessing.

The checkbox therefore starts **unchecked** (INV-NEWWAVE-002). Defaulting it to
`true` would turn a decision the user must make into a silent land-grab.

The working directory must be a non-empty **absolute** path (INV-NEWWAVE-001):
the kernel does not resolve it against anything, so a relative path would land
wherever the server happens to run. Invalid input blocks submit and renders an
inline hint.

## Deliberately deferred (not "missing")

Cut from the legacy NewTaskForm on purpose; do not re-add without a slice:

- The GitHub issue-dev workflow variant.
- The raw `workflow_input` JSON escape hatch.
- The debounced `GET /api/coves/resolve` auto-match that pre-selects a cove from
  the typed directory.
- The directory `Browse…` picker (`ui/directory-browser`).

## Test contract

`getByRole` / `getByLabelText` only — never a CSS class selector (a lint rule
rejects class selectors in `querySelector`). `public.test.tsx` holds behavior;
`page/public.contract.test.tsx` holds the invariants above, one `it` each, all
four mutation-verified.
