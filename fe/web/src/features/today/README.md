# `features/today`

The landing route: a clock, the Today terminal panel, and a week calendar whose
agenda surfaces live wave activity.

## Visual contract

Tokens only (`--text*`, `--surface*`, `--space-*`, `--radius-*`, `--font-*`).
All styling is `today.module.css` in `@layer features`. Cove colour is the one
value that arrives as inline `style` — it is per-row data, not a variant.

## Accessibility contract

- Every navigable row is a `<button>`; the accessible name carries the wave
  title, the attention/running state, the lifecycle phrase, and the cove name.
  Dot flags are `aria-hidden` decoration for fast scanning only.
- Day cells are buttons with `aria-pressed` and a full-date accessible name.
- **Intentionally not done:** no `<a href>` anywhere (INV-A11Y-061).

## Test contract

`getByRole` only. `public.test.tsx` holds behavior; `public.contract.test.tsx`
holds invariants. Tests pin `nowMs` so assertions cannot drift across midnight
or DST.

## Deliberate gaps (do not "fix" these by accident)

- **INV-TODAY-002** — `scheduledEvents` is permanently empty in production and
  that is a *seam*, not dead code. Scheduled events and live wave activity must
  co-exist in one agenda; a scheduling plugin fills the prop later. Deleting the
  branch deletes the seam.
- **The Today terminal is not wired here yet** (`features/today/terminal`). When
  it lands, its resolve order is a contract: read the cached `calm.todayCardId`
  → verify the card still has a terminal row → bootstrap **only** on 404. Any
  other error must surface as an error, never a silent rebuild (INV-TODAYTERM-001),
  the whole chain runs in one in-flight-guarded async resolver
  (INV-TODAYTERM-003), the Today wave **omits `cwd` and `attach_folder`**
  entirely (INV-TODAYTERM-005), and the 404 check is duck-typed on
  `status` rather than `instanceof` (INV-TODAYTERM-006).

  INV-TODAYTERM-005 used to read "passes `cwd: '/'` with `attach_folder: false`".
  #1147 S3 inverted it: an omitted `cwd` is the *managed* branch, so the kernel
  allocates and `git init`s a real directory the wave's workers can lease, while
  `/` was never a workspace at all — a `kind: codex` task on that wave died in
  `git_repo_root_for_wave_cwd` with nothing but `spawn-failed`, which is the
  defect #1147 was opened on. An explicit `cwd` now means "attach this existing
  repository" and is validated, so `/` would be a 400.
- Attention counting is lifecycle-only for now. The kernel's card-FSM signal
  (`anyCardNeedsInput`) is OR'd in once overlays are read; the sidebar and this
  clock must keep using the same predicate.
