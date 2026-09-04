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
carried unchanged). The later Area-navigation refactor (#1354) deleted the Area
route and its Conversations flow, so the two Area compatibility assertions are
not resurrected by this PR's rebase. A later reader who finds this paragraph
before the table should read both decisions together.

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
draws. **The gap is allowed; leaving it undeclared is not.** The desktop-only
document and conversation work added `launchpad`, `launchpadDocument`,
`launchpadError`, `documentAction`, `conversationList` and
`conversationAction` to `TodayPageProps`; none reaches the phone. The original
review chain missed this class of gap because nothing anywhere stated what the
phone leaves out.

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

The main column is **the document**. The header retains the compact
`N waiting · N running` summary, but the former `Waiting on you` row list is
gone by owner call: it duplicated operational detail above the durable result
and delayed the document. "The document is the protagonist" is expressed by
area and visual weight — and, since 2026-09-03, by type: the document region
reads at the prose rank (`--text-lg` paired with `--measure-prose`, the only
pairing tokens.css sanctions) while everything around it stays interface-sized.
Running is ambience and lives in the panel.

- **INV-TODAYDOC-001** — the page load only *resolves* (`GET /api/today/launchpad`).
  `POST /api/today/launchpad/ensure` materializes a workspace and waits on a
  `planner-harness-start` operation, so it must never be on this path; it belongs
  to an explicit action. The Conversations `+` is that action when no launchpad
  exists: the press ensures it, then opens a draft scoped to the returned track.
  `app/router/today-document.test.tsx` still pins that no write of any kind
  occurs during page load.
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
- **The write-the-report trigger is gone** (#1343, owner call). `onWriteSummary`,
  its `Write` / `Rewrite` labels, its pending state and its notice are all
  removed. The empty state is now **one sentence** and nothing else.

  What replaced it is not another control. Two server-side changes carry it:
  the day's activity window is injected when a conversation is started on the
  launchpad track, and that conversation's agent is started under an identity
  whose first duty is keeping today's report current
  (`planner_card::LAUNCHPAD_ASSISTANT_SYSTEM_PROMPT_TEMPLATE`). Material
  without authority was measured not to be enough — the ordinary assistant
  prompt closes with "you are a guest in a document the planner agent
  maintains", which is false on the launchpad. Ordinary tracks are unchanged.
  `POST /api/today/summary` is still served and still behaves exactly as it
  did; nothing in the browser calls it.

  **INV-TODAYDOC-007 did not move and did not weaken.** It is still enforced by
  that endpoint, which still refuses an empty window without creating a
  conversation or sending a message. What changed is that the frontend no
  longer has a path to it, so the refusal has no UI to surface in — which is
  why `summaryNotice` and its copy are gone rather than relocated. The
  injection path is a *different* ruling on the same fact: an empty day there
  is briefed as empty (`activity_window::opening_activity_briefing`) rather
  than refused, because a conversation the user started commissions no report
  and taking it away over a quiet morning would be the wrong trade. Both live
  in `crates/calm-server`; neither is a statement this page can make.
- **Reset** (#1343) — `documentAction` carries it, and it is the only control
  the document region has. It posts `POST /api/today/launchpad/report/reset`,
  which puts the report back to the kernel's canonical empty document and
  touches nothing else — no conversation is created, reset or deleted.

  Three things about it are decisions rather than details:

  * **It sends no document, and must never grow a parameter for one.** The
    empty-state predicate is a byte-for-byte comparison against
    `TrackReportPayload::initial()`, ~2.6 kB of kernel-owned text assembled
    from two `include_str!`-ed contract fragments. A client posting its own
    copy to `POST /api/tracks/{id}/report` would be mirror code, and one byte
    out fails *silently*: a 200, a rewritten report, and an empty state that
    never appears.
  * **It is destructive**, so it goes through `ConfirmDialog` and
    `useDeleteConfirm`, the same shape and the same failure surface as the
    track delete on this route. The copy lives in `ui/confirm-dialog/copy.ts`
    and names what is *not* lost as well as what is.
  * **It is offered only beside a written report.** There is nothing to reset
    when the report is already canonical, and the empty state is one sentence.
- **Waiting remains a count, not a main-column list.** The header still reports
  the true waiting total; the document begins immediately below it. Tracks stay
  reachable through their Area and the selected day in Calendar.
- **The first-run page uses the full Today layout.** `areas` is the
  *user-visible* list — #175 filters the system area out of `GET /api/areas` and
  the launchpad lives there — so "no tracks, no areas" is an ordinary state,
  not a reason to replace the page with a second generic empty sentence. The
  Calendar and Conversations panel remain visible; the document region alone
  says `Nothing written today yet.`. The conversation `+` remains visible
  before a launchpad exists and explicitly creates it when pressed.

### The refresh chain, and why nothing generated protects it

`core/events/invalidation-plan.ts`'s `track.report_edited` policy now carries
**four** keys. Two of them exist only for this page: `['track', id]` is what the
document is read through, and `['today-launchpad']` is what the empty-state
predicate is read through.

Since #1343 this chain is *more* load-bearing, not less. Nothing on this page
asks for the report to be written any more — an agent writes it from a
conversation — so the event is the **only** way the page can learn it happened.
Without either key the report is written and the page does not move until a
reload.

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
- **The `+` is always offered.** Once a launchpad exists it opens the ordinary
  launchpad-scoped draft. Before one exists its visible empty copy says `Start
  a conversation with Today.`; pressing it calls
  `POST /api/today/launchpad/ensure`, uses the returned track id as the draft's
  scope, and opens the same composer. Creation is therefore attributable to the
  press, never hidden on page load, and the reader never has to choose or guess
  which assistant owns Today. `ensure` can create the track and still return a
  harness-start failure; that failure remains visible after the resolve finds
  the track, and Retry opens the now-existing draft instead of silently doing
  nothing or ensuring it again.
- **The opening activity briefing is system context, not a user turn.** The
  kernel pairs it atomically with the reader's first message so either both are
  durable or neither is. Its typed input-segment presentation keeps the
  transcript from attributing server-supplied counts to the reader.
- **A cross-track index is gone from here, and is not lost.** Owner's plan is a
  card of its own holding everything about one track; it has its own issue. Do
  not squeeze it back into this module.
- Rows carry no track name (`showTrack: false`) — this page is about the one
  track they are on — and no turn count, because `toTrackConversation` leaves
  `turns` absent: the endpoint does not count them.
- Pending and failed list reads are not empty lists. Pending renders no claim;
  failure renders an error with Retry; only a successful `[]` says there are no
  conversations yet. The same applies one level earlier to the launchpad
  resolve: unknown or failed cannot be reworded as an empty list.
- A transcript-derived first-message name is projected back onto the server row
  after the drawer closes, because it is stable and the endpoint does not carry
  it. Turn counts and activity times remain open-row snapshots rather than stale
  exact claims. The list must not fall back to `Assistant` after showing the
  confirmed name while open. The summary writer is the one named exception: its
  first persisted user turn is a server-owned bootstrap instruction, so its
  deterministic card id is projected as `Today’s progress` and that internal
  prompt never becomes reader-facing chrome. An explicit server title still wins.
- A conversation composer does not accept a follow-up until its initial history
  read has succeeded. This is the baseline that lets an optimistic echo tell a
  genuinely new server item from an older identical message. A failed read keeps
  its own error and Retry beside the disabled composer; it is never reworded as
  a failed send.
- The provider grants one in-flight send per conversation across route remounts.
  A write's 200 is its acknowledgement and releases that lease without waiting
  for the two background reads: a failed or hung history refresh is not a failed
  send. The confirmed optimistic turn still blocks another same-card send until
  a server item above its pre-send high-water confirms it. Different conversations
  remain independent. A write failure is stored under that same conversation id,
  so it follows the request across a remount and never appears under another drawer.

## Deliberate gaps (do not "fix" these by accident)

- **INV-TODAY-002** — `scheduledEvents` is permanently empty in production and
  that is a *seam*, not dead code. Scheduled events and live track activity must
  co-exist in one agenda; a scheduling plugin fills the prop later. Deleting the
  branch deletes the seam.
- Attention counting is lifecycle-only for now. The kernel's card-FSM signal
  (`anyCardNeedsInput`) is OR'd in once overlays are read; the sidebar and this
  clock must keep using the same predicate.
