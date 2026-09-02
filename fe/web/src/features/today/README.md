# `features/today`

The landing route: a status bar, **the day's document**, the Today terminal
placeholder, and a panel holding the week calendar's activity agenda, the
Running / Recent lists and the conversation module.

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

## The document region (#1253)

The main column is **status bar, then document**. The status bar is the header's
`N waiting · N running` plus the compact waiting rows; it is O(1) in height, so
it cannot grow and push the document off the first screen. "The document is the
protagonist" is expressed by area and visual weight, not by type size (§8.1).
Running and Recent are ambience and live in the panel.

- **INV-TODAYDOC-001** — the page load only *resolves* (`GET /api/today/launchpad`).
  `POST /api/today/launchpad/ensure` materializes a workspace and waits on a
  `spec-harness-start` operation, so it must never be on this path; it belongs
  to an explicit action. There is no such action yet.
- **INV-TODAYDOC-002** — a failed resolve is rendered as an error and the empty
  state is suppressed. A 5xx that degrades into "nothing written today" tells
  the reader their day was empty when the server was simply unreachable. 404 is
  not a failure: it is the ordinary "no launchpad yet" answer.
- **INV-TODAYDOC-003** — the empty state is decided by the server's
  `report_has_noninitial_content` and by nothing else. **Never null-check the
  report, and never read its text.** The kernel's freshly-minted report is a
  well-formed document — a maintenance-contract comment plus four empty H1s —
  so `readWaveReport` returns non-null for it and a null-check renders four
  empty headings where the empty state belongs. Reading the body would be
  mirror code for text the kernel owns besides. The predicate is an
  approximation: it answers "has anyone written this report", flips on a human
  edit as readily as on an agent's, and never flips back.
- **No trigger button.** `POST /api/today/summary` lands in #1253 PR2. Until
  then the empty state is text only — not a stub, not a disabled control.

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
