// The conversation itself: a transcript and the box you write into.
//
// ── Why it does not look like a chat app ──────────────────────────────────
//
// **The reply is never in a bubble.** A bubble is a variable-width column, so
// the prose inside it has no measure — every turn wraps at a different width —
// and in the 364px this drawer actually has, a bubble with its own padding
// leaves ~320 and runs 45 characters a line against the 65–75 prose is read at.
// That argument is about the *long* text, so it binds the reply and only the
// reply: what you typed is usually one line, and it is the thing you scan back
// for rather than read.
//
// **Side carries "who".** Your turn is flush to the inline-end edge, the reply
// to the inline-start, and that is the mechanism. It is the strongest signal
// available and the cheapest: no ink, no shape, no width taken from the text.
// What it spends is the one flush left edge — which is why only *your* turn
// moves, and the reply, the long thing that is actually read, keeps the
// column's left edge and its full width. Your turn adds the faintest fill in
// the app on top of side; the recipe and the reasoning are in the stylesheet.
//
// **No per-turn labels, and no per-turn timestamps.** This is where the first
// version of this file was wrong, and it was wrong by measurement: in a strict
// alternation "YOU" and "AGENT" appeared eight times down four exchanges, and
// "now" eight times beside them. Sixteen lines of chrome restating the two
// facts the reader already had — that they are in this conversation, and that
// turns alternate. What carries "when" instead is a separator, printed only
// where the conversation actually stopped and started again
// (`CONVERSATION_GAP_MS`), which is the only time the answer is not "just now".
//
// The unit is the **exchange** — one thing you said and everything that came
// back — and the layout groups by it: tight inside, loose between.

import { useEffect, useLayoutEffect, useMemo, useRef, type ReactNode, type Dispatch, type SetStateAction } from 'react';
import { createPortal } from 'react-dom';
import {
  ChatComposer as AstryxChatComposer,
  ChatComposerInput,
  ChatSendButton,
  ChatSystemMessage,
  type ChatComposerTrigger,
  type ChatToolCallItem,
} from '@astryxdesign/core/Chat';
import { Code } from '@astryxdesign/core/Code';
import { Markdown } from '@astryxdesign/core/Markdown';
import { createStaticSource } from '@astryxdesign/core/Typeahead';

import { EdgeNavigator } from '../../../ui/edge-navigation/public.tsx';
import { observeResize } from '../../../ui/edge-navigation/resize.ts';
import { drawerSeamAround } from '../../../ui/drawer/public.tsx';
import { Icon } from '../../../ui/icon/public.tsx';
import { useState } from '../../../ui/state/public.ts';

import { foldQuietSyncs } from '../../../../../core/domain/conversation-quiet-sync.ts';
import {
  isLiveConversation, isQueuedConversationTurn, opensAfterGap, opensExchange,
  type Conversation, type ConversationActivity, type SendOutcome, type TranscriptEntry,
} from '../../../../../core/domain/conversation.ts';
import { QuietSyncFold } from './quiet-sync.tsx';
import styles from './thread.module.css';
import {
  ToolCallGroup, toolCallGroupShowsRunning, untouchedToolCallGroup, useToolCallFocus, withDetailOpen,
  type ToolCallGroupUi,
} from './activity-groups.tsx';
import {
  groupTranscriptActivities, keyTranscriptGroups, noTranscriptGroupKeys, type TranscriptGroupKeys,
} from '../../../../../core/domain/conversation-groups.ts';

export type ChatThreadProps = Readonly<{
  conversation: Conversation;
  /** Messages and the actions between them, in the order they happened. */
  turns: readonly TranscriptEntry[];
  /** True while a turn is in flight; the composer stays usable, the dot pulses. */
  pending?: boolean;
}>;

export function ChatThread({ conversation, turns, pending = false }: ChatThreadProps) {
  const live = pending || isLiveConversation(conversation.state);
  const lastTurn = turns[turns.length - 1];
  const endRef = useRef<HTMLDivElement | null>(null);
  /** The box every marker lookup starts from. It is not `.thread` itself
   *  because the stylesheet's `> * + *` rules space that element's children,
   *  and this wrapper is what keeps a marker search rooted above them without
   *  joining that list. It no longer holds the rail — see `railSeam`. */
  const frameRef = useRef<HTMLDivElement | null>(null);
  const exchanges = useMemo(() => exchangesOf(turns), [turns]);
  /*
   * #1667 D3 — the transcript is drawn by block, not by entry: a report-edit
   * wake and everything the planner did in answer to it is one folded line
   * (`QuietSyncFold`), everything else is one entry each. The grouping is
   * the domain's (`foldQuietSyncs`); this component only decides how a group
   * is painted. The index map is what keeps `opensExchange`, `opensAfterGap`
   * and the live-mark rule reading the SAME positions they read before:
   * they are stated over `turns`, and a fold changes nothing about where an
   * entry stands in the conversation, only about how it is drawn.
   */
  const blocks = useMemo(() => foldQuietSyncs(turns), [turns]);
  const indexOf = useMemo(
    () => new Map(turns.map((entry, index) => [entry, index] as const)), [turns],
  );
  // Quiet syncs remain a single top-level boundary; their existing disclosure
  // owns its children. Ordinary tools on either side must never join through it.
  const quietBlocks = useMemo(() => new Map(blocks
    .filter((block) => block.kind === 'quiet-sync').map((block) => [block.id, block] as const)), [blocks]);
  const visibleTurns = useMemo(() => blocks.map((block) => block.kind === 'entry'
    ? block.entry : block.entries[0]), [blocks]);
  /*
   * Which run of tool calls is which, carried from one transcript to the next
   * (`keyTranscriptGroups`). The memory it reads is the memory of the last
   * transcript that was *committed*, and it is advanced only by a commit —
   * never by the render that computed it. That is not a nicety: React renders
   * transcripts it then throws away (a transition that suspends and is
   * overtaken, a render the next update interrupts), and a memory advanced
   * during render is left holding keys assigned to a transcript no one saw.
   * The next render of the transcript that *is* on screen then reads that
   * memory, finds none of its calls in it, issues fresh keys, and every open
   * group is torn down and rebuilt closed. Idempotence on a repeat of the same
   * input does not cover this — the input that was abandoned was different.
   *
   * `useMemo` on `turns` is the render-time half: the keys are derived from
   * the transcript and the committed memory, both of which are stable across
   * the re-renders between two transcripts. The layout effect is the commit
   * half. Writing the memory in a layout effect rather than a passive one
   * puts it in the same commit as the DOM it describes; nothing renders in
   * between.
   */
  const committedGroupKeys = useRef<TranscriptGroupKeys>(noTranscriptGroupKeys());
  const keyedGroups = useMemo(
    () => keyTranscriptGroups(groupTranscriptActivities(visibleTurns), committedGroupKeys.current),
    [visibleTurns],
  );
  const transcriptGroups = keyedGroups.groups;
  useLayoutEffect(() => {
    committedGroupKeys.current = keyedGroups.memory;
  }, [keyedGroups]);
  /*
   * What the reader has done to each run of calls — open or closed, and which
   * failure details inside it they opened — by the run's carried key. Held
   * here, not in the vendor's element, because that element does not survive
   * a page shift that leaves the run one call, or none, and its rows do not
   * survive one that drops the call the reader was reading
   * (`activity-groups.tsx`). A key is issued once and never reissued, so an
   * entry for a run that vanished can never describe a later stranger — and
   * the run itself, remembered by its calls' ids (`keyTranscriptGroups`),
   * gets its entry back when they return; this component is remounted per
   * conversation (`key={open.id}` at the router), so nothing here crosses
   * from one transcript's runs to another's.
   */
  const [groupUi, setGroupUi] = useState<ReadonlyMap<string, ToolCallGroupUi>>(() => new Map());
  const updateGroupUi = (key: string, update: (previous: ToolCallGroupUi) => ToolCallGroupUi) => {
    setGroupUi((previous) => new Map(previous).set(key, update(previous.get(key) ?? untouchedToolCallGroup())));
  };
  /*
   * Where focus goes when the element under it goes — a call's row, or a
   * run's whole element, taken by the same page shift (`activity-groups.tsx`).
   * Held on the transcript's element, which outlives every run's, and fed by
   * its focus events; every child drawn below carries `data-nc-entry` so the
   * landing can find an entry's element again after the commit.
   */
  const focus = useToolCallFocus(
    transcriptGroups.filter(({ entry }) => entry.author !== 'turn' || entry.status !== 'completed'),
    (activities) => activities.map(toolCallOf),
    (key) => groupUi.get(key) ?? untouchedToolCallGroup(),
  );
  /*
   * Whether the end of the transcript already says "working", so the
   * placeholder mark below it stays down: an agent reply, a running call on
   * its own line, or a run of calls whose *visible* rows include a running
   * one — the header's latest call when the group is closed, every call when
   * it is open. The last call finishing does not end the turn, and a group
   * closed on a finished latest call has nothing spinning in it, so the
   * placeholder is right to come back then; but open, with an earlier call
   * still running, the vendor's spinner is already on screen and a second
   * mark under it would say the same thing twice.
   */
  const tail = transcriptGroups[transcriptGroups.length - 1];
  const tailQuiet = tail === undefined ? undefined : quietBlocks.get(tail.entry.id);
  const tailCarriesLiveMark = tail === undefined ? false
    : tailQuiet !== undefined ? tailQuiet.entries.some((entry) => entry === lastTurn)
    : tail.activities === null ? tail.entry.author === 'agent'
    : tail.activities.length === 1 ? tail.activities[0].state === 'running'
    : toolCallGroupShowsRunning(tail.activities.map(toolCallOf), groupUi.get(tail.key)?.expanded ?? false);
  /*
   * ── The rail is painted in the drawer's seam, not in the transcript ───────
   *
   * The dots used to sit in a 10px gutter cut out of the transcript's own
   * column, taken from the measure of every line of every reply. They are now
   * in the strip of page between the drawer's card and the window — 24px that
   * the card's `inset-inline-end` leaves empty at every viewport width, and
   * that nothing else in the app paints on. The transcript gives up nothing.
   *
   * **Only the DOM moves. None of the rules do.** The markers are still in the
   * pane, the lit-dot rule still measures them against the pane's own box, the
   * jump still writes the pane's `scrollTop`, and the roving stop, the
   * envelope and the preview are all still this component's. What changed is
   * where `ExchangeRail` renders, which is a `createPortal` and nothing else —
   * the smallest edit that gets the ink out of the column, and the one that
   * leaves every measurement reading the same rects it read before.
   *
   * **Why a portal rather than rendering it up at the router.** The rail's
   * state — which exchange is lit, where the roving stop is, what is being
   * previewed — is derived from the same `turns` the transcript is built from
   * and changes on the same scrolls. Rendering it as the drawer's sibling
   * would mean lifting every one of those to a component that has no other
   * reason to know about exchanges, and keeping it in step with a transcript
   * it no longer contains. The portal moves the pixels and leaves the
   * reasoning where the reasoning belongs.
   *
   * **`.drawer` is `overflow: hidden`, so a descendant cannot do this.** That
   * clip is what the card's corner radius cuts against; a rail parented
   * anywhere inside the card and reaching into the seam is simply not painted.
   * The seam is the card's *sibling*, handed down by `ui/drawer` — which is
   * also the only component that knows when the drawer is closing, and
   * therefore the only one that can make the rail leave with it.
   *
   * No seam, no rail: outside a drawer there is nowhere for it to be, and a
   * transcript rendered in place is a transcript without a jump list rather
   * than one with a jump list in the wrong box.
   *
   * **The seam is found, not passed**, by the same mechanism and for the same
   * reason as the drawer's scrolling pane two effects above: `closest()` off a
   * data attribute the drawer stamps. `ui/drawer`'s `drawerSeamAround` owns
   * both ends of that. It is held in state because a portal needs the node at
   * *render* time and a ref landing does not re-render anything; a layout
   * effect is what turns "the node is in the DOM" into "the node is a value",
   * and it costs the one commit in which the rail is not yet painted.
   */
  const [railSeam, setRailSeam] = useState<HTMLElement | null>(null);
  /* The frame is not rendered at all on an empty transcript, so the one edge
     that can bring it into existence under a live component is the first turn
     arriving. A remount covers every other way a different drawer could be
     above this transcript, and the router remounts on `key={open.id}`. */
  const hasTranscript = turns.length > 0;
  useLayoutEffect(() => {
    setRailSeam(drawerSeamAround(frameRef.current));
  }, [hasTranscript]);
  const railShown = exchanges.length > 0 && railSeam !== null;
  const [active, setActive] = useState<string | null>(null);
  /**
   * Re-derive the lit dot from the painted boxes, right now. Installed by the
   * rail effect below, so it is a no-op both before that effect has run and
   * wherever there is no layout to read (see the effect's own note).
   */
  const readActive = useRef<() => void>(() => {});
  /**
   * Whether the reader is parked at the end of the transcript, which is the
   * only state in which a newly appended turn may move the pane. Starts true
   * because a conversation opens at its newest turn.
   */
  const followsNewest = useRef(true);
  /** The turn at the end of the transcript as of this render — what the follow
   *  effect below both depends on and decides by. */
  const newestId = lastTurn?.id;
  /** The newest turn as of the last time the effect below ran, so that a change
   *  which did not put anything new at the end — *Load earlier* — is not
   *  mistaken for one that did. It records what the effect *saw*, not where the
   *  reader was taken: it is updated on every run, including the runs that
   *  decline to scroll because the reader is reading something earlier. Anything
   *  else would make one declined turn arm the next one. */
  const followedTo = useRef<string | undefined>(undefined);

  /*
   * ── Follow the newest turn, but only for a reader who is already there ────
   *
   * This effect used to write `scrollTop = scrollHeight` on every change of
   * `turns.length`, unconditionally, and the comment that stood here claimed
   * the count-keying "protects someone reading back through the thread". That
   * was wrong, and it is worth writing down why rather than quietly deleting:
   * keying on the count only suppresses re-renders that change *nothing*. An
   * append that reaches this component as an extra entry changes the count, so
   * it fired the write — and one that does not, because the domain collapsed it
   * into the entry before it, is the case the dependency below now also covers.
   * A live turn appends an activity line per `item/started`/`item/completed`,
   * dozens over a four-minute turn, so "go back and re-read the second exchange
   * while the agent works" survived about one poll — the browser tier's
   * "leaves the pane where the reader put it" case is that failure, held down.
   *
   * So the write is now conditional on the reader being at the bottom already,
   * which is the standard behaviour of every transcript that appends. The flag
   * is maintained from the pane's own scroll events rather than measured here,
   * and that ordering is the point: by the time this effect runs the new row is
   * already in the DOM, so `scrollHeight` has grown and a reader who *was* at
   * the bottom would measure as no longer being there. What the flag records is
   * where the reader last put themselves.
   *
   * `FOLLOW_BOTTOM_SLACK_PX` is what "at the bottom" tolerates: a few pixels of
   * subpixel rounding, and the reader who nudged the wheel once without meaning
   * to leave.
   *
   * **A change in `turns.length` is not the same fact as a turn arriving**, and
   * the difference is *Load earlier*. Prepending history grows the count while
   * the newest turn stays exactly what it was, so the write fired for it too —
   * and on a transcript that fits in its pane the flag is unconditionally true
   * (the distance to the bottom of a pane that cannot scroll is zero, whatever
   * the reader does), so the reader who asked to see older messages was thrown
   * to the bottom of the conversation instead of shown what loaded. So the
   * write is gated on the last turn's id having changed since the last time
   * this effect took the reader anywhere.
   *
   * **And the last turn's id is a dependency, because it changes on its own.**
   * An earlier round keyed this effect on `turns.length` alone and argued that
   * "a transcript whose length has not changed has nothing for either to do".
   * That is false, and this repository's own domain layer falsifies it on the
   * ordinary path: `buildTranscript` collapses a trailing `Thought` into the
   * reply that answers it, so `[reasoning, reasoning]` and
   * `[reasoning, reasoning, agentMsg]` are both one entry long with *different*
   * last ids (`core/domain/conversation.ts`, and its own test). `mergeTranscript`
   * does the same to an optimistic echo. Keyed on the count, the commonest
   * arrival there is — the agent answering while its last row is a finished
   * thought — never re-ran this effect at all, so a reader parked at the bottom
   * was not taken to the answer they were waiting for.
   *
   * **A pane whose height changes moves the reader without a scroll event.**
   * The composer shrinks as a draft is sent, and the window resizes; either can
   * bring a parked reader within `FOLLOW_BOTTOM_SLACK_PX` of the bottom, or
   * carry them out of it, with no `scroll` for the listener to hear. So the
   * same measurement runs from a `ResizeObserver` on the pane. Without it the
   * flag could sit `false` for the rest of the session while the reader was
   * plainly at the end, and live output stopped following.
   *
   * Scroll only the drawer's own pane. `scrollIntoView` walks every ancestor
   * scrollport, and `.main` used to be one — opening a conversation then
   * panned the page toward the centre for a frame. The pane is marked
   * `data-nc-drawer-scroll`; tests that stamp the marker see the `scrollTop`
   * write, and a missing marker is a silent no-op.
   */
  useEffect(() => {
    const end = endRef.current;
    if (end == null) return;
    const scroller = end.closest<HTMLElement>('[data-nc-drawer-scroll]');
    if (scroller == null) return;
    const arrived = newestId !== followedTo.current;
    followedTo.current = newestId;
    if (arrived && followsNewest.current) scroller.scrollTop = scroller.scrollHeight;
    const measure = () => {
      followsNewest.current = scroller.scrollHeight - scroller.scrollTop
        - scroller.clientHeight <= FOLLOW_BOTTOM_SLACK_PX;
    };
    scroller.addEventListener('scroll', measure, { passive: true });
    const unobserve = observeResize(scroller, measure);
    return () => {
      scroller.removeEventListener('scroll', measure);
      unobserve();
    };
  }, [turns.length, newestId]);

  /*
   * ── Which dot is lit: the last exchange that has scrolled past the top ────
   *
   * **The rule.** The lit exchange is the *last* one whose opening marker sits
   * at or above a horizontal edge inside the pane. In words: the most recent
   * heading you have read past. If none has — you are at the very start of the
   * transcript — it is the first.
   *
   * That edge is the pane's top (within `ACTIVE_MARKER_SLACK_PX`) for most of
   * the scroll and **slides down toward the pane's bottom through the last
   * pane-height**; "past the top" is therefore the rule's shape rather than its
   * whole statement, and the slide is argued for in full below. Everything in
   * the next three paragraphs is about the top-edge form, which is what the
   * rule is wherever there is more than a pane-height of scroll left.
   *
   * This replaces "the topmost opening marker currently intersecting the
   * pane", and that earlier rule's long argument for an `IntersectionObserver`
   * is gone with it rather than left standing, because the observer was not
   * merely a mechanism for a rule that worked: it was the wrong *trigger*, and
   * the rule it fed was wrong in three ordinary positions.
   *
   *   1. Only your line carries `data-nc-exchange`; the reply that follows is
   *      a sibling and is not observed at all. So with exchange 2's reply
   *      filling the top of the pane and exchange 3's question just peeking in
   *      at the bottom, "topmost intersecting marker" is 3 — while every word
   *      on screen belongs to 2.
   *   2. An exchange taller than the pane has *no* marker intersecting while
   *      you read its middle. The observer then does not fire at all, so the
   *      mark froze wherever it was — including at "nothing lit" when a
   *      conversation opened on a tall final reply.
   *   3. Pressing a dot near the end scrolls to a position the browser clamps
   *      to the maximum, and the old rule then overruled the press with some
   *      earlier dot. Measured on nine exchanges with three short trailing
   *      replies (`railTurns(9, 6)` in the browser tier): press the ninth dot,
   *      the sixth lights.
   *
   * The rule above answers all three from one idea, and it is a *position*
   * question rather than a *visibility* question — which is why the trigger is
   * now the pane's own `scroll` event, throttled to a frame, rather than an
   * observer. An observer reports crossings; this rule has to be evaluated at
   * every scroll offset, including the offsets where nothing crosses anything
   * (case 2 is exactly that). Reading `getBoundingClientRect()` off every
   * marker once a frame is a few dozen reads on the longest transcript anyone
   * keeps, in a handler that already ran because the compositor moved.
   *
   * **The end of the scroll is where the rule has to bend, and it bends
   * gradually.** Once the pane is at its maximum offset no further marker can
   * *ever* be brought to the top, so for the trailing exchanges the question
   * "which one did I scroll past" has no answer and the rule would freeze on
   * whichever one happened to reach the top last. There the honest answer is
   * the last exchange that has *started* on screen — the pane's bottom edge,
   * not its top. That is also what makes pressing the final dot light the final
   * dot, and what lights something rather than nothing when a conversation
   * opens on a reply taller than the pane.
   *
   * The first version of that switched between the two edges on a one-pixel
   * threshold, and the switch is worth recording because it looked harmless and
   * was not: the two edges are a whole pane-height apart, so *any* threshold
   * makes the answer jump by a pane's worth of transcript. Measured on
   * `railTurns(9, 6)` in a 400px pane: one pixel short of the maximum lit the
   * ninth dot, two pixels short lit the sixth — three exchanges, back and
   * forth, on scroll deltas a trackpad emits continuously. So the edge now
   * *slides*: it is the top edge while at least a pane-height of scroll
   * remains, and it travels down to the bottom edge as that last pane-height
   * runs out. Both ends are exactly the behaviour argued for above, and no
   * offset in between moves the mark discontinuously. `ACTIVE_MARKER_SLACK_PX`
   * doubles as the tolerance for the fractional pixel the engine may leave
   * behind at maximum scroll, so no separate end-of-scroll epsilon is needed.
   *
   * **The slide is also capped by how far the pane has actually travelled**,
   * and that cap is what keeps every transcript on the top edge at the top of
   * *itself*. Without it the slide is keyed on the remaining scroll alone, and
   * at `scrollTop === 0` "remaining" is the transcript's whole overflow — so
   * every transcript overflowing by less than one pane-height opened already
   * slid by `paneHeight − overflow`. Measured before the cap, six short
   * exchanges in a pane 60px shorter than their transcript: the fifth dot lit
   * at the top of a conversation nobody had begun reading, and pressing the
   * first dot lit the sixth, because the press writes a `scrollTop` the engine
   * clamps and the re-read below then puts that answer back. That band includes the
   * first thing this rail ever does for anyone — it appears at five exchanges,
   * and five exchanges in this drawer is one to two panes tall.
   *
   * **What the slide costs, stated because the algebra is not on the page.**
   * Wherever it is moving — the last pane-height of a tall transcript, and the
   * whole scroll of one that overflows by less than a pane — the edge descends
   * at 1px per 1px of scroll *while the content rises at the same rate*, so the
   * comparison point sweeps the document at **twice** the scroll rate. Measured
   * (`railTurns(30, 0)`, 400px pane): one dot per ~85px of scroll for the first
   * 2200px, then one per 40px over the last 355px — seven exchanges crossing in
   * a final pane-height that only holds four. It is bounded by one pane-height,
   * it is monotonic (both terms move the same way, so there is no flicker and
   * no offset that maps to two answers), and the region it is confined to is
   * exactly the one where "which did I scroll past" has no answer anyway. It is
   * visible on the last screenful; it is not a wrong answer.
   *
   * **Where there is no layout, there is nothing to read.** `read()` stops at a
   * pane reporting zero height. A `display: none` ancestor produces exactly
   * that, through the `ResizeObserver` as well as through `scroll`, and without
   * the guard those observations moved the mark to the last exchange — every
   * marker reports a top of 0, so every one of them is "at or above the edge".
   * (The guard used to have a second job, and it is gone with the mechanism:
   * `read()` also published `--nc-rail-room`, and a zero-height observation
   * published a `0px` bound that collapsed the rail's track. The track is
   * bounded by the seam in CSS now and there is nothing left to publish, so what
   * the guard protects is the mark and only the mark.) The standing case is the
   * web-dom tier: jsdom computes no boxes at all, so every marker would report a
   * top of 0 and the rule would return a confident answer about a page that was
   * never laid out. There the rail still renders and still jumps, and the mark
   * is answered by the press alone.
   *
   * **The guard is in `read()` and nowhere else, and that placement is the
   * point.** It stood at the top of this effect too, before the listeners were
   * attached, which turned a pane that merely *mounted* at zero height into a
   * rail that never worked again: nothing re-runs this effect when the drawer
   * gains its height (the exchanges did not change), so the observer that would
   * have noticed was never installed. Measured at 0 → 400px followed by a real
   * scroll: no dot lit, ever. So the listeners go on unconditionally and every
   * path through them asks `read()`, which is the one place that knows whether
   * there is a box to measure.
   */
  const exchangeKey = JSON.stringify(exchanges.map((exchange) => exchange.id));
  useEffect(() => {
    const frame = frameRef.current;
    if (!railShown || frame === null) return;
    const scroller = frame.closest<HTMLElement>('[data-nc-drawer-scroll]');
    if (scroller === null) return;
    const markers = [...frame.querySelectorAll<HTMLElement>('[data-nc-exchange]')];
    if (markers.length === 0) return;

    /*
     * ── This effect no longer publishes anything, and that is the whole of ──
     *    what moving the rail out of the pane bought
     *
     * Two custom properties used to be measured here on every scrolled frame
     * and written onto the frame: `--nc-rail-room`, the distance from the
     * track's own top to the pane's bottom, and `--nc-rail-reach`, the width of
     * transcript the preview was allowed to float over. Both existed for one
     * reason — the rail was *inside the scrolling pane*, stuck to it with
     * `position: sticky`, so its own top edge moved as the reader scrolled and
     * no rule in the stylesheet could name where it had got to. The long
     * argument that stood here (sticky does not lift an element to its inset;
     * a track sized by the pane's height overhangs the drawer by the 36px of
     * close-clearance; a *Load earlier* button moves the flow position again;
     * the republication is one wheel notch behind a body that grows) was
     * entirely about that situation, and the situation is gone. It is deleted
     * rather than kept, because every sentence in it is now false.
     *
     * The rail is in the drawer's seam. The seam is an absolutely-positioned
     * box with `inset-block: var(--space-9) var(--space-11)` — its height is a
     * CSS fact, not a measured one, so the track bounds itself with a plain
     * `max-block-size: 100%` and the preview caps itself against `--panel-span`
     * and the container. Nothing scrolls under the rail any more, so there is
     * nothing to republish per frame.
     *
     * What is left in this effect is the lit-dot rule and nothing else, and its
     * arithmetic is untouched: it reads the *pane's* box and the *markers'*
     * boxes, both of which are still in the same scrollport they always were.
     * The rail's departure cannot reach it, because the rail was never one of
     * the rects it reads.
     */
    const read = () => {
      if (scroller.clientHeight === 0) return;
      const pane = scroller.getBoundingClientRect();
      /* How far the pane can still travel, and how far the edge has therefore
         slid from the top one toward the bottom one — capped by how far the
         pane has actually been scrolled, which is what keeps every transcript
         on the top edge at the top of itself. */
      const remaining = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight;
      const slid = Math.min(scroller.scrollTop, Math.max(0, pane.height - remaining));
      const edge = Math.min(pane.bottom, pane.top + ACTIVE_MARKER_SLACK_PX + slid);
      let current = markers[0];
      for (const marker of markers) {
        if (marker.getBoundingClientRect().top > edge) break;
        current = marker;
      }
      const id = current?.dataset.ncExchange;
      if (id !== undefined) setActive(id);
    };
    readActive.current = read;

    let queued: number | null = null;
    const onScroll = () => {
      if (queued !== null) return;
      queued = requestAnimationFrame(() => { queued = null; read(); });
    };
    /* The pane's resize is a trigger as well as its scroll: a drawer that grows
       or shrinks moves every marker relative to the pane's edges without
       emitting a `scroll`, and the rule is a question about exactly those
       positions. */
    const unobserve = observeResize(scroller, read);
    scroller.addEventListener('scroll', onScroll, { passive: true });
    read();
    return () => {
      readActive.current = () => {};
      unobserve();
      scroller.removeEventListener('scroll', onScroll);
      if (queued !== null) cancelAnimationFrame(queued);
    };
  }, [railShown, exchangeKey]);

  /* One entry of the transcript, in the position `turns` gives it. Hoisted
     out of the block loop below so a folded group and a bare entry draw the
     same thing the same way; `index` is the entry's place in `turns`, which
     is what the exchange, gap and live-mark rules are stated over. */
  const renderEntry = (turn: TranscriptEntry, key = turn.id, showLive = live): ReactNode => {
    const index = indexOf.get(turn) ?? -1;
    const last = index === turns.length - 1;
    if (turn.author === 'activity') {
      return <ActivityLine key={turn.id} entry={key} activity={turn} live={showLive && last} />;
    }
    if (turn.author === 'system') {
      return (
        <div key={turn.id} data-nc-entry={key}>
          {opensAfterGap(turns, index) && index > 0 && (
            <p className={styles.gap}>{clockTime(turn.atMs)}</p>
          )}
          <details
            className={styles.system}
            data-nc-turn="system"
          >
            <summary className={styles.systemSummary} title={turn.text}>
              <span className={styles.systemDisclosure} aria-hidden="true">›</span>
              <span className={styles.systemLabel} data-nc-system-label="">
                · {turn.label} ·
              </span>
            </summary>
            <p className={styles.systemDetail}>{turn.text}</p>
          </details>
        </div>
      );
    }
    if (turn.author === 'turn') {
      /*
       * #1625 P1 — how the turn ended, and only when it did not end
       * well. A `completed` outcome is in `turns` as an anchor and
       * paints nothing at all: the reply above it already says the turn
       * finished. `interrupted` and `failed` are the two facts the
       * transcript cannot otherwise show — it just goes quiet either way.
       *
       * Astryx's `ChatSystemMessage` is a leaf (no scroll, no measure)
       * and does not forward `data-*`, so the state hooks sit on this
       * wrapper. Its content span is `nowrap`, which is right for the
       * one-word label and wrong for an error sentence, so the message
       * is its own block below. No label, no time: the "why it stopped"
       * is the whole content, as the file header argues for every turn.
       */
      if (turn.status === 'completed') return null;
      return (
        <div
          key={turn.id}
          className={styles.outcome}
          data-nc-entry={key}
          data-nc-turn="outcome"
          data-nc-turn-outcome={turn.status}
        >
          <ChatSystemMessage>{turn.status === 'interrupted' ? 'Stopped' : 'Failed'}</ChatSystemMessage>
          {turn.status === 'failed' && turn.message !== undefined && turn.message !== '' && (
            <p className={styles.outcomeDetail} data-nc-turn-outcome-message="">{turn.message}</p>
          )}
          {turn.status === 'failed' && (
            <OutcomeHint code={turn.code} rawStatus={turn.rawStatus} />
          )}
        </div>
      );
    }
    const opens = opensExchange(turns, index);
    return (
      <div
        key={turn.id}
        className={opens ? styles.exchange : undefined}
        data-nc-entry={key}
        /* The same element the layout already groups by is the element the
           rail jumps to. There is no second notion of "an exchange starts
           here" to keep in step with `opensExchange`. */
        {...(opens ? { 'data-nc-exchange': turn.id } : {})}
      >
        {/* A time only where the conversation restarted. */}
        {opensAfterGap(turns, index) && index > 0 && (
          <p className={styles.gap}>{clockTime(turn.atMs)}</p>
        )}
        {turn.author === 'you' ? (
          <>
            {/*
              * The mark is on the turn and the words are under it, rather
              * than inside the `<p>`: the paragraph is the message
              * verbatim, and a caption folded into it would become part
              * of the message's own text — to a screen reader reading the
              * paragraph, and to every `getByText` that matches one.
              */}
            <p
              className={styles.said}
              data-nc-turn="you"
              {...(isQueuedConversationTurn(turn) ? { 'data-nc-queued': '' } : {})}
            >{turn.text}</p>
            {/*
              * #1505 S6 — the images that went with what was said.
              *
              * Rendered here rather than by `features/planner`, which
              * this module may not import (`features-no-cross-domain`),
              * and rendered from the url the server built rather than
              * from a path assembled here.
              *
              * `alt=""` and `aria-hidden` on the list: the image is the
              * message's own content and the transcript has no
              * description of it to offer, so announcing "image" once
              * per thumbnail would add noise without adding a fact. The
              * count is said once, in text, above them.
              */}
            {(turn.attachments ?? []).length > 0 && (
              <ul className={styles.attachments} data-nc-turn-attachments="">
                {(turn.attachments ?? []).map((attachment) => (
                  <li key={attachment.id} className={styles.attachment}>
                    <img src={attachment.url} alt="" />
                  </li>
                ))}
              </ul>
            )}
            {isQueuedConversationTurn(turn) && (
              /*
               * What separates "the agent is working on this" from "the
               * agent has not seen this yet", said in words because
               * nothing else on this surface says it. The live dot below
               * belongs to the turn already running, and a queued message
               * sits above it looking exactly like one being answered.
               *
               * `role="status"` rather than a bare caption: it appears
               * without the reader having moved, in response to their own
               * press, and it is the answer to "did that go anywhere?".
               */
              <p className={styles.queuedNote} data-nc-queued-note="" role="status">
                Queued · sends when this turn ends
              </p>
            )}
          </>
        ) : (
          <div className={styles.reply} data-nc-turn="agent">
            <Reply text={turn.text} />
            {showLive && last && <span className={styles.live} aria-label="Working" />}
          </div>
        )}
      </div>
    );
  };

  if (turns.length === 0) {
    return (
      <div className={styles.empty} data-nc-thread-empty="">
        <p className={styles.emptyLead}>{live ? 'The agent is working.' : 'Nothing said yet.'}</p>
        <p className={styles.emptyHint}>{live ? 'Messages will appear here.' : 'Write below and it starts here.'}</p>
        {live && <span className={styles.live} aria-label="Working" />}
      </div>
    );
  }

  return (
    <div className={styles.threadFrame} ref={frameRef}>
      {/* One class, whether or not the rail is up: the transcript's geometry no
          longer depends on it at all. The frame used to switch to a two-column
          grid here and give the first column away to the dots; the seam is not
          the transcript's to give, so there is nothing to switch. */}
      {railShown && railSeam !== null && createPortal(
        <EdgeNavigator
          className={styles.edgeNavigation}
          label="Jump to an exchange"
          items={exchanges.map((item, index) => ({ ...item,
            label: `Jump to exchange ${index + 1}${railLabel(item.text) === '' ? '' : `: ${railLabel(item.text)}`}`,
          }))}
          activeId={active}
          onSelect={(id) => {
            /* Nothing moved, nothing is marked. The lookup comes first so that
               a marker which is not there leaves the whole control untouched —
               it used to set the mark and then discover it had nowhere to
               scroll, which lit a dot for an exchange the reader was not taken
               to. */
            if (!jumpToExchange(frameRef.current, id)) return;
            /* The mark moves on the press, so the dot answers in the same frame
               it was pressed — and then the rule re-reads the boxes the jump
               just moved, so the mark ends up agreeing with what the pane is
               actually showing.

               The re-read is not redundant with the scroll event the write
               fires, and the case that separates them is a write the engine
               clamps to the offset the pane is *already* at: no movement, so no
               `scroll`, so nothing else would ever correct the press. A
               transcript shorter than its pane is that case for every dot —
               press the fourth of five fully visible exchanges and the honest
               answer is still the first, because nothing moved and nothing was
               scrolled past. Where there is no layout (web-dom) this call is a
               no-op and the press is the whole answer. */
            setActive(id);
            readActive.current();
          }}
        />,
        railSeam,
      )}
      <div className={styles.thread} data-nc-thread="" ref={focus.ref} onFocus={focus.onFocus} onBlur={focus.onBlur}>
        {transcriptGroups.map(({ entry: turn, activities, key }) => {
          const block = quietBlocks.get(turn.id);
          if (block !== undefined) {
            return (
              <div key={block.id} data-nc-entry={key}>
                <QuietSyncFold group={block} time={clockTime(block.atMs)}
                  live={live && block.entries.some((entry) => entry === lastTurn)}>
                  {block.entries.map((entry) => renderEntry(entry, entry.id, false))}
                </QuietSyncFold>
              </div>
            );
          }
          const last = (activities?.at(-1) ?? turn) === lastTurn;
          if (turn.author === 'activity') {
            if (activities !== null && activities.length > 1) {
              return (
                <ToolCallGroup
                  /* Carried by membership, not read off any one call: the group
                     keeps its element — and the state below is keyed the same
                     way, so it keeps what the reader did to it even where the
                     element cannot be kept — as calls are appended to it, as
                     *Load earlier* prepends the rest of a run the page cut
                     through, as a page shift drops its head, and as a running
                     call finishes in place. */
                  key={key}
                  entry={key}
                  calls={activities.map(toolCallOf)}
                  ui={groupUi.get(key) ?? untouchedToolCallGroup()}
                  onExpandedChange={(expanded) => updateGroupUi(key, (ui) => ({ ...ui, expanded }))}
                  onDetailOpenChange={(callKey, open) => updateGroupUi(key, (ui) => withDetailOpen(ui, callKey, open))}
                  /* The same `live && last` a lone line answers to. A group
                     the conversation has moved past — a user message and a
                     new call after it — keeps a call left `running` by an
                     interrupted turn, and must not look like it resumed. */
                  live={live && last}
                />
              );
            }
            return <ActivityLine key={turn.id} entry={key} activity={turn} live={live && last} />;
          }
          return renderEntry(turn, key);
        })}
        {/* A reply that has not arrived yet still gets a place to arrive in. The
            tail owns the live mark only when it is an agent reply, a running
            action on its own line, or a group with a running call among the
            rows it is showing (`tailCarriesLiveMark`); otherwise this
            placeholder keeps the one mark visible. */}
        {live && !tailCarriesLiveMark && (
          <p className={styles.reply}><span className={styles.live} aria-label="Working" /></p>
        )}
        <div ref={endRef} aria-hidden="true" />
      </div>
    </div>
  );

}

/** What the envelope's scheduler ref holds before the effect has installed one
 *  and after it has torn one down: calling it is how the exchange-set trigger
 *  stays safe on the first render and on the way out. */
/** One thing you said and everything that came back — as far as the rail needs
 *  it: something to point at, and the words to call it by. */
type Exchange = Readonly<{ id: string; text: string }>;

/**
 * How far below the pane's top edge a marker may still count as "scrolled
 * past". It absorbs the subpixel difference between the scroll `jumpToExchange`
 * asks for and the one the engine performs — without it a jump can land the
 * pressed marker a fraction of a pixel below the edge and light its
 * predecessor. It does the same job at the other end, where the engine's
 * achievable maximum offset can fall a fraction short of
 * `scrollHeight - clientHeight`: the sliding edge overshoots the pane's bottom
 * by this much before being clamped to it.
 *
 * **What it does not do is nothing.** It is a 4px shift of the comparison
 * edge, so it moves the answer at the offsets where a marker is within 4px of
 * that edge — measured against a 0px slack, the mark changes 1–2 scroll pixels
 * earlier at a handful of offsets. What is claimed for it is only the scale:
 * 4px is a sixth of the reply's 24.75px line box, so it can bring the *next*
 * exchange forward by a pixel or two of scroll and cannot skip one.
 */
const ACTIVE_MARKER_SLACK_PX = 4;

/**
 * How far from the bottom still counts as reading the newest turn.
 *
 * Between two and three lines of the reply's serif — `.reply` sets
 * `--text-md` (15px) at `--leading-loose` (1.65), a 24.75px line box, so 64px
 * is 2.6 of them.
 * Below that a reader has not left — they nudged a wheel, or the engine rounded
 * — and yanking them back to the bottom on the next append is what they
 * expected anyway. Above it they went somewhere on purpose, and the transcript
 * must stay where they put it.
 */
const FOLLOW_BOTTOM_SLACK_PX = 64;

/**
 * Watch an element's box, where the platform has an observer to watch it with.
 *
 * All three callers — the follow effect's `measure`, the rail's `read`, and the
 * track's own `keepInRailView` — re-measure geometry the engine owns, and all
 * three run in the web-dom tier, where jsdom provides no `ResizeObserver` at
 * all (checked: jsdom 29) and computes no boxes for one to report. The feature
 * check is here, once, rather than folded into any caller's zero-height guard:
 * those guards are about whether there is layout to read, and this one is about
 * whether the platform can tell us it changed.
 */


/**
 * One plain sentence for the four `codexErrorInfo` values a reader can act on
 * (#1625 P1). Every other code is shown as the token codex sent — `other`,
 * `internalServerError`, `httpConnectionFailed` — because a made-up sentence
 * for a code this app does not understand would be a guess dressed as a fact.
 * A status this app does not know at all (`rawStatus`) is said as such.
 */
const FAILURE_HINTS: Readonly<Record<string, string>> = Object.freeze({
  contextWindowExceeded: 'The conversation no longer fits in the model’s context window.',
  usageLimitExceeded: 'The usage limit for this account has been reached.',
  rateLimitExceeded: 'Requests are being rate-limited; try again in a moment.',
  serverOverloaded: 'The model provider is overloaded; try again in a moment.',
});

function OutcomeHint({ code, rawStatus }: { code: string | undefined; rawStatus: string | undefined }) {
  const hint = code === undefined ? null : (FAILURE_HINTS[code] ?? code);
  if (hint === null && rawStatus === undefined) return null;
  return (
    <p className={styles.outcomeDetail} data-nc-turn-outcome-hint="">
      {hint ?? `Ended with status “${rawStatus}”`}
    </p>
  );
}

function exchangesOf(turns: readonly TranscriptEntry[]): readonly Exchange[] {
  const found: Exchange[] = [];
  turns.forEach((turn, index) => {
    /* `opensExchange` already implies `author === 'you'`; the narrowing below is
       for the type checker, which cannot read that from the domain function. */
    if (!opensExchange(turns, index) || turn.author !== 'you') return;
    found.push({ id: turn.id, text: turn.text });
  });
  return found;
}

/** How much of a prompt a name may carry. Long enough to tell two questions
 *  about the same file apart, short enough that a screen reader does not read a
 *  paragraph to announce a button. */
const RAIL_LABEL_MAX = 60;

/** The prompt as a button may carry it, or `''` where there is nothing to
 *  carry — the ordinal that names it either way is the caller's. */
function railLabel(text: string): string {
  /* Line breaks are the author's and the transcript keeps them; a button's name
     is a single line, so they collapse here and only here. */
  const line = text.replace(/\s+/g, ' ').trim();
  return line.length <= RAIL_LABEL_MAX ? line : `${line.slice(0, RAIL_LABEL_MAX - 1)}…`;
}

/**
 * Put the exchange at the top of the drawer's pane — **by writing that pane's
 * `scrollTop`, and never by `scrollIntoView`**.
 *
 * This is the same rule, for the same reason, as the follow-the-newest-turn
 * effect in `ChatThread`, and it is written out again rather than shared
 * because the two do different arithmetic: that one goes to the bottom, this
 * one goes to a marker. `scrollIntoView` walks *every* ancestor scrollport and
 * scrolls each one, so it does not merely move the transcript — it pans
 * whatever else the drawer happens to be sitting inside. That has been fixed
 * twice already (b1481da2, 3f51ea50) and both times the symptom was the page
 * shifting under a reader who asked for something inside the drawer.
 *
 * The delta is read off painted boxes rather than off `offsetTop`, because the
 * marker is several boxes deep inside the pane and `offsetTop` is relative to
 * whichever ancestor happens to be positioned — a fact about the stylesheet,
 * not about the transcript.
 *
 * A marker that is not there, or a pane that is not there, is a silent no-op:
 * the rail is a way of moving faster through something you can already reach by
 * scrolling, so there is nothing to report and nothing to fall back to. What it
 * returns is **whether there was somewhere to go** — both boxes found and a
 * `scrollTop` written — and not whether the pane actually moved: the engine
 * clamps that write, and a press near either end legitimately asks for an
 * offset it is already at. Distinguishing the two is the caller's re-read, not
 * this return value; the failure the return value exists to stop is a dot that
 * lights for an exchange the reader was never taken to *because it is not
 * there*, which is a different thing from a jump that had nowhere left to go.
 */
function jumpToExchange(frame: HTMLElement | null, id: string): boolean {
  if (frame === null) return false;
  const marker = [...frame.querySelectorAll<HTMLElement>('[data-nc-exchange]')]
    .find((candidate) => candidate.dataset.ncExchange === id);
  if (marker === undefined) return false;
  const scroller = marker.closest<HTMLElement>('[data-nc-drawer-scroll]');
  if (scroller === null) return false;
  scroller.scrollTop += marker.getBoundingClientRect().top - scroller.getBoundingClientRect().top;
  return true;
}

/**
 * ── The reply is markdown, and what that costs ────────────────────────────
 *
 * It used to be `{turn.text}` inside a `<p>` with `white-space: pre-wrap`, and
 * that was a lie about what the agent writes: the thing on the other end of
 * this drawer is the same one that writes the report, and it answers in
 * headings, lists and fenced code. All of it arrived as one flat paragraph
 * with the hashes and backticks still in it.
 *
 * **Why Astryx's `Markdown` and not a markdown library.** It is already a
 * dependency, and it carries its own parser and its own `CodeBlock` (fences are
 * rendered through it automatically — `Markdown.tsx:1147-1166`), so nothing new
 * is installed for either.
 *
 * ── `isStreaming` is deliberately **not** passed, and the first draft of this
 *    note was wrong about why it should be ─────────────────────────────────
 *
 * That draft called it "incremental parsing with a per-chunk fade" and argued
 * it was load-bearing on a live turn. It is not incremental parsing. Read from
 * the vendor: `isStreaming` routes the text through `useStreamingText`, which
 * is a **character-by-character typewriter** — `CHARS_PER_TICK.natural = 10` at
 * a rAF tick derived from `--duration-fast-min` (~13ms), i.e. it *withholds*
 * text the component already has and reveals it at ~770 chars/s, snapping to
 * the full string only when the flag goes false.
 *
 * Three reasons that is the wrong clock for this transcript, in the order they
 * matter:
 *
 *  1. **We already have a clock, and it is the poll.** Text arrives from
 *     `harness/items` in poll-sized jumps. A typewriter on top is a second,
 *     slower clock in front of the first, so a 2000-character answer keeps
 *     revealing for ~2.6 seconds *after* it has entirely arrived.
 *  2. **It grows a box inside a scrollport that three mechanisms measure.**
 *     The follow-the-newest effect reads `scrollHeight`, and the lit-dot rule
 *     re-reads every marker's rect on scroll and on resize. A block that grows
 *     every frame for seconds is a resize storm aimed at exactly the machinery
 *     the rest of this file spends its length getting right.
 *  3. **It splits the text into `<span>`s while it plays** (`wrapTextWithFade`),
 *     so the reply is not one text node until the animation ends. Measured:
 *     `wave-conversation.test.tsx`'s `[G5]` — an upstream case this file never
 *     touches — fails on `findByText('it runs tracks')` with Testing Library's
 *     "the text is broken up by multiple elements" hint.
 *
 * The fade is a real feature for a consumer holding a token stream. We are not
 * one, and pretending to be costs all three of the above to buy an animation
 * our data cannot drive smoothly anyway.
 *
 * **The cost, stated rather than discovered later: whitespace is now
 * CommonMark's, not the author's.** A single newline inside a paragraph is a
 * soft break — it renders as a space (`Markdown/parser.ts:388-404`: a hard
 * break needs two trailing spaces). Before this, `pre-wrap` printed every
 * newline exactly where it was written. Prose typed with single returns and no
 * blank line between them therefore reflows into one paragraph. That is the
 * whitespace contract of the language we are now speaking, and the alternative
 * — rewriting single newlines into hard breaks before handing the string over
 * — cannot be done without knowing which of them are inside a fence, which is
 * re-implementing the parser we just adopted in order to feed it.
 *
 * `core/domain/conversation.ts` still says the text is verbatim, and it still
 * is: what changed is the renderer, not the transport.
 *
 * **Only the reply.** What *you* typed stays a plain `<p>`: `*` and `#` in
 * something a person typed into a chat box are punctuation, not syntax, and a
 * composer that silently reinterprets what you sent is worse than one that
 * shows it back.
 *
 * `headingLevelStart={3}` because the page owns `<h1>` and its sections own
 * `<h2>`; a reply's own `#` is a heading inside a drawer, not a second page
 * title. Astryx clamps anything past `h6`.
 */
function Reply({ text }: { text: string }) {
  return <Markdown density="compact" headingLevelStart={3}>{text}</Markdown>;
}

/**
 * A duration is only worth printing when it is a duration the reader felt.
 *
 * Every `item/completed` carries `durationMs`, and most of them are a
 * `calm.report.read` that took 12ms. Printing those puts a number on nearly
 * every line of the transcript and says nothing on any of them — the same
 * budget argument the `.activity` stylesheet note makes about the line itself.
 * A second is the floor because a second is roughly where "that took a while"
 * starts being a thing the reader noticed happening.
 */
const ACTIVITY_DURATION_FLOOR_MS = 1_000;

/**
 * `4.3s` under a minute, `3m 12s` over it — the seconds zero-padded so the
 * two-part form does not read as `3m 2s` for a shorter interval than `3m 12s`.
 *
 * The branch is decided on the number **as it will be read**, not as it
 * arrived. Deciding on the raw milliseconds puts everything in
 * `[59_950, 60_000)` on the sub-minute side, where `toFixed(1)` rounds it to
 * `60.0s` — a reading that is exactly what having two formats exists to avoid,
 * printed one millisecond away from `1m 00s`. Rounding to tenths first and
 * testing *that* means the minute form takes over at the instant the seconds
 * form would have said sixty.
 */
function formatActivityDuration(durationMs: number): string {
  const tenths = Math.round(durationMs / 100);
  if (tenths < 600) return `${(tenths / 10).toFixed(1)}s`;
  const seconds = Math.round(durationMs / 1_000);
  return `${Math.floor(seconds / 60)}m ${String(seconds % 60).padStart(2, '0')}s`;
}

/**
 * ── A run of actions is one Astryx `ChatToolCalls`, a lone action is a line ──
 *
 * Two or more adjacent activities (`groupTranscriptActivities`) render as the
 * vendor's grouped tool calls, through `ToolCallGroup`: closed until the
 * reader opens it, to one row naming the latest call and the count, and open
 * to every call in transcript order. Whether it is open, and which failure
 * details in it are, is this component's state and not the vendor's — see
 * `activity-groups.tsx` for why. A single activity keeps `ActivityLine` below,
 * whether it is a run that was always one call or a run a page shift has
 * left one call of: Astryx renders one call inline without group chrome, and
 * the line already says the same thing in this transcript's own register,
 * with its failure detail in the open.
 *
 * What one activity becomes, stated field by field because each is a choice:
 *
 *  - `key` is the activity's id, which `buildTranscript` holds stable from
 *    `item/started` to `item/completed`, so a call that finishes updates its
 *    row in place rather than remounting it (and keeps a detail the reader had
 *    opened on it open). It is also what `ToolCallGroup` records an opened
 *    detail under.
 *  - `status` follows `ActivityState` one to one. Astryx has no fourth value
 *    for "started and never reported an end", which is what a `running` row
 *    is once the conversation is no longer live — an interrupted or exited
 *    turn — or once the conversation has moved past it: the same row, after
 *    the next message and the next turn's own calls. `ActivityLine` prints
 *    that row without its pulse (`live && last`); the stylesheet does the
 *    same to the group by hiding the vendor's spinner unless the group
 *    carries `data-nc-live`, which only the group at the tail of a live
 *    transcript does, so neither a dead conversation nor a run the
 *    conversation has left behind ever spins — open or closed.
 *  - `duration` is gated by the same floor as the line. Astryx prints it only
 *    on a completed call, so a failed call's duration is not shown in a group
 *    where the line would have shown it; that is the vendor's choice and this
 *    file does not work around it.
 *  - `errorMessage` and `resultDetail` are both the failure detail (non-null
 *    only on a failed activity — the domain's rule). The first is Astryx's
 *    hover title on the status icon; the second is what makes the row a
 *    button that reveals the detail inline, in the vendor's inline `Code`
 *    element, which wraps a long token instead of widening the column. The
 *    detail is wrapped in an element stamped `data-nc-detail` with the
 *    activity's id: the vendor mounts it only while the detail is open, so
 *    its presence is how `ToolCallGroup` reads the vendor's state.
 */
function toolCallOf(activity: ConversationActivity): ChatToolCallItem {
  const duration = activity.durationMs !== null && activity.durationMs >= ACTIVITY_DURATION_FLOOR_MS
    ? formatActivityDuration(activity.durationMs)
    : undefined;
  return {
    key: activity.id,
    name: activity.verb,
    target: activity.target ?? undefined,
    status: activity.state === 'done' ? 'complete' : activity.state === 'failed' ? 'error' : 'running',
    duration,
    errorMessage: activity.detail ?? undefined,
    resultDetail: activity.detail === null
      ? undefined
      : <div data-nc-detail={activity.id}><Code>{activity.detail}</Code></div>,
  };
}

/**
 * One action, one line.
 *
 * The dot is the same 6px accent pulse a running track row wears, and it is here
 * for the same reason it is there: it is the one place in the app that says
 * "this is happening right now". A running action is the honest place for it in
 * a transcript — before this existed, a four-minute turn spent entirely in
 * shell runs and a `report.write` looked from the drawer like nothing at all.
 *
 * `detail` is non-null only on a failed activity — that is the domain's rule
 * and it is asserted there (`conversation.test.ts`), so this reads the field
 * rather than re-deriving the condition from `state`.
 *
 * The failure reason is a second row *inside the same `<p>`* — `data-nc-state`
 * is the shared attribute the rest of the app reads state off, and it belongs
 * on the element that is the line. So the `<p>` is the two-row box, and the
 * first row gets a wrapper of its own: `.activityRow` holds the verb, the noun,
 * `Failed`, the duration and the live dot as one `nowrap` flex line, and the
 * reason is the `<p>`'s second child.
 *
 * The wrapper does not move the state anywhere. It holds the *first row's*
 * contents; `data-nc-state` stays on the `<p>` above it, and the reason stays
 * inside that same `<p>`, which is the containment `public.test.tsx` asserts.
 * (An earlier note here said a nested wrapper would move the attribute. That is
 * true of wrapping the whole line — it is not true of this shape, and the
 * objection cost two rounds of trying to get a `flex-wrap` to do the job.)
 *
 * Which is what the wrap could not do. A flex line fills and wraps *before* it
 * shrinks, and `.activityTarget`'s `overflow: hidden` zeroes its automatic
 * minimum size, so a wrapping box gives a 64-character command a row of its own
 * and pushes `Failed` and the duration onto a third — four rows on a failed
 * line, and no ellipsis anywhere. Confining that to `detail !== null` moved the
 * damage from every long `done` line onto every failed line; it did not fix it.
 * Two rows is a fact about the structure now, not an outcome of a layout pass.
 */
function ActivityLine({ activity, entry, live }: {
  activity: ConversationActivity;
  /** The run's carried key, stamped as `data-nc-entry` — a run of one is still a run (`useToolCallFocus`). */
  entry: string;
  live: boolean;
}) {
  const running = activity.state === 'running';
  const duration = !running && activity.durationMs !== null
    && activity.durationMs >= ACTIVITY_DURATION_FLOOR_MS
    ? formatActivityDuration(activity.durationMs)
    : null;
  return (
    <p
      className={`${styles.activity} ${activity.state === 'failed' ? styles.activityFailed : ''}`}
      data-nc-state={activity.state}
      data-nc-entry={entry}
    >
      <span className={styles.activityRow}>
        <span>{activity.verb}</span>
        {activity.target !== null
          && <span className={styles.activityTarget}>{activity.target}</span>}
        {activity.state === 'failed' && <span className={styles.activityFailure}>Failed</span>}
        {duration !== null && <span className={styles.activityDuration}>{duration}</span>}
        {running && live && <span className={styles.live} aria-label="Working" />}
      </span>
      {activity.detail !== null && (
        <span className={styles.activityDetail}>{activity.detail}</span>
      )}
    </p>
  );
}

/**
 * ── `/` in the composer ───────────────────────────────────────────────────
 *
 * **Why there is a slash command at all, when the same action has a `+`.**
 * `<PanelAction label="New conversation">` lives in the CONVERSATIONS module
 * head, on the panel column — and `app/shell/shell.module.css` hides that whole
 * column while a drawer is open (`.main:has([data-nc-drawer]) [data-nc-panel]
 * { visibility: hidden }`). So the reader who is *inside* a conversation, which
 * is precisely the reader who has just decided this thread is finished, cannot
 * reach the `+` without first closing what they are reading. `/new` in the
 * composer is the only new-conversation door that exists in that state. It is
 * not a duplicate of the `+`; it is the same door cut through the wall the
 * drawer puts up.
 *
 * **One command, and no registry behind it.** There is no command table, no
 * discovery mechanism and no second entry — those are the shape you build when
 * the set is open, and this set is closed at one. Adding a second command is
 * the moment to reconsider, not now.
 *
 * **It runs the `+`'s own callback**, passed in as `onNewConversation`, rather
 * than a copy of what the `+` does. Two entry points that reimplement one
 * action drift, and the router's `start()` already carries every rule about
 * where a new conversation may attach and what a held draft does to it.
 *
 * **Availability tracks the `+` exactly**: the router passes the callback only
 * where the `+` is offered *and does something*, so `undefined` here means no
 * trigger is configured at all and the field stays a plain `textbox`.
 */
export const NEW_CONVERSATION_COMMAND = Object.freeze({
  id: 'new-conversation',
  /*
   * The same words as `PanelAction label="New conversation"` — but this string
   * is now what Astryx *filters* on, not what the row shows: the row is
   * `renderItem`'s glyph, name and description below, and none of the three is
   * derived from this. That separation is what lets the row print `new` while
   * the reader types `/new`, and it is one-directional — `renderItem` reads
   * nothing from the item, so no rewording of the row can reach the matcher.
   * Keeping the `+`'s wording here is what makes the command reachable by the
   * name the reader has already seen on the `+`'s tooltip: `/conversation`
   * finds it just as `/new` does.
   */
  label: 'New conversation',
});

/**
 * `onSend` answers either way, and the caller's choice decides which.
 *
 * Checked by shape rather than by `!== undefined`: the `void` half of the
 * signature is a return the caller does not make, and a caller that returns
 * some other value would otherwise be awaited as if it were an outcome.
 */
function isThenable(value: unknown): value is Promise<SendOutcome> {
  return typeof (value as { then?: unknown } | null | undefined)?.then === 'function';
}

/**
 * The composer is Astryx's ChatComposer: rounded well, auto-grow, send/stop
 * geometry, Enter-to-send with IME guard. We own the value and the send
 * callback so the kernel path stays a string.
 */
export function ChatComposer({
  onSend, onStop, onNewConversation, disabled = false, focusOnMount = false, draft: controlledDraft,
  footerActions, sendAdornment,
  drawer, headerActions, allowEmptyText = false,
}: {
  /** See `SendOutcome`. A caller with its own draft persistence returns `void`. */
  onSend: (text: string) => void | Promise<SendOutcome>;
  /** A route may retain unsent words across its recovery surfaces. */
  draft?: Readonly<{ text: string; onChange: Dispatch<SetStateAction<string>> }>;
  /**
   * Interrupt the turn in flight. Its presence is what turns Send into Stop.
   *
   * There is deliberately **no `stopping` prop** guarding it. One stood here —
   * `onStop={stopping === true ? undefined : onStop}` — meaning "a stop already
   * asked for cannot be asked for again", and it did not mean that: Astryx's
   * `ChatSendButton` computes `isDisabled={!isStopShown && isDisabled}`, so
   * with Stop shown the button is unconditionally enabled, and withholding the
   * callback only emptied its `onClick`. Measured with `stopping` true:
   * `{ disabled: false, ariaDisabled: null }`, and pressing it called nothing.
   * That is precisely the shape the note below this call rejects — a control
   * that says it can be pressed and then does nothing — bought for no change in
   * behaviour: the rule it was reaching for is already stated one layer up, at
   * the top of the router's `interrupt()`: `if (!working || stopping) return;`.
   * Removing the prop moved nothing; it deleted a duplicate.
   *
   * **The dead-button shape is not fixed by that, and this note must not be
   * read as claiming it is.** During `stopping` the router's guard returns
   * immediately, so Stop still says it can be pressed and still does nothing —
   * the identical shape, one call frame further in. What the removal bought is
   * that the fact now lives in the one place that knows it, not that the reader
   * stopped seeing a live-looking control.
   *
   * It stays broken because the fix is not on offer from out here:
   * `ChatSendButton` accepts neither `isDisabled` nor the `tooltip` Astryx
   * requires before it will render `aria-disabled` (see the `sendButton` note
   * below), and `useChatComposerContext` is not exported, so a hand-rolled
   * substitute cannot read the composer's own state either. **Known gap, owned
   * by the vendor's API surface** — recorded here so the next reader measures
   * Astryx again rather than re-deriving a workaround that cancels itself.
   */
  onStop?: () => void;
  /** Start a new conversation — the *same* callback the module head's `+`
   *  fires. Absent where the `+` is absent, and its absence is what keeps the
   *  `/` menu from existing at all. */
  onNewConversation?: () => void;
  disabled?: boolean;
  /**
   * Put the caret in the field as this composer mounts (#1211 S2).
   *
   * Read **once**, at mount, and never again — it seeds the same standing
   * `wantsFieldFocus` request a send arms, so it inherits that machinery
   * whole: the retry while the field refuses focus, the perch on the
   * composer's own box rather than `<body>`, and giving up the moment the
   * reader puts the focus somewhere themselves. A prop watched over time would
   * be a second, subtly different focus policy.
   *
   * Mount is the right one-shot for this: the caller (`app/router`) renders
   * this composer only while a conversation is open, so it mounts exactly when
   * the drawer opens on a row.
   *
   * **The precondition that comes with it, spelled out because it is a real
   * edge of this interface.** The flag has effect only for the mount it arrives
   * on, and this component has no `key` on the router's path — it is reused
   * across conversations. So a caller that raises the flag a second time while
   * the same composer is still mounted gets nothing: the caret stays where it
   * is. **One mount per intent is the caller's job.** The one production caller
   * satisfies it by construction — the intent is stated by a create, so the
   * track (and therefore the drawer and this composer) is always new — which is
   * why this is documented rather than defended in code; a component that
   * watched the prop would be the second focus policy the note above rejects.
   * Pinned by "ignores the flag being raised again on a composer that is
   * already mounted" in `thread.browser.test.tsx`.
   *
   * **Where it is proved.** In `thread.browser.test.tsx`, for the reason the
   * restore below gives: whether Astryx's editable answers
   * `[contenteditable="true"]` in the commit this mounts in is a fact about a
   * real engine, and the failure it decides between — caret in the field, or
   * caret parked on the perch with a request that nothing on this path will
   * rerun — looks identical in jsdom, which resolves the selector at once.
   */
  focusOnMount?: boolean;
  /**
   * Per-conversation controls that belong beside the field rather than in the
   * transcript — the model picker (#1505 S4-3).
   *
   * A pass-through to Astryx's `footerActions` slot and nothing more. It is a
   * slot rather than a `modelSelection` prop because the composer has no
   * business knowing what a model is: the controls need a query, a mutation
   * and a card id, all of which live in `app/router`, and `features/**` may
   * not import `app/**`. Handing the composer the rendered node keeps that
   * edge where it is.
   */
  footerActions?: ReactNode;
  /**
   * #1255 S3 — something to stand immediately before Send, in the composer's
   * `sendActions` slot.
   *
   * A slot and not a `contextUsage` prop, for the reason `footerActions` gives
   * one line up: what goes there needs a query and a card id, both of which
   * live in `app/router`, and `features/**` may not import `app/**`.
   *
   * It shares the slot with the one remaining send-door button below, which
   * appears only in a state this one knows nothing about, so it is rendered
   * *before* it rather than instead of it. Read the slot's contents as
   * "everything that is not the send button", in reading order.
   */
  sendAdornment?: ReactNode;
  /**
   * #1505 S6 — the composer's two vendor slots, passed as nodes rather than as
   * attachment state.
   *
   * `features-no-cross-domain` forbids this module from importing
   * `features/planner`, and it should: what goes above the input is not this
   * component's business. It renders whatever it is handed into the slots
   * Astryx documents for it and knows nothing about images.
   */
  drawer?: ReactNode;
  headerActions?: ReactNode;
  /**
   * Whether a send with no words is a real send.
   *
   * True exactly when the message carries something other than text — today,
   * an attached image. It is a prop and not an inference because this
   * component cannot see what is in the drawer: the drawer is an opaque node
   * (above), and the caller that filled it is the one that knows whether it is
   * empty.
   */
  allowEmptyText?: boolean;
}) {
  const [localDraft, setLocalDraft] = useState('');
  const draft = controlledDraft?.text ?? localDraft;
  const setDraft = controlledDraft?.onChange ?? setLocalDraft;
  const stopShown = onStop != null;

  /*
   * The callback is read through a ref so `triggers` can be a stable array.
   * `useTriggerMenu` holds the *object identity* of the active trigger in
   * state and compares it on every input event (`state.activeTrigger !==
   * trigger`); a fresh array each render would re-open and re-search the menu
   * on every keystroke.
   */
  const newConversationRef = useRef(onNewConversation);
  newConversationRef.current = onNewConversation;

  const rootRef = useRef<HTMLDivElement>(null);
  const [sendCount, setSendCount] = useState(0);
  const wantsFieldFocus = useRef(focusOnMount);
  /** The element this component last put focus on — the perch or the field.
   *  `null` while no restore is in flight, and the whole of how the effect
   *  below tells "focus is still where we left it" from "the reader moved it". */
  const parkedFocus = useRef<Element | null>(null);

  /*
   * ── Put the caret back after a send, and keep trying until it lands ───────
   *
   * Load-bearing, not a nicety — see the `sendButton` note below: Send is a
   * natively disabled control the moment the draft empties, and a natively
   * disabled control that currently holds focus hands that focus to `<body>`.
   * Sending from the button is the one path that puts focus there first.
   *
   * **Why this is an effect and not a line at the end of `onSubmit`.** It was
   * that line, and on the app's own wiring it did nothing. Both router call
   * sites pass a `disabled` that goes true inside the very click that sends —
   * `disabled={store.sending}` on the conversation path, `disabled={creating}`
   * on the draft path (`app/router/public.tsx`), two different flags with the
   * same timing — and the send handler behind each sets it synchronously, so
   * the real order inside one click is:
   * `onSend` queues the flag → the old code put focus in the field → React
   * flushes → `isDisabled` is true → Astryx turns the field into
   * `contenteditable="false"` → **Chromium hands the focus it just received to
   * `<body>`**. Measured: with no `disabled` prop the field keeps focus, with
   * `disabled` flipping true on send `document.activeElement` is `BODY`. The
   * failure this code exists to prevent survived the code intact, and the unit
   * test could not see it — it rendered a composer with no `disabled` at all,
   * which is a configuration the app never builds, and jsdom does not drop
   * focus off a `contenteditable` going false anyway. The binding assertion is
   * in `thread.browser.test.tsx`, against a wrapper wired the way the router
   * wires it.
   *
   * So the restore runs *after* the commit that carries `disabled`, and it is a
   * standing request rather than one attempt: while the field refuses focus,
   * focus is parked on the composer's own box — which is stable, is where the
   * reader is looking, and is never `<body>` — and `wantsFieldFocus` stays
   * armed so the rerun this effect gets when `disabled` clears lands the caret
   * where it belongs. `sendCount` is in the deps because a composer with no
   * `disabled` (the draft path) sees no flag change to rerun on.
   *
   * **Giving up is decided by identity, not by containment.** The test used to
   * be "focus has left `.composer` entirely", and it let two real cases through.
   * A send shows Stop *inside* the composer, so a reader who tabs to Stop and
   * waits is still "inside" — and when `disabled` cleared the caret was yanked
   * off the control they had deliberately aimed at. And `<body>` was read as
   * "nobody moved", which is also what the document reports after a click on any
   * non-focusable part of the page. So the effect now remembers the element it
   * parked on and continues only while focus is *exactly* there: anything else,
   * inside the composer or out, is the reader having spent their own intent, and
   * `<body>` after a successful park is a click somewhere blank rather than the
   * disabling-control drop this exists for. That drop only happens in the same
   * commit as the send, before anything has been parked, which is precisely why
   * the check is skipped on the first run (`parkedFocus` still `null`).
   *
   * The field is found by query rather than by ref because it is Astryx's
   * element, handed to `ChatComposer` as an `input` slot; there is no ref for
   * it to give us — and the `[contenteditable="true"]` selector is exactly why
   * this works: while disabled, the attribute reads `false` and there is
   * nothing to match, which is the same fact the browser is acting on.
   */
  useEffect(() => {
    if (!wantsFieldFocus.current) return;
    const root = rootRef.current;
    if (root === null) return;
    const parked = parkedFocus.current;
    if (parked !== null && document.activeElement !== parked) {
      wantsFieldFocus.current = false;
      parkedFocus.current = null;
      return;
    }
    const messageField = root.querySelector<HTMLElement>('[contenteditable="true"], textarea');
    messageField?.focus();
    if (messageField !== null && document.activeElement === messageField) {
      wantsFieldFocus.current = false;
      parkedFocus.current = null;
      return;
    }
    if (!root.contains(document.activeElement)) root.focus({ preventScroll: true });
    parkedFocus.current = document.activeElement;
  }, [sendCount, disabled]);

  const triggers = useMemo<ChatComposerTrigger[]>(() => [{
    character: '/',
    searchSource: createStaticSource([NEW_CONVERSATION_COMMAND]),
    menuLabel: 'Commands',
    emptySearchResultsText: 'No command by that name',
    /*
     * One row, two columns: what you type on the left, what it does on the
     * right. `item.label` is deliberately *not* rendered — see the token below.
     */
    renderItem: () => (
      <span className={styles.commandItem}>
        {/*
          * **The glyph is the `+`'s glyph, and that is the whole point.** The
          * module head's `<PanelAction label="New conversation">` is not a
          * similar action, it is *this* action — the same `onNewConversation`
          * runs from both. Drawing the same `plus` here is the cheapest way to
          * say so to a reader who has already pressed the `+` and is now
          * looking at a door they have not seen before. A second glyph invented
          * for the menu would be asserting the opposite.
          *
          * No label on it: `Icon` is `aria-hidden` by construction, the row's
          * accessible name comes from the item's `label`, and the words next to
          * it already say what it does. An `aria-label` here would make the
          * screen reader read the action twice.
          *
          * **The name is `new`, without the slash the reader already has.**
          * This row is only on screen because a `/` is sitting in the field two
          * inches below it — printing a second one restates the character that
          * *caused* the menu. And the row now carries two independent signals
          * that this is a command you press: the `+` on the left, which is the
          * module head's own button, and the description on the right in the
          * caption rank. A slash would be a third, spending width on the one
          * thing about this row that was never in doubt. What is left, `new`,
          * is the part the reader does not have and has to type.
          *
          * That is a reversal of the previous round, which printed `/new`
          * because the token is what you type and because every menu of this
          * shape (codex, Claude Code, Slack) prints the slash. Recorded plainly
          * so nobody re-derives it: that argument assumed a row with no glyph,
          * where the slash was the *only* thing marking the label as a command
          * rather than a noun. The `+` took that job, and the argument went
          * with it.
          *
          * **The slash's disappearance from the row is display-only.** What
          * Astryx matches against is `NEW_CONVERSATION_COMMAND.label`, and
          * `renderItem` never touches it — see the token above.
          *
          * Glyph and literal are siblings in the row rather than nested in a
          * box of their own — they read as one thing because they share the
          * name's colour and sit tight against each other, which is cheaper
          * than a wrapper and avoids a real layout trap (both recorded in the
          * stylesheet).
          */}
        <Icon name="plus" size="sm" />
        <span className={styles.commandName}>new</span>
        {/*
          * **The description carries only the half that is not already said.**
          * It was `Opens a fresh thread; this one stays in the list.` — but a
          * `+` and the word `new` now spell "opens a fresh thread" twice over
          * before the sentence starts, and the type scale (tokens.css §type)
          * says of `--text-xs`, the rank this sits at: *never a sentence*. What
          * is left is the thing the reader actually risks being wrong about,
          * because it is the thing a "new" button in a list of threads could
          * plausibly do either way: the thread they are reading survives.
          *
          * Screenshot comparison of three (`r8-hint-a` / `-b` / `-c`):
          *   a  This one stays in the list   ← this one
          *   b  Keeps this one in the list   — reads as a promise the *command*
          *      makes, so the eye goes back to the name to find the subject; and
          *      "keeps" invites "keeps it where?" that "stays" does not.
          *   c  This one stays               — half the width and none of the
          *      answer: stays *where* is exactly the question being asked.
          * No full stop: it is a phrase, not a sentence, and the row has no
          * second one for it to be separated from.
          */}
        <span className={styles.commandHint}>This one stays in the list</span>
      </span>
    ),
    /*
     * A command is *run*, not inserted. `onSelect` returning `''` is how this
     * API says "put nothing in the field": Astryx has already deleted the
     * typed `/new` before calling us, so the empty string leaves the composer
     * clear and the text never reaches `onSubmit`. The action itself is the
     * side effect here — there is no other hook on this path that fires once
     * per selection.
     */
    onSelect: () => {
      newConversationRef.current?.();
      return '';
    },
  }], []);

  /**
   * Hand one message to the caller, from wherever the reader asked for it.
   *
   * **`stopShown` is deliberately not a reason to refuse (#1505).** It stood in
   * this guard and it was the whole of the bug: a turn in flight puts `onStop`
   * on this component, which turns Send into Stop, which made every press and
   * every Enter return here — before `onSend`, and before the `setDraft('')`
   * below, so the sentence was neither sent, nor queued, nor reported, nor even
   * left in the field. The kernel never asked for that refusal:
   * `POST /planner/input` (`send_planner_input`) does not read the phase at
   * all, folds the text into the harness pending queue, and the run loop issues
   * it as the next turn. The composer was refusing on the kernel's behalf a
   * thing the kernel does happily.
   *
   * `disabled` stays, and means what it always meant on this path: the router
   * passes `store.sendBlocked`, which is "the last POST has not settled yet",
   * not "the agent is busy".
   */
  const submit = (value: string) => {
    const text = value.trim();
    /* #1505 S6 — `allowEmptyText` is the caller saying the message carries an
       image. Without it an empty draft is still nothing to send. */
    if ((text === '' && !allowEmptyText) || disabled) return;
    const outcome = onSend(text);
    setDraft('');
    /*
     * Cleared optimistically, put back for the one outcome that says the
     * server has nothing and named it. `unresolved` is excluded on
     * purpose — the endpoint carries no idempotency key, so offering the
     * text back there is one Enter away from a second delivery.
     * `abandoned` is excluded because that answer is about a conversation
     * this composer is no longer showing. `not-sent` is excluded because
     * a second submission's text must not take the field from an earlier
     * send that is still waiting to hear whether it was refused; the
     * residual that leaves is in #1449's list.
     *
     * Only into an empty field: the reader can type again the moment the
     * field is cleared, and what they typed is theirs.
     *
     * KNOWN GAPs (#1449). The restore reaches THIS composer, not the
     * conversation: close the drawer during the request and re-open the
     * same conversation and it runs into an unmounted one, so the error
     * line stands with the sentence gone. And the trimmed `text` is what
     * goes back, so trailing whitespace the reader typed does not.
     */
    if (isThenable(outcome)) {
      void outcome.then((result) => {
        if (result !== 'refused') return;
        setDraft((current) => current === '' ? text : current);
      });
    }
    /* The caret goes back to the field from the effect above, not from
       here: `onSend` may have already queued the `disabled` that takes
       the field away, and this handler runs before React flushes it. */
    wantsFieldFocus.current = true;
    /* A fresh request, so the effect's first run must not compare against
       a perch left over from an earlier one — see the effect's note. */
    parkedFocus.current = null;
    setSendCount((count) => count + 1);
  };

  /*
   * The send-door button for this state, or none.
   *
   * Lifted out of the `sendActions` attribute when the slot gained a second
   * occupant (the context ring): the slot now holds "whatever is not Send",
   * and a ternary nested inside a fragment inside an attribute is not a thing
   * anybody should have to read.
   */
  /*
   * ── The one send door this composer adds, and the one it no longer does ──
   *
   * **Gone: `Queue message`.** It stood beside Stop while a turn ran, because
   * `ChatSendButton` is one button in two states and that state is Stop, so a
   * person who had typed a sentence had no button to press. It is gone at the
   * owner's call, and what it cost is exactly that: a *button*. Enter still
   * queues — Astryx's `handleSubmit` refuses only an empty draft and
   * `isDisabled`, never `isStopShown`, so the keyboard path is untouched and
   * `POST /planner/input` is reached the same way it always was — held down by
   * the `[F4]`/`[F5]`/`[F6]` cases in `track-conversation.test.tsx`, which
   * drive it through Enter.
   *
   * **For anyone who cannot use a keyboard, that is the capability and not
   * only the affordance**, and it is gone while a turn runs: with a draft
   * typed there is only Stop, and with an image and no text there is not even
   * `Send image` (it is suppressed by `!stopShown` below). Saying "the
   * affordance, not the capability" would be true only of a reader who has
   * both hands on a keyboard. **KNOWN GAP, accepted by the owner** in exchange
   * for one control in that corner instead of two. The queue strip above the
   * field is where a queued message is now visible, which is the half that was
   * missing when that button was written.
   *
   * **Kept: `Send image`.** Different failure, and not a duplicate of
   * anything: `ChatSendButton` takes its availability from the composer
   * context's `canSend`, which is false on an empty draft, and an image-only
   * message IS an empty draft. Without this there is no control at all —
   * not a second one — and Enter is already handled a few lines up by the
   * same `allowEmptyText` guard. It shows only while the vendor's own button
   * is unavailable, so the two are never both live, and it is named
   * `Send image` rather than `Send` so two controls never share one name.
   */
  const sendDoor = allowEmptyText && draft.trim() === '' && !stopShown ? (
    <button
      type="button"
      className={styles.queueSend}
      data-nc-send-attachment=""
      disabled={disabled}
      onClick={() => { submit(draft); }}
    >Send image</button>
  ) : undefined;

  return (
    <div
      ref={rootRef}
      className={styles.composer}
      data-nc-composer=""
      /* Programmatic focus only, never a tab stop: this is the perch the send
         effect above parks on for the length of a send, so that the focus taken
         off a disabling Send has somewhere to be that is not `<body>`. */
      tabIndex={-1}
      /*
       * **Named, because focus stops here and a screen reader announces what it
       * stops on.** Measured before this went in: `role: null, aria-label: null`
       * — for the whole length of a request the reader was parked on an
       * anonymous `div` whose only readable text was the field's placeholder,
       * which is worse than the `<body>` this perch replaced in one respect
       * (`<body>` at least announces the document).
       *
       * `group` rather than `form` or `region`: it is a set of related controls
       * with no landmark claim to make, and a landmark inside a drawer that is
       * already `complementary` would add a second thing to the reader's
       * landmark list for no navigational gain.
       *
       * The name builds on the field's own `label="Message"` rather than
       * repeating it. Repeating it was the first attempt and it is wrong twice
       * over: two elements one nesting apart answering to the same accessible
       * name is ambiguous to a reader navigating by name, and it is ambiguous to
       * every `getByLabelText('Message')` in `public.test.tsx`, which stopped
       * resolving. `Message composer` names the box for what it is — the field
       * plus the controls around it — and keeps the word the reader is already
       * oriented by.
       */
      role="group"
      aria-label="Message composer"
      onKeyDownCapture={(event) => {
        /* Astryx clears its input after Enter even when its parent refuses
           submission. Keep unsent words while disabled, and
           let IME Enter accept its candidate without submitting. */
        if (event.key !== 'Enter' || event.shiftKey) return;
        /*
         * Only Enter pressed IN THE FIELD is a send.
         *
         * This handler is on the composer's root and captures, which was
         * harmless while the field was the only focusable thing under it. The
         * `drawer` slot changed that: the queued-message bubbles live inside
         * this root now, each with its own control. Tab to one of them with
         * an image picked and an empty draft, press Enter, and the
         * `allowEmptyText` branch below sent the image and called
         * `preventDefault()` — so the button a person was actually on never
         * fired. A key pressed on a button belongs to that button.
         */
        if (!(event.target instanceof Element)
          || event.target.closest('[contenteditable], textarea, input') === null) return;
        if (event.nativeEvent.isComposing) {
          event.stopPropagation();
          return;
        }
        if (disabled) {
          event.preventDefault();
          event.stopPropagation();
          return;
        }
        /*
         * #1505 S6 — Astryx's own `handleSubmit` refuses an empty draft, and
         * an image-only message IS an empty draft. Measured, not assumed: the
         * vendor guard is `if (!value.trim()) return;` before it calls
         * `onSubmit`, so Enter over a picked image would do nothing at all.
         * Submitting here and stopping the event is the same door the
         * `sendActions` button below uses, for the same reason.
         */
        if (allowEmptyText && draft.trim() === '') {
          event.preventDefault();
          event.stopPropagation();
          submit(draft);
        }
      }}
    >
      <AstryxChatComposer
        density="compact"
        value={draft}
        onChange={setDraft}
        placeholder="Say something"
        isDisabled={disabled}
        isStopShown={stopShown}
        footerActions={footerActions}
        /* Handed over whole — the "one interrupt at a time" rule is the
           router's, at the top of `interrupt()`. See the `onStop` prop note. */
        onStop={onStop}
        /* Enter arrives here. Astryx's own `handleSubmit` refuses only on an
           empty draft and on `isDisabled` — it has never consulted
           `isStopShown` — so with the composer's guard corrected, Enter sends
           while a turn runs exactly as the button does. */
        onSubmit={submit}
        /*
         * ── The second door, open only while the first one says Stop ────────
         *
         * `sendButton` is one button in two states, and while a turn runs that
         * state is Stop. Letting a send *through* therefore is not enough: with
         * only that control on screen the reader who has typed a sentence can
         * press nothing but Stop. So the send gets a control of its own for
         * exactly as long as Send is not itself available.
         *
         * A plain `<button>` rather than a second `ChatSendButton`: that
         * component takes its label, its variant and its `onClick` from
         * `isStopShown`, so a second one would render Stop as well, and it
         * accepts neither `isDisabled` nor the `tooltip` Astryx needs before it
         * will announce one (measured — see the `onStop` and `sendButton` notes
         * above). Nothing here reaches into vendor internals; it sits in the
         * `sendActions` slot the composer documents for exactly this.
         *
         * **Named `Queue message`, not `Send`.** Two buttons cannot both be
         * "Send", and this one is not Send: what it does is put the sentence on
         * the harness's pending queue behind the turn in flight. The name is
         * the promise the transcript then keeps, one line up, with `Queued ·
         * sends when this turn ends`.
         *
         * Its own availability is `canSend` restated by hand, because
         * `useChatComposerContext` is not exported — but restated over the same
         * `draft` this component owns and hands Astryx as `value`, so there is
         * no second source of truth for it to drift from.
         */
        {...(drawer === undefined ? {} : { drawer })}
        {...(headerActions === undefined ? {} : { headerActions })}
        sendActions={sendDoor === undefined && sendAdornment === undefined ? undefined : (
          <>{sendAdornment}{sendDoor}</>
        )}
        input={(
          <ChatComposerInput
            label="Message"
            placeholder="Say something"
            /* No triggers where there is no command to offer: without them the
               field keeps `role="textbox"` rather than becoming an
               `aria-expanded="false"` combobox that can never expand. */
            {...(onNewConversation === undefined ? {} : { triggers })}
          />
        )}
        /*
         * ── Send's availability, and why it is Astryx's and not ours ────────
         *
         * This used to be `<ChatSendButton isDisabled={stopping} />`, and both
         * halves of that were wrong.
         *
         * The override *replaced* `ChatSendButton`'s own default,
         * `isDisabled = !(context?.canSend ?? false)`. With `canSend` out of the
         * picture, Send on an empty composer measured `{ label: 'Send',
         * disabled: false, ariaDisabled: null }` — a control that says it can be
         * pressed and then does nothing, which is the one thing a button may
         * never do.
         *
         * And the value it substituted was dead anyway: the router only passed
         * `stopping` on the paths where it also passes `onStop`, so `stopShown`
         * is true whenever `stopping` could be, and `ChatSendButton` computes
         * `isDisabled={!isStopShown && isDisabled}` — identically `false`. The
         * prop expressed an intention ("a stop already asked for cannot be
         * asked for again") that the component's own arithmetic cancelled. A
         * later round tried to rescue that intention by withholding `onStop`
         * instead, which cancelled just as completely and left a live-looking
         * Stop with an empty `onClick`; the prop is gone, and the rule it wanted
         * lives at the top of the router's `interrupt()`. See the `onStop` prop
         * note above for the measurements.
         *
         * ── The trade this makes, stated plainly ────────────────────────────
         *
         * Astryx renders `aria-disabled` **only** when a `tooltip` is set
         * (`Button/Button.tsx`: `useAriaDisabled = tooltip != null &&
         * buttonDisabled`); otherwise it is a native `disabled`.
         * `ChatSendButton` accepts no `tooltip` and forwards no rest props, so
         * from out here the choice is native `disabled` or nothing — and
         * `useChatComposerContext` is not exported, so a hand-rolled send button
         * could not read `canSend` either without reimplementing the composer's
         * state.
         *
         * Native `disabled` is announced ("Send, button, unavailable") but it
         * leaves the tab order, and a control that vanishes from under a
         * keyboard user's focus drops that focus on `<body>` — which is exactly
         * what §5.1's deleted test existed to prevent. That failure has one
         * trigger here and it is `submit`: focus is on Send, the click sends,
         * the draft empties, `canSend` goes false, and the button focus is
         * sitting on goes away.
         *
         * So the focus is *moved deliberately*, back into the field, before
         * that can happen — which is where a person who just sent a message
         * wants it regardless — by the standing request in the focus effect at
         * the top of this component, which was a `returnFocusToField` helper
         * called from `onSubmit` until that was measured doing nothing (the
         * effect's own note has the measurement). That leaves "Send is
         * not tabbable while the field is empty", which is the standard
         * behaviour of a disabled control and costs a keyboard user nothing:
         * there is nothing to send, and the field they would have to visit to
         * change that is the previous stop in the same tab ring.
         */
        sendButton={<ChatSendButton />}
      />
    </div>
  );
}

/**
 * ── The footer's error strip ──────────────────────────────────────────────
 *
 * Everything around the composer used to be a bare `<p>` and a bare `<button>`
 * composed by the router: no inset, so they sat flush against the card's edge
 * while the composer kept the card's `--nc-card-inset`, and no rank, so an
 * error printed at body size in body ink. One root cause, three symptoms (the
 * send error, the draft error, the two remedies) — so the fix is components
 * rather than call-site classNames.
 *
 * They own presentation only. *When* any of them appears, what it says, and
 * what pressing it does stay the router's, unchanged — which is why the strip
 * is a container the router fills rather than a component that decides for
 * itself: a remedy can be offered with no error beside it (an unconfirmed send
 * whose landing came back `absent`), and that case must still render.
 */

/** The strip itself: an `alert` region welded to the top edge of the composer
 *  well. It is rendered above `<ChatComposer>`, not below it — the geometry
 *  ("upper corners rounded, lower square") only reads as *attached* from that
 *  side, and the stylesheet says why that attachment is the point. */
export function ChatFooterNotice({ children, tone = 'error' }: { children: ReactNode; tone?: 'error' | 'neutral' }) {
  return <div role={tone === 'neutral' ? 'status' : 'alert'}
    className={`${styles.footerNotice} ${tone === 'neutral' ? styles.footerNoticeNeutral : ''}`}>{children}</div>;
}

/** What went wrong, at the caption rank the activity lines already use for a
 *  failed action. It carries no colour of its own beyond `--error-text`; the
 *  strip around it carries the fill. */
export function ChatFooterError({ message }: { message: string }) {
  return <span className={styles.footerError}>{message}</span>;
}

/** The way out of that error, inline in the strip. `tertiary` is §4.1's
 *  quietest tier: the remedy must be findable without competing with Send,
 *  which is the control anyone looking at this footer is actually aiming for. */
export function ChatFooterRemedy({ disabled = false, onClick, children }: {
  disabled?: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button type="button" data-nc-action="tertiary" disabled={disabled} onClick={onClick}>
      {children}
    </button>
  );
}

/** A wall clock, not a relative time: the separator exists to say *when*, and
 *  "3h" is only useful when you already know when now is. */
function clockTime(atMs: number): string {
  return new Date(atMs).toLocaleTimeString('en-US', { hour: 'numeric', minute: '2-digit' });
}
