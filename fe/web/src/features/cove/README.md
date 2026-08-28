# `features/cove`

Two presentational surfaces plus the shared cove palette.

| Module | What it is |
| --- | --- |
| `palette.ts` | INV-DUP-006 — the canonical cove colour table (`COVE_PALETTE`), consumed only by the sidebar's random new-cove picker. Not redeclared anywhere. |
| `page/public.tsx` | `<CovePage>` — the cove route shell: swatch, rename, wave count, `+ New wave`, delete-with-confirm, and a body slot. |
| `new-wave/public.tsx` | `<NewWaveForm>` — title-only create (#1131). Local form state only; never calls an API. |

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
  request is aborted and pending is released; a `finally` clears pending. A *rejected* `onDeleteCove` must
  therefore still close the dialog and leave a reopened Confirm usable —
  otherwise the second attempt is dead on arrival. The rejection is swallowed
  here on purpose: surfacing it is the caller's job, not stranding the dialog is
  this component's.
- Rename goes through the shared `ui/editable-title` (INV-DUP-008), which trims
  and skips no-op commits; `CovePage` only wires `onRenameCove` into it.

## Title-only create (#1131)

The form has one field, Task. Submit is enabled iff `title.trim()` is non-empty.
`NewWaveDraft` is `{ title }`. The caller (`AppShell`) sends `POST /api/waves`
as `{ cove_id, title, theme }` and **omits** `cwd` / `attach_folder`. `cove_id`
comes from the surface that opened the dialog (cove page `+` or the rail's
per-cove `+`); it is not a form field.

The kernel then stores `$HOME` on the wave row and does not insert
`cove_folders`. Binding a project in-conversation is a later slice. Legacy
`web/` `NewTaskForm` is unchanged and still sends a full body.

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
