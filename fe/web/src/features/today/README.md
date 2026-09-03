# `features/today`

The landing route: a status bar and **the day's document**, beside a panel
holding the week calendar's activity agenda, the Running list, and the
launchpad track's conversations.

Two things were removed on 2026-09-03 (owner call) and must not drift back in
without one:

- **The Today terminal placeholder.** A dashed box reading "Terminal is not
  wired up yet" closed the main column, and this file carried a full contract
  (INV-TODAYTERM-001/003/005/006) for an implementation that never landed and
  has no `features/today/terminal` to land in. Both are gone. A page does not
  get to keep making a promise it has not kept.
- **RECENT.** A deliberate trade of reach for focus, recorded as a trade rather
  than as a de-duplication: RECENT and the calendar agenda were selected by
  different rules, so the overlap between them and the reach given up follow from
  `activeTracksOn` — the calendar agenda's selector, whose criterion is in
  `fe/core/domain/track.ts` — together with RECENT's own `updatedAt` ordering
  and cap. What the trade buys is what this route is for: Today answers
  *what needs me*, and a list ordered by `updatedAt` with no date bound is an
  archive browser, which is a surface this page is not and should not grow
  into.

  This entry deliberately spells out no set relation between the two lists:
  four review rounds on #1340 each introduced a fresh falsifiable claim while
  trying to write that relation down precisely. The criterion belongs to the
  code.

**The Conversations module was proposed for removal in that same pass and kept
(#1340); #1341 then changed what it lists.** Those are two decisions, and
reading either one alone gets the module wrong.

#1340 kept it on the ground that it was not a duplicate of the track pages'
module: on Today it read the session registry, which made it a cross-track index
(#1189 S5), and a trial deletion turned 18 assertions red across the three
`*-conversation.test.tsx` suites in `app/router/`.

#1341 is owner withdrawing that ground, not a re-run of the argument. Today now
lists the launchpad track's own conversations, so the module says on this route
the sentence a track page says on its own: *the conversations of the track you
are looking at*. The cross-track index does not stay here — it becomes a card of
its own, on its own issue. The 18 assertions were adjudicated one by one in the
#1341 PR description (9 withdrawn together with the #1189 S5 delivery they
belonged to, 5 rewritten to question the registry directly, 2 narrowed, 2
carried unchanged), so a later reader who finds this paragraph before that table
should read the table.

What the module is today, and which parts of it are load-bearing, is written up
under **The Conversations module (#1341)** below.

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

## The phone, and the ledger that declares it (#1234)

The compact viewport draws a header and the month calendar, and that is all it
draws. **The gap is allowed; leaving it undeclared is not.** #1253 added six
props to `TodayPageProps` — `launchpad`, `launchpadDocument`, `launchpadError`,
`onWriteSummary`, `summaryPending`, `summaryNotice` — none of which the phone
renders, and the whole review chain missed it, because nothing anywhere stated
what the phone leaves out.

`page-props.ts` states it. `TODAY_VIEWPORT_LEDGER` maps **every** key of
`TodayPageProps` to `{ render: true }` or `{ render: false, why }`, and two
things follow mechanically from `tsc -b`:

- a prop added to `TodayPageProps` and not to the ledger does not compile, and
  the diagnostic names the prop;
- a prop declared `render: false` is not a member of `TodayCompactProps`
  (`Pick<TodayPageProps, …>` over the rendered keys), so `TodayCompact` cannot
  read it even by accident.

Two more things hold it up, and both were added after review found the ledger
easy to walk around:

- **`TodayPage` does not know which viewport it is on.** `ViewportDispatch`
  (`viewport-dispatch.tsx`) owns `useCompactViewport` and is generic in both
  prop packs, so it has no name for any Today field; `TodayPage` names fields
  but cannot tell the viewports apart. While one function held both, an
  `if (compact) return <>{props.launchpadDocument}…</>` compiled clean and the
  ledger had nothing to say about it. The reach is exactly this: while
  `public.tsx` does not import `useCompactViewport`, that escape cannot be
  written there. Importing the hook back into it still compiles — measured —
  so what this buys is a visible import instead of one line in a branch.
- **The ledger is bound to the real signatures, in types.** `page-props.ts`
  asserts that the ledger's keys and `keyof TodayPageProps` are the *same set*
  and that neither side has an index signature; `page-props.test.ts` asserts
  that `TodayPage` and `TodayCompact` take exactly the canonical and derived
  types. Both use type identity, not `extends`: `TodayPageProps & { x?: … }` is
  mutually assignable to `TodayPageProps`, so an assignability check sees no
  problem with an entry that has quietly grown a prop. The signature assertions
  live in the *test* module on purpose — a local type of the same name shadows
  any assertion written inside `public.tsx` itself.

Consequences for whoever touches this next: the props type lives in
`page-props.ts` rather than `public.tsx` (which re-exports it) because a ledger
that names `keyof TodayPageProps` and a page that consumes the derived keys
would otherwise import each other, and `no-circular` counts type-only edges.
`render: true` only guarantees the phone renderer *may* name the prop; that it
reaches the DOM is a liveness property no type carries. And the clock is
sampled per renderer, so crossing the breakpoint reseeds `now` and restarts the
15s interval — identical behaviour whenever `nowMs` is pinned, and a fresher
clock when it is not.

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

## The Conversations module (#1341)

It lists **the launchpad track's own conversations** — the same rule a track
page follows for itself, said about the track whose report is the document
above. The list and the module head's `+` are injected by `app/router`
(`conversationList` / `conversationAction`), because `features/**` may not
import a sibling domain; what changed in #1341 is what the router feeds those
two slots, not their shape.

Its previous source was the session registry — the conversations this browser
tab had opened, on any track, each row suffixed `, on <track>`. Two things were
wrong with that, and only the first is cosmetic. It made Today's Conversations
module answer a different question from the one the same-named module answers on
a track page. And it could not reach the conversation this page itself creates:
`POST /api/today/summary` starts one conversation on the launchpad and that
conversation *is* the thing the reader asked for — it is what writes the report
— but the registry only learns of a conversation from a tab that has it on
screen (the open row, or the rows of a `'rows'` route that named its track), and
no route rendered the launchpad's list, because the launchpad sits in the system
area that `GET /api/areas` filters out. So this module never listed it. The
endpoint's own doc comment said it would be
"openable in Today's Conversations module"; it was not, and that gap was
observed as a failing test before the inversion, not reasoned about.

Consequences worth knowing before "fixing" one of them:

- **A row opens in place**, in Today's own drawer, instead of navigating. The
  row is on the launchpad and the launchpad's page is this one; the launchpad
  lives in the system area, which `GET /api/areas` filters out (#175), so a
  navigation would land the reader on a track that no list of theirs contains.
- **The `+` is offered**, because there is now a single track to attach a
  conversation to. `TodayRoute` withholds it on one condition, `launchpadTrackId
  === ''`: with no launchpad the way to get a track is
  `POST /api/today/launchpad/ensure`, a write that waits on codex and that
  INV-TODAYDOC-001 keeps off this page.
- **A cross-track index is gone from here, and is not lost.** Owner's plan is a
  card of its own holding everything about one track; it has its own issue. Do
  not squeeze it back into this module.
- Rows carry no track name (`showTrack: false`) — this page is about the one
  track they are on — and no turn count, because `toTrackConversation` leaves
  `turns` absent: the endpoint does not count them.

## Deliberate gaps (do not "fix" these by accident)

- **INV-TODAY-002** — `scheduledEvents` is permanently empty in production and
  that is a *seam*, not dead code. Scheduled events and live track activity must
  co-exist in one agenda; a scheduling plugin fills the prop later. Deleting the
  branch deletes the seam.
- Attention counting is lifecycle-only for now. The kernel's card-FSM signal
  (`anyCardNeedsInput`) is OR'd in once overlays are read; the sidebar and this
  clock must keep using the same predicate.
