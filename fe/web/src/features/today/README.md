# `features/today`

The landing route: a status bar and **the day's document**, beside a panel
holding the week calendar's activity agenda and the Running list.

Two things were removed on 2026-09-03 (owner call) and must not drift back in
without one:

- **The Today terminal placeholder.** A dashed box reading "Terminal is not
  wired up yet" closed the main column, and this file carried a full contract
  (INV-TODAYTERM-001/003/005/006) for an implementation that never landed and
  has no `features/today/terminal` to land in. Both are gone. A page does not
  get to keep making a promise it has not kept.
- **RECENT.** This was a deliberate trade of reach for focus, and it is worth
  stating as one rather than as a de-duplication, because the two lists were
  never the same list.

  What *did* overlap: the calendar's agenda one module up is
  `activeTracksOn(selected)` over the visible tracks — every one of those whose
  activity interval overlaps the selected day, with no cap on how many rows it
  draws (the module scrolls at eight) — while RECENT took the same visible
  tracks minus waiting and running, ordered them by `updatedAt`, and **kept 12**.
  So on the default selection a quiet track whose interval covers today was
  drawn twice inside one card, and the cap is the only thing that ever excused
  it: 13 such tracks put 13 rows in the agenda and 12 in RECENT, and the row
  RECENT dropped was the least recently touched. The de-dup that existed
  (`shown`) only excluded waiting and running; it never looked at the agenda,
  so "one track appearing twice on a page distorts both the counts and the
  scan" was violated by the card against itself for each of those tracks except
  the ones the cap had already discarded.

  What did **not** overlap, and is the part being given up: RECENT applied no
  date filter, so it also carried tracks whose interval had already closed
  before today, and today's agenda holds none of those. That class is not the
  clean "finished before today" it sounds like, because `activeTracksOn` closes
  the interval at

      end = terminalAt ?? (isTerminal(lifecycle) ? updatedAt : nowMs)

  and `terminalAt` is nullable. A terminal track with `terminalAt === null` — a
  pre-migration row — is closed at `updatedAt` instead, so editing its title
  today puts it back in *today's* agenda even though the work finished last
  month. What RECENT could show and today's agenda cannot is therefore the
  quiet tracks the overlap test rejects for today: those whose `end` fell
  before today's start, plus the degenerate row whose `createdAt` is still in
  the future. The `terminalAt` case is worth knowing precisely because it means
  "what happened while I was away" was never a clean class here — a null
  `terminalAt` moves a row between the two lists on a metadata edit.

  Those tracks are not lost. A row is in the agenda of the day its own `end`
  falls on — that day satisfies both endpoints of the overlap test by
  construction, for any row whose close is not earlier than its creation — and
  stepping the calendar there costs one `Previous week` press per week back,
  since a press moves the selection seven days and the grid follows its week.
  What is given up is the glance, and the price rises with age: a track that
  closed six weeks ago is six presses away instead of one row on the landing
  page. It is worth paying because of what this route is for — Today answers
  *what needs me*, and a list ordered by `updatedAt` with no date bound is an
  archive browser, which is a surface this page is not and should not grow
  into.

**The conversation module was proposed for removal in the same pass and kept.**
It looks like a duplicate of the track pages' module and is not one: on Today it
is the **cross-track index** (#1189 S5). It is the only place a track's
conversations stay reachable once you have navigated away from that track, and
G6 opens one *from here* — the row navigates to the track and opens its
assistant drawer in one act. Removing it turned 18 assertions red across the
three `*-conversation.test.tsx` suites in `app/router/`, all of them behavioural
(`[G5] lists every conversation of a track on Today after merely visiting it`,
`[G6] opens an assistant conversation asked for from Today`, and the two that
pin what must *not* reach Today). Judge the duplication complaint against those
before touching it: the fix, if there is one, is about how the module is
*labelled* on this route, not whether it exists.

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
protagonist" is expressed by area and visual weight — and, since 2026-09-03, by
type: the document region reads at the prose rank (`--text-lg` paired with
`--measure-prose`, the only pairing tokens.css sanctions) while everything
around it stays interface-sized. Running is ambience and lives in the panel.

- **INV-TODAYDOC-001** — the page load only *resolves* (`GET /api/today/launchpad`).
  `POST /api/today/launchpad/ensure` materializes a workspace and waits on a
  `spec-harness-start` operation, so it must never be on this path; it belongs
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
  sits behind one disclosure control rather than being dropped: RUNNING
  excludes anything already counted as waiting.
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
- Attention counting is lifecycle-only for now. The kernel's card-FSM signal
  (`anyCardNeedsInput`) is OR'd in once overlays are read; the sidebar and this
  clock must keep using the same predicate.
