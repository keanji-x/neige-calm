# `features/cove`

Two presentational surfaces plus the shared cove palette.

| Module | What it is |
| --- | --- |
| `palette.ts` | INV-DUP-006 — the canonical cove colour table (`COVE_PALETTE`), consumed only by the sidebar's random new-cove picker. Not redeclared anywhere. |
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
  request is aborted and pending is released; a `finally` clears pending. A *rejected* `onDeleteCove` must
  therefore still close the dialog and leave a reopened Confirm usable —
  otherwise the second attempt is dead on arrival. The rejection is swallowed
  here on purpose: surfacing it is the caller's job, not stranding the dialog is
  this component's.
- Rename goes through the shared `ui/editable-title` (INV-DUP-008), which trims
  and skips no-op commits; `CovePage` only wires `onRenameCove` into it.

## `attach_folder` and the 409 contract

`NewWaveDraft.attachFolder` maps to the wave-create body's `attach_folder`, and
**the form derives it — it is never asked** (INV-NEWWAVE-002).

`POST /api/waves` needs a `cwd` under a folder the target cove has already
claimed; otherwise the server answers **HTTP 409 with code `conflict`** naming
the cove that would have to claim it, unless `attach_folder: true` claims it in
the same transaction. But `GET /api/coves/{cove_id}/folders` already says which
folders a cove owns, so for a cove that owns one there is nothing to ask:

| Folders the target cove owns | What the form shows | What it sends |
| --- | --- | --- |
| 1 | the Task field only | that folder's `path`, `attach_folder: false` |
| more than 1 | a `Folder` select, defaulting to the first by `path` ascending | the picked `path`, `attach_folder: false` |
| 0 | a `Folder` **path input** — this is the cove's first folder | the typed path, `attach_folder: true` |

There is no checkbox any more. With zero existing claims `false` is a guaranteed
409, so the setting had exactly one legal value; with one or more, the path is a
fact the server already holds. The land-grab the checkbox guarded against is
still guarded — claiming a directory another cove owns still answers 409 naming
that cove, and the caller surfaces the server's own sentence in `error`.

The zero-folder path must be a non-empty **absolute** path (INV-NEWWAVE-001):
the kernel does not resolve it against anything, so a relative path would land
wherever the server happens to run. Invalid input blocks submit and renders an
inline hint. `sortedCoveFolders` (`core/domain/cove.ts`, INV-NEWWAVE-003) is
what makes "the first folder" one deterministic thing.

`coveId` is **controlled by the caller**. The form's shape depends on a query,
`features/**` may not import `app/**`, so whoever owns the folder read owns the
cove selection too — today that is `AppShell`, which owns the dialog because the
rail and the cove page both open it.

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
