# `features/today`

The landing route: a status bar, **the day's document**, the Today terminal
placeholder, and a panel holding the week calendar's activity agenda, the
Running / Recent lists and the conversation module.

## Visual contract

Tokens only (`--text*`, `--surface*`, `--space-*`, `--radius-*`, `--font-*`).
All styling is `today.module.css` in `@layer features`. Area colour is the one
value that arrives as inline `style` — it is per-row data, not a variant.

## Accessibility contract

- Every navigable row is a `<button>`; the accessible name carries the track
  title, the attention/running state, the lifecycle phrase, and the area name.
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
  `planner-harness-start` operation, so it must never be on this path; it belongs
  to an explicit action. There is no such action yet.
- **INV-TODAYDOC-002** — **`null` is data; any failure is an error.** A failed
  read is rendered as an error and the empty state is suppressed: a 5xx that
  degrades into "nothing written today" tells the reader their day was empty
  when the server was simply unreachable.

  There is no status-code special case. "No launchpad yet" is a 200 carrying a
  null body, so the frontend never inspects a status to decide whether an
  answer is data. It briefly did — the endpoint returned 404 and this layer
  translated it — and that put a browser console error on every fresh-workspace
  session of the landing route, failing two Playwright specs that assert none.
  Routine absence is data; anomalous absence (a launchpad with no report card)
  is still a 404, and correctly so.

  This covers the track detail as well as the resolve, and the two document
  reads have **three** failure-shaped states that must not be collapsed:
  in flight (which is every page load, because the detail query cannot start
  until the resolve returns a track id), detail read failed, and payload
  undecodable. `readTrackReport() === null` is true in all three, so a single
  `ReportDocument` `empty` for all of them once told a reader whose server was
  unreachable that their build was too old, with no retry. `app/router` splits
  them; `app/router/today-document.test.tsx` owns the coverage, because this
  feature cannot see the interleaving.
- **INV-TODAYDOC-003** — the empty state is decided by the server's
  `report_has_noninitial_content` and by nothing else. **Never null-check the
  report, and never read its text.** The kernel's freshly-minted report is a
  well-formed document — a maintenance-contract comment plus four empty H1s —
  so `readTrackReport` returns non-null for it and a null-check renders four
  empty headings where the empty state belongs. Reading the body would be
  mirror code for text the kernel owns besides. The predicate is an
  approximation, and the shape of the approximation matters for PR2: it is a
  statement about the report's **current content**, not about its history. It
  consults no history (`TrackReportPayload::report_startup_read_required` is a
  pure comparison against the canonical pair), so it flips on a human edit as
  readily as on an agent's, and **it flips back** — restore `summary`/`body`
  byte-for-byte to canonical and it reads `false` again, `doc_rev` and `blocks`
  notwithstanding.

  **So it is not a durable "the summary has run" marker, and must not be used
  as one.** Suppressing a re-run button on it would mean a user reverting the
  document silently un-suppresses the button. Anything that needs "did the
  summary run" needs its own persistent marker or event.
- **The trigger** (#1253 PR2, D5) — `onWriteSummary` posts to
  `POST /api/today/summary`, which takes no body and no prompt. The control is
  offered in **both** states, with the label the only difference (`Write` /
  `Rewrite`): the report's own contract is "a snapshot of now, rewritten every
  time", so re-running is ordinary, and the predicate above cannot be used to
  suppress it. It is **not** hidden when nothing has happened today — this page
  has no activity read and by design never will (D4 deleted the layer that would
  have offered one), so the gate is the server's: the endpoint computes the day's
  window itself and refuses an empty one without creating a conversation or
  sending a message (INV-TODAYDOC-007). The refusal comes back as
  `summaryNotice` and reads as a fact about the day, not as an error.
- **The status bar is capped** (`WAITING_ROW_LIMIT`). Its O(1) height is D7's
  reason for putting it above the document, so an uncapped list would not be a
  cosmetic problem — it would falsify the layout's justification. The overflow
  sits behind one disclosure control rather than being dropped: RUNNING and
  RECENT both exclude anything already counted as waiting.
- **The first-run page owns a document too.** `areas` is the *user-visible*
  list — #175 filters the system area out of `GET /api/areas` and the launchpad
  lives there — so "no tracks, no areas" is an ordinary state for a workspace
  whose only content is the day's report.

### The refresh chain, and why nothing generated protects it

`core/events/invalidation-plan.ts`'s `track.report_edited` policy now carries
**four** keys. Two of them exist only for this page: `['track', id]` is what the
document is read through, and `['today-launchpad']` is what the empty-state
predicate is read through. Without either, pressing the trigger leaves the page
unchanged until a reload — the first bug report this feature would have got.

`PolicyMap` is exhaustive over event **kinds**, not over query keys, so deleting
either line adds no missing kind and turns no golden red. What guards them is
two assertions and nothing else: the literal key list in
`core/events/invalidation-plan.contract.test.ts`, and the end-to-end one in
`app/events/query-invalidation-adapter.test.ts`, which also covers the second
link — a planned key with no adapter arm is silently dropped.

## Deliberate gaps (do not "fix" these by accident)

- **INV-TODAY-002** — `scheduledEvents` is permanently empty in production and
  that is a *seam*, not dead code. Scheduled events and live track activity must
  co-exist in one agenda; a scheduling plugin fills the prop later. Deleting the
  branch deletes the seam.
- **The Today terminal is not wired here yet** (`features/today/terminal`). When
  it lands, its resolve order is a contract: read the cached `calm.todayCardId`
  → verify the card still has a terminal row → bootstrap **only** on 404. Any
  other error must surface as an error, never a silent rebuild (INV-TODAYTERM-001),
  the whole chain runs in one in-flight-guarded async resolver
  (INV-TODAYTERM-003), the Today track **omits `cwd` and `attach_folder`**
  entirely (INV-TODAYTERM-005), and the 404 check is duck-typed on
  `status` rather than `instanceof` (INV-TODAYTERM-006).

  INV-TODAYTERM-005 used to read "passes `cwd: '/'` with `attach_folder: false`".
  #1147 S3 inverted it: an omitted `cwd` is the *managed* branch, so the kernel
  allocates and `git init`s a real directory the track's workers can lease, while
  `/` was never a workspace at all — a `kind: codex` task on that track died in
  `git_repo_root_for_track_cwd` with nothing but `spawn-failed`, which is the
  defect #1147 was opened on. An explicit `cwd` now means "attach this existing
  repository" and is validated, so `/` would be a 400.
- Attention counting is lifecycle-only for now. The kernel's card-FSM signal
  (`anyCardNeedsInput`) is OR'd in once overlays are read; the sidebar and this
  clock must keep using the same predicate.
