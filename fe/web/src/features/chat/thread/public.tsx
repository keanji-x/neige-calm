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

import { useEffect, useLayoutEffect, useMemo, useRef, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import {
  ChatComposer as AstryxChatComposer,
  ChatComposerInput,
  ChatSendButton,
  type ChatComposerTrigger,
} from '@astryxdesign/core/Chat';
import { Markdown } from '@astryxdesign/core/Markdown';
import { createStaticSource } from '@astryxdesign/core/Typeahead';

import { drawerSeamAround } from '../../../ui/drawer/public.tsx';
import { Icon } from '../../../ui/icon/public.tsx';
import { useState } from '../../../ui/state/public.ts';

import {
  isLiveConversation, opensAfterGap, opensExchange,
  type Conversation, type ConversationActivity, type TranscriptEntry,
} from '../../../../../core/domain/conversation.ts';
import styles from './thread.module.css';

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
  const lastTurnCarriesLiveMark = lastTurn?.author === 'agent'
    || (lastTurn?.author === 'activity' && lastTurn.state === 'running');
  const endRef = useRef<HTMLDivElement | null>(null);
  /** The box every marker lookup starts from. It is not `.thread` itself
   *  because the stylesheet's `> * + *` rules space that element's children,
   *  and this wrapper is what keeps a marker search rooted above them without
   *  joining that list. It no longer holds the rail — see `railSeam`. */
  const frameRef = useRef<HTMLDivElement | null>(null);
  const exchanges = useMemo(() => exchangesOf(turns), [turns]);
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
  const railShown = exchanges.length >= EXCHANGE_RAIL_MIN && railSeam !== null;
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

  if (turns.length === 0) {
    return (
      <div className={styles.empty} data-nc-thread-empty="">
        <p className={styles.emptyLead}>Nothing said yet.</p>
        <p className={styles.emptyHint}>Write below and it starts here.</p>
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
        <ExchangeRail
          exchanges={exchanges}
          active={active}
          onJump={(id) => {
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
      <div className={styles.thread} data-nc-thread="">
        {turns.map((turn, index) => {
          const last = index === turns.length - 1;
          if (turn.author === 'activity') {
            return <ActivityLine key={turn.id} activity={turn} live={live && last} />;
          }
          if (turn.author === 'system') {
            return (
              <div key={turn.id}>
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
          const opens = opensExchange(turns, index);
          return (
            <div
              key={turn.id}
              className={opens ? styles.exchange : undefined}
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
                <p className={styles.said} data-nc-turn="you">{turn.text}</p>
              ) : (
                <div className={styles.reply} data-nc-turn="agent">
                  <Reply text={turn.text} />
                  {live && last && <span className={styles.live} aria-label="Working" />}
                </div>
              )}
            </div>
          );
        })}
        {/* A reply that has not arrived yet still gets a place to arrive in. The
            last entry owns the live mark only when it is an agent reply or a
            running action; otherwise this placeholder keeps the one mark visible. */}
        {live && !lastTurnCarriesLiveMark && (
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
const NOTHING_TO_REPAINT = () => {};

/**
 * ── The rail of dots, and the three things it is not ──────────────────────
 *
 * **It is not a second definition of an exchange.** `opensExchange` in
 * `core/domain/conversation.ts` already says where one starts — "authored by
 * you, and the turn before it was not" — and `.exchange` in the stylesheet
 * already groups by it. One dot per element that carries `data-nc-exchange`,
 * which is the *same* element the class goes on. If the domain's rule changes,
 * the dots change with it and there is nothing here to update.
 *
 * **It is not a table of contents, and it no longer costs the column
 * anything.** The whole of the thread stylesheet's argument is about holding a
 * reading measure inside a drawer that can be as narrow as `--panel-span`'s
 * 240px floor — the reply gives up a type step for it and the bubble gave up
 * its existence — so a rail that spent even 60px of the column on labels would
 * be undoing, for navigation, exactly what the transcript gave up for reading.
 *
 * Two earlier rounds argued about *how much* of the column to spend on it: zero
 * (with a hit box that overhung the paragraph and pressed a dot when you
 * dragged to select a line), then 14px, then 10px "≈1.7 characters of measure".
 * All three are gone, and so is the trade they were haggling over. **The rail
 * is in the drawer's seam** — the 24px strip of page between the card's
 * trailing edge and the window that `.drawer`'s own `inset-inline-end` leaves
 * empty at every viewport width. It takes nothing from the measure because it
 * is not in the measure. Any future argument about the rail's width is an
 * argument about the seam, not about the transcript.
 *
 * **The prompt has a third channel: a layer that floats over the card.** It is
 * the accessible name for everyone, and — after `RAIL_PREVIEW_DELAY_MS` of a
 * pointer resting on a dot — a panel beside the rail for the reader who has a
 * pointer. That is not a reversal of the paragraph above: it paints on a layer
 * above the card rather than reserving space in it, so the measure is
 * untouched, and it is gone the moment the pointer is. It now floats
 * inline-*start*, back across the card, because the rail is on the far side of
 * the transcript from where it used to be. `title` used to do this job and has
 * been removed, because the UA's own tooltip and this one are two hovers on one
 * control, on two different delays, neither of which can be told to wait for
 * the other.
 *
 * **It is not always there.** Under `EXCHANGE_RAIL_MIN` the dots are pure
 * chrome — with four exchanges the whole conversation is a scroll or two away
 * and the rail's answer to "where am I" is one you already have. This is the
 * same restraint `opensAfterGap` applies to the timestamp: print the aid only
 * where the thing it is an aid *to* actually happened.
 *
 * ── One tab stop, not one per exchange ────────────────────────────────────
 *
 * Without this a thirty-exchange conversation puts thirty buttons in the
 * drawer's tab ring — i.e. the reader who wants to *type* has to pass the whole
 * navigation aid to get there. The seam has moved *where* those buttons sit in
 * the ring (they are now after the composer rather than before the transcript,
 * because the seam is the card's next sibling — see `.seam` in
 * `ui/drawer/drawer.module.css`) and has not changed the count, which is what
 * this is about. So the dots use the standard roving `tabIndex`: exactly one of
 * them is in the tab ring at a time, and Up/Down (plus Home/End) move between
 * them once you are inside. Which one holds the stop is the one you are
 * reading, so Tab-then-Enter goes where you already are rather than to the top
 * of the conversation.
 *
 * **That last sentence is a claim about a state that has to be given back.**
 * `roved` is what the arrows moved the stop to, and `onFocus` writes it — which
 * a mouse press fires too. Left alone it never cleared, so the sentence above
 * was true exactly until the reader's first click and false for the rest of the
 * session: measured before this, click the second dot then scroll to the end
 * and the ninth dot is lit while the tab stop is still on the second. So the
 * hand-placed stop is dropped whenever the lit exchange changes, i.e. whenever
 * the reader has gone somewhere the stop was supposed to be — and the DOM focus
 * moves with it when it is inside the rail, or the reader would be left holding
 * a button that is no longer a tab stop (see the effect). Arrowing to a dot
 * and *not* pressing it leaves the mark alone and therefore leaves the stop
 * where it was put, which is the whole point of a roving group.
 *
 * It holds the exchange's **id**, not its index. Prepending loaded history
 * moves every index by the number of turns that arrived, and a stop stored as
 * `2` would then name a different exchange; an id either still exists or is
 * gone, and gone falls back to the lit one with no clamp to get wrong.
 */
function ExchangeRail({ exchanges, active, onJump }: {
  exchanges: readonly Exchange[];
  active: string | null;
  onJump: (id: string) => void;
}) {
  const railRef = useRef<HTMLDivElement | null>(null);
  const trackRef = useRef<HTMLDivElement | null>(null);
  const dotRefs = useRef<(HTMLButtonElement | null)[]>([]);
  const previewRef = useRef<HTMLDivElement | null>(null);
  /** The exchange the roving stop has been put on by hand, or `null` while it
   *  still follows the lit dot. See the note above for both halves of that. */
  const [roved, setRoved] = useState<string | null>(null);
  /** The exchange whose prompt is floating beside the rail, or `null`. Pointer
   *  state and nothing else: no keyboard path writes it, because the name the
   *  keyboard reader already hears is the same sentence. */
  const [previewed, setPreviewed] = useState<string | null>(null);
  /** The pending `RAIL_PREVIEW_DELAY_MS`, or `null` when none is armed. */
  const previewDelay = useRef<number | null>(null);
  /** The envelope effect's own scheduler, handed out so a change of the
   *  exchange set can ask for a frame without re-installing the listeners. */
  const repaintEnvelope = useRef(NOTHING_TO_REPAINT);
  /** The identity of the list, not its contents: what has to change for the
   *  dots to have moved. A fresh array of the same exchanges is not that. */
  const exchangeKey = exchanges.map((exchange) => exchange.id).join('\0');
  const activeIndex = exchanges.findIndex((exchange) => exchange.id === active);
  const rovedIndex = roved === null
    ? -1
    : exchanges.findIndex((exchange) => exchange.id === roved);
  /** Where the stop sits with nothing hand-placed: the lit dot, or the first
   *  where nothing is lit. It is what the stop *returns to*, which is why the
   *  effect below reads it rather than `tabStop` — inside that effect the
   *  hand-placed stop it is clearing is still the one in the render. */
  const litStop = Math.max(0, activeIndex);
  const tabStop = rovedIndex < 0 ? litStop : rovedIndex;
  /** Read from the effect below through a ref so that effect does not re-run on
   *  a change of *index*: history prepended in front of a hand-placed stop
   *  moves every index while moving nothing the reader did. */
  const litStopRef = useRef(litStop);
  litStopRef.current = litStop;

  /*
   * The stop is given back to the lit dot as soon as the lit dot moves — the
   * one thing that says the reader is no longer where they left it.
   *
   * **And the focus goes with it, when the focus is in here.** Moving a roving
   * `tabIndex` out from under a focused element breaks the one invariant the
   * pattern rests on — that the element holding focus is the element in the tab
   * ring — and leaves the reader on a `tabIndex="-1"` button, so their next Tab
   * leaves from somewhere the document no longer counts as a stop. Measured
   * before this: focus dot 2, scroll the pane to the end, and the tab stop is
   * dot 9 while `document.activeElement` is still dot 2. So on the one path
   * where that can happen — the rail holds focus and the lit dot moves under it
   * — the focus is moved to the dot the stop is going to, and only then. A rail
   * that does not hold focus never takes it, which is why this is guarded on
   * `track.contains` rather than done unconditionally.
   *
   * `preventScroll` and the explicit `keepInRailView` for the same reason as
   * `rove`: the default would scroll the drawer's pane out from under the
   * reader who is scrolling it.
   */
  useEffect(() => {
    setRoved(null);
    const track = trackRef.current;
    const focused = document.activeElement;
    if (track === null || focused === null || !track.contains(focused)) return;
    const stop = dotRefs.current[litStopRef.current];
    if (stop == null || stop === focused) return;
    stop.focus({ preventScroll: true });
    keepInRailView(track, stop);
  }, [active]);

  /*
   * The rail has its own scrollport (see `.railTrack`), so the lit dot can be
   * outside it — on a forty-exchange conversation most of them are. Keeping it
   * in view is done by writing the track's own `scrollTop`, by the same rule
   * and for the same reason as `jumpToExchange`: `scrollIntoView` would walk
   * out to the drawer's pane and pan the transcript the reader is reading.
   *
   * **And again whenever the track's own height changes**, which is the case a
   * one-shot effect on `activeIndex` misses entirely: the track fills the
   * drawer's seam, so a window that shrinks shortens it, and a lit dot that was
   * just inside the old lower edge is then outside the new one with nothing to
   * re-run the check — the lit exchange did not change, so `activeIndex` did
   * not change. Observing the track rather than anything upstream of it keeps
   * this component's dependency where its own arithmetic is; it is the same
   * observation whether the height comes from a measured custom property, as it
   * once did, or straight from the seam's own box, as it does now.
   */
  useEffect(() => {
    const track = trackRef.current;
    const show = () => {
      keepInRailView(track, activeIndex < 0 ? null : dotRefs.current[activeIndex]);
    };
    show();
    if (track === null) return;
    return observeResize(track, show);
  }, [activeIndex]);

  /*
   * ── The envelope: every dot's distance from the pointer, once a frame ─────
   *
   * **What is published, and why it is a number rather than a class.** Each dot
   * gets one custom property, `--nc-dot-lift`, between 0 and 1; the stylesheet
   * maps it onto a size *and onto the height of the dot's own row* (see the
   * `@media (pointer: fine)` block). A class cannot express this, because the
   * answer is not "which dot" but "how much, each" — a `.magnified` on the
   * nearest dot is precisely the degenerate case this envelope exists instead
   * of, and the whole point is that a band of neighbours moves together and the
   * eye reads a curve rather than a jump — seven dots with the pointer on a
   * centre, eight between two, because the falloff is exactly zero at
   * `RAIL_SPREAD_SPAN` dots. A property also keeps the DOM still: nothing is
   * added, removed or reclassed as the pointer moves, so React is not involved
   * at all and the write is a style mutation on an element React already owns.
   *
   * ── Why the distance is counted in dots and not in pixels ─────────────────
   *
   * **This is the change that makes the spread possible at all, and getting it
   * wrong is a feedback loop.** The falloff used to be a function of each dot's
   * measured distance from the pointer in pixels, normalised by that dot's own
   * measured height. That was safe while the lift only changed the ink inside a
   * fixed 24px row: the rects the pass reads could not move in response to what
   * the pass writes.
   *
   * The lift now changes the row's height, so they can. Written the old way the
   * loop closes on itself: the pointer's neighbour lifts, its row grows, its
   * centre is pushed further from the pointer, so next frame it measures as
   * further away and lifts *less*, so it shrinks, so it measures nearer again.
   * That is an oscillator, not an envelope, and it runs at frame rate.
   *
   * So the falloff is a function of **index** distance — `|i − u|`, where `u`
   * is where the pointer is on the *list*: the index of the dot it is inside,
   * plus how far through that dot it is. Index distance is invariant under
   * everything the spread does to the geometry, so writing a lift cannot change
   * the lift. `u`'s fractional part is still read off a live box, and that one
   * term does feed back — but it feeds back *contractively*: a row that grows
   * under a stationary pointer puts the pointer nearer its own centre, which
   * takes the fraction toward 0, which is the direction that stops. Its whole
   * range is a lift of 0.926 to 1 on the dot under the pointer, i.e. about a
   * pixel of ink, and it settles inside two frames.
   *
   * **Symmetry is a requirement and not an aesthetic.** `|i − u|` is symmetric
   * about `u` by construction, so the column grows by the same amount above the
   * pointer as below it. In a column short enough to fit the track — which
   * `--nc-rail-max` makes the ordinary case — the flex centring then absorbs
   * exactly half of that growth at each end, and the dot under the pointer does
   * not move at all: the arithmetic is `−G/2 + (growth above it)`, and those
   * are the same number. An asymmetric falloff would slide the whole column
   * under a stationary pointer, which is the "the rail jumps when I approach
   * it" failure. The browser tier pins the fitting case at zero drift.
   *
   * Stated rather than papered over: in a column long enough to *overflow* the
   * track, `safe center` has degraded to `start`, so there is no centring left
   * to absorb the growth and the column does slide down by the growth above the
   * pointer. That amount is constant (half the total) everywhere except within
   * `RAIL_SPREAD_SPAN` dots of the very top, where it ramps in — so the slide
   * is not visible as motion in the body of a long rail, and is worth about two
   * pitches at its head. Compensating it by writing the track's own `scrollTop`
   * was tried and is not here: it makes the reader's own wheel un-honourable,
   * because a scroll and a re-shaped envelope are indistinguishable to it, and
   * a scrollTop the reader sets gets taken back on the next frame.
   *
   * **Throttled to a frame, by the same shape as the scroll handler in
   * `ChatThread`.** `pointermove` fires far more often than the compositor
   * paints — a high-polling mouse emits several per frame — and every one of
   * them would otherwise cost a full pass of `getBoundingClientRect` over every
   * dot. So a move only records where the pointer is and asks for a frame; the
   * frame does the reading and the writing. The reads are all taken before any
   * of the writes for the ordinary reason: interleaving them would force a
   * layout per dot instead of one for the pass.
   *
   * **Leaving the rail has to land back at exactly nothing.** `pointerleave`
   * (and `pointercancel`, which is what a drag or a lost pointer capture sends
   * instead) sets the position to `null` and schedules one more pass, which
   * *removes* the property rather than writing a zero — so a dot at rest
   * carries no inline style at all and the CSS fallback is what applies. The
   * same removal runs from the effect's cleanup, because an unmounting rail can
   * leave its dots behind for a frame and a stuck swell is the one visible
   * failure this whole mechanism can have.
   *
   * **The track's own scroll is a third trigger**, and it is not decorative: the
   * track is a scrollport with `overflow-y: auto`, so a wheel over the rail
   * moves the dots under a stationary pointer. Without this the envelope stays
   * where the dots used to be — a curve centred on nothing.
   *
   * **And a change of the exchange set is a fourth**, for the same reason and
   * through a different door. This effect installs once, so nothing in it fires
   * when a turn arrives, history is prepended, or a refetch replaces the list:
   * every dot moves under a pointer that has not, and the lifts published for
   * the old positions stay written. Measured before the effect below existed:
   * with the pointer parked on a centred dot and one exchange prepended, that
   * dot slid a whole pitch and kept a lift of `1` — and kept it until the
   * reader moved, scrolled, or left. The scheduler is therefore handed out
   * through a ref and re-run on the exchange key, which asks for one ordinary
   * frame rather than tearing the listeners down and rebuilding them on every
   * turn of a live conversation.
   *
   * **A touch pointer publishes no lift at all.** A finger produces
   * `pointermove` too, and on a hybrid device (a laptop with a touchscreen
   * reports `pointer: fine`, so the stylesheet's spread is live) a tap
   * would otherwise leave a swell under wherever it landed with no second event
   * to clear it. The pointer type is checked here rather than trusted to the
   * media query for exactly that case.
   */
  useEffect(() => {
    const track = trackRef.current;
    if (track === null) return;
    /* The *array object* is stable for the life of the component — the render
       only ever writes into its slots and trims its tail, never replaces it —
       so holding it here reads the current dots on every pass and still gives
       the cleanup something that cannot have been swapped underneath it. Its
       length tracks the live dot count, which is what keeps the two caches
       below re-sized when a conversation gets shorter. */
    const dots = dotRefs.current;
    /** Where the pointer is, in client coordinates, or `null` for "not here". */
    let at: number | null = null;
    let queued: number | null = null;
    /** The last lift written per dot, so a pass that changes nothing writes
     *  nothing — which on a forty-dot rail is most of them, most frames. */
    let written: number[] = [];
    /**
     * And **which element each of those was written to**, because the value on
     * its own is a claim about a node that may not be on the page any more.
     *
     * The dots are keyed by exchange id, so a list that keeps its length and
     * changes its ids — which a refetch handing back re-identified turns would
     * be — unmounts every button and mounts a fresh one in its slot. The fresh
     * one carries no inline style, because the inline style is this effect's
     * and not the render's, while the cache still holds what was written to the
     * button that is gone. Measured on twelve dots with the pointer parked on
     * the sixth: the arc `4 / 4.609 / 6 / 7.375 / 8 / …` px and every one of
     * its `--nc-dot-lift` values vanished at the swap and did not come back on
     * a further move at the same y, because that pass computes the same lifts
     * and skips the same writes. The shoulders kept publishing the complement
     * of an envelope that was no longer on the page, so the 26.75px of aim the
     * spread had opened went back to the resting 12 with the track still
     * holding four openings of blank for it.
     *
     * Keyed by identity rather than by deleting the cache outright: the writes
     * it saves are only about forty string sets a frame against a pass that is
     * already doing a fixed point over every dot's box, but the reason to keep
     * it is that the skip is what makes a *quiet* frame — a wheel that moved
     * nothing, a settling step that converged — touch no style at all.
     */
    let writtenTo: (HTMLElement | null)[] = [];

    /*
     * ── One settling step, because the answer moves what the answer was read
     *    from ───────────────────────────────────────────────────────────────
     *
     * A pass reads where the pointer is on the list *from the layout the last
     * pass left*, and then changes that layout. Usually the two agree to within
     * nothing: the shoulders hold every dot at its resting position, so a
     * pointer that moved a few pixels lands on the row it looked like it was
     * on. They disagree when the layout the reading was taken from was shaped
     * around somewhere else entirely — a wheel over the rail is the case that
     * exposed it. Measured: the track scrolled 96px under a stationary pointer,
     * the pass read the dot under it *in the geometry spread around the dot the
     * pointer used to be on*, and settled on a row 1.6 rows away from the one
     * the resulting layout actually put there. The ink came out at 6.5px
     * against a peak of 8 and stayed there, because nothing else was going to
     * schedule another frame.
     *
     * So the pass re-reads and re-answers until the answer stops moving, which
     * on an ordinary pointer-move frame is the second read finding the same row
     * and stopping. The bound exists because a fixed point is an argument and
     * not a guarantee; four is far more than any measured case needs, and
     * falling out of it leaves the rail one settling step short for one frame
     * rather than spinning.
     */
    const paint = () => {
      queued = null;
      if (written.length !== dots.length) {
        written = Array.from({ length: dots.length }, () => Number.NaN);
        writtenTo = Array.from({ length: dots.length }, () => null);
      }
      let settled = Number.NaN;
      for (let step = 0; step < RAIL_SETTLE_STEPS; step += 1) {
          /* Read every box first, write after: one forced layout for the step. */
        const boxes = dots.map((dot) => dot?.getBoundingClientRect() ?? null);
        /*
         * Where the pointer is **on the list**: the index of the dot it is inside
         * plus how far through that dot it is, in the range ±0.5 of a row. The
         * nearest centre rather than a containment test, because the rows are
         * flush and a pointer on a boundary has to belong to one of them; the
         * clamp is what keeps a pointer in the track's own padding — above the
         * first row or below the last — from reporting a fraction that runs past
         * the end of the list.
         *
         * `null` when the pointer is away, and also when nothing has a box at
         * all: a rail inside a closed drawer is at rest by definition, and its
         * dots are zero-height, which would make every fraction a division by
         * zero rather than an answer.
         */
        let atDot: number | null = null;
        /* The index of the row the pointer is inside — the whole part of `atDot`,
           kept rather than recovered by rounding, because a pointer exactly on a
           row's lower edge has a fraction of +0.5 and would round into the next
           row while its lift belongs to this one. */
        let atRow = -1;
        /* Held in a `const` so the narrowing survives into the callback below,
           which `at`'s own `let` does not give TypeScript. */
        const pointerAt = at;
        if (pointerAt !== null) {
          let nearest = -1;
          let nearestGap = Number.POSITIVE_INFINITY;
          boxes.forEach((box, index) => {
            if (box === null || box.height === 0) return;
            const gap = Math.abs(box.top + box.height / 2 - pointerAt);
            if (gap < nearestGap) { nearestGap = gap; nearest = index; }
          });
          if (nearest >= 0) {
            const box = boxes[nearest]!;
            const through = (pointerAt - (box.top + box.height / 2)) / box.height;
            atDot = nearest + Math.max(-0.5, Math.min(0.5, through));
            atRow = nearest;
          }
        }
        const lifts = dots.map((dot, index) => {
          if (dot === null || atDot === null) return 0;
          const near = Math.max(0, 1 - Math.abs(index - atDot) / RAIL_SPREAD_SPAN);
          /* Smoothstep, not `near` itself: a straight ramp reaches the outermost
             dot with a non-zero slope, so the envelope ends on a visible corner
             and the two dots at the edge of the span pop as the pointer crosses
             them. This one leaves and arrives flat at both ends. */
          return Math.round(near * near * (3 - 2 * near) * 1000) / 1000;
        });
        /*
         * ── The shoulders, which are what stop the column sliding ─────────────
         *
         * The problem this solves, measured before it existed. The spread makes
         * the column longer, and where that extra length *goes* depends on how
         * the track is aligning its content, which changes with the length. On a
         * column short enough to be centred, half the growth is absorbed at each
         * end and a dot in the middle of the rail does not move at all — but a
         * dot near either end has a lopsided envelope (there are no dots past the
         * list to grow) and slid by 12px. On a column long enough to overflow,
         * `safe center` has degraded to `start`, nothing is absorbed at the top,
         * and the aimed dot slid by **49px** — four resting pitches, far enough
         * that the pointer was no longer inside the dot it had been put on. Two
         * different alignments, two different failures, one cause: the column's
         * length is a function of where the pointer is.
         *
         * So it is made not to be. The track carries a shoulder of blank at each
         * end, and each shoulder is exactly the growth that is *missing* from its
         * side — `RAIL_SPREAD_SPAN / 2` of an opening is what a fully interior
         * envelope puts above the pointer, and what is not currently there in
         * grown rows is made up in padding. Two consequences, and both are the
         * point:
         *
         *   - **The column's total length never changes.** Growth and shoulder
         *     always sum to `RAIL_SPREAD_SPAN` openings, hovered or not, at the
         *     ends of the list or the middle. The track's `scrollHeight` is
         *     therefore a constant, so the spread can never push a rail that fit
         *     past the cap, `safe center` never flips its alignment mid-hover,
         *     and the first and last dots stay exactly as reachable as they are
         *     at rest. That is the "spreading loses the end dots" failure, closed
         *     by construction rather than by a clamp.
         *   - **The distance from the track's top to the pointer's dot is a
         *     constant too**, under either alignment, which is the algebra the
         *     paragraph above wanted: the shoulder shrinks by precisely what the
         *     rows above the pointer grew. The dot under the pointer does not
         *     move, so the rail cannot walk out from under an aim.
         *
         * They are published as multiples of the opening rather than as pixels,
         * for the same reason the lift is: the stylesheet owns
         * `--nc-rail-pitch-open` and `--nc-rail-pitch`, and nothing here should
         * have to know what either of them is. The masses below are in the same
         * units — a sum of lifts — so both sides of that multiplication are the
         * component's and the pixel value is the stylesheet's.
         *
         * `p` is how far through its own row the pointer is, so a pointer between
         * two centres splits that row's growth between the two sides instead of
         * handing all of it to one — without which the shoulders step by a whole
         * opening as the pointer crosses a boundary, and the column jumps by 16px
         * at every dot.
         */
        let above = 0;
        let below = 0;
        if (atDot !== null) {
          const p = atDot - atRow + 0.5;
          lifts.forEach((lift, index) => {
            if (index < atRow) above += lift;
            else if (index > atRow) below += lift;
            else { above += lift * p; below += lift * (1 - p); }
          });
        }
        const shoulder = RAIL_SPREAD_SPAN / 2;
        track.style.setProperty('--nc-rail-lead', `${Math.max(0, shoulder - above)}`);
        track.style.setProperty('--nc-rail-tail', `${Math.max(0, shoulder - below)}`);

        lifts.forEach((lift, index) => {
          const dot = dots[index];
          if (dot == null || (writtenTo[index] === dot && written[index] === lift)) return;
          writtenTo[index] = dot;
          written[index] = lift;
          if (lift === 0) dot.style.removeProperty('--nc-dot-lift');
          else dot.style.setProperty('--nc-dot-lift', `${lift}`);
        });

        /* Settled when this step read the same place on the list as the last one,
           and immediately when there is no pointer to read: a release writes one
           set of zeroes and has nothing to converge on. */
        if (atDot === null || (step > 0 && Math.abs(atDot - settled) < 0.01)) break;
        settled = atDot;
      }
    };
    const schedule = () => {
      if (queued !== null) return;
      queued = requestAnimationFrame(paint);
    };
    const onMove = (event: PointerEvent) => {
      at = event.pointerType === 'touch' ? null : event.clientY;
      schedule();
    };
    const rest = () => { at = null; schedule(); };

    track.addEventListener('pointermove', onMove, { passive: true });
    track.addEventListener('pointerleave', rest);
    track.addEventListener('pointercancel', rest);
    track.addEventListener('scroll', schedule, { passive: true });
    repaintEnvelope.current = schedule;
    return () => {
      repaintEnvelope.current = NOTHING_TO_REPAINT;
      track.removeEventListener('pointermove', onMove);
      track.removeEventListener('pointerleave', rest);
      track.removeEventListener('pointercancel', rest);
      track.removeEventListener('scroll', schedule);
      if (queued !== null) cancelAnimationFrame(queued);
      for (const dot of dots) dot?.style.removeProperty('--nc-dot-lift');
      /* The shoulders go back to the stylesheet's own resting pair for the same
         reason the lifts do: an unmounting rail can leave its track behind for
         a frame, and a stuck shoulder is a column parked off its centre. */
      track.style.removeProperty('--nc-rail-lead');
      track.style.removeProperty('--nc-rail-tail');
    };
  }, []);

  /* The fourth trigger, from the note above: the dots moved because the list
     did, so ask for the same frame a pointer move would have asked for. It is
     a no-op when nothing is hovered — `at` is `null` and every lift the pass
     computes is 0, which is what the dots already carry. */
  useEffect(() => { repaintEnvelope.current(); }, [exchangeKey]);

  /*
   * ── Where the preview sits ────────────────────────────────────────────────
   *
   * Centred on the dot it describes, and then held between the track's own two
   * edges. Both halves are needed: centring is what makes it obvious *which*
   * dot is being described on a rail whose dots are a pitch apart, and the
   * clamp is what keeps a panel next to the first or the last dot from starting
   * above the track or running past it and being cut by the pane's edge with
   * half a sentence showing.
   *
   * **The clamp is a legibility fix and not a containment one, and the note on
   * `.railPreview` used to overstate it.** It bounds the panel's *top*; a panel
   * taller than the track still overruns the track's bottom, measured at 205px
   * against a track bottom of 165px in a 240px drawer with a full-cap prompt.
   * What actually keeps this layer off the composer is the pane's own
   * `overflow` clip. The clamp only ever changes anything at the two ends,
   * which is where the browser tier asserts it.
   *
   * The write is in block-start rather than a transform because the clamp needs
   * the box's real height, which is only knowable after it has laid out at its
   * final width.
   *
   * A layout effect and not a passive one: this runs in the commit that first
   * paints the layer, so the reader never sees a frame of it at the track's top
   * edge before it moves to the dot.
   *
   * **And it runs again on the track's own scroll**, which is the same bug the
   * envelope has a `scroll` listener for and which was left unfixed one
   * function away. The track is a scrollport, so a wheel over the rail moves
   * the dot while the panel — positioned against `.rail`, which does not scroll
   * — stays exactly where it was: measured at 80px of track scroll, 80px of
   * separation, with the panel still pointing at nothing. The listener is armed
   * only while a preview is up, because there is no position to maintain
   * otherwise.
   */
  useLayoutEffect(() => {
    const preview = previewRef.current;
    const rail = railRef.current;
    const track = trackRef.current;
    if (preview === null || rail === null || track === null) return;
    const dot = dotRefs.current[exchanges.findIndex((exchange) => exchange.id === previewed)];
    if (dot == null) return;
    const place = () => {
      const trackBox = track.getBoundingClientRect();
      const dotBox = dot.getBoundingClientRect();
      const height = preview.getBoundingClientRect().height;
      const wanted = dotBox.top + dotBox.height / 2 - height / 2;
      const lowest = Math.max(trackBox.top, trackBox.bottom - height);
      const top = Math.min(Math.max(wanted, trackBox.top), lowest);
      preview.style.insetBlockStart = `${top - rail.getBoundingClientRect().top}px`;
    };
    place();
    track.addEventListener('scroll', place, { passive: true });
    return () => { track.removeEventListener('scroll', place); };
  }, [previewed, exchanges]);

  /* An armed delay outlives the rail if nothing cancels it, and what it would
     do on firing is set state on an unmounted component. */
  useEffect(() => () => {
    if (previewDelay.current !== null) clearTimeout(previewDelay.current);
  }, []);

  /*
   * ── Warm up once, then follow the pointer ─────────────────────────────────
   *
   * The delay is what keeps this from being noise: a pointer crossing the rail
   * on its way somewhere else passes over a dozen dots, and a preview that
   * appeared on each of them would be a strobe rather than an aid. So the first
   * one waits `RAIL_PREVIEW_DELAY_MS`.
   *
   * **Once it is up, moving between dots swaps it with no second wait**, and
   * that asymmetry is deliberate. The reader who has waited for a preview is
   * reading previews, and making them wait again for the neighbour turns
   * "compare these two exchanges" into two full delays and a gap of nothing in
   * between — which on a rail this dense is most of what anyone does with this.
   * It is the ordinary warm-up model every tooltip group uses, and the
   * cool-down is a single event: leaving the rail.
   *
   * **"Up" means a panel that rendered, and the test for it is the ref rather
   * than the state.** `previewed` records what was *armed*, and those are not
   * the same thing: `previewText` can be `''` for an exchange that is in the
   * list — `railLabel` handles that case explicitly, so the codebase already
   * holds empty prompts to be reachable — and the exchange can be dropped
   * outright by a refetch or by history loading. Both leave `previewed` set
   * with nothing on screen, and both used to satisfy the branch below.
   * Measured: rest on a dot whose prompt collapses to `''` (no panel appears),
   * then cross a neighbouring dot, and its panel was up 120ms later against a
   * 450ms delay — the strobe the delay exists to prevent, reached through the
   * one path that never showed the reader anything to warm up from. `previewRef
   * .current` is non-null exactly while a panel is mounted, which is the
   * condition the sentence above has always described.
   */
  const dropPreview = () => {
    if (previewDelay.current !== null) {
      clearTimeout(previewDelay.current);
      previewDelay.current = null;
    }
    setPreviewed(null);
  };
  const previewOnRest = (id: string) => {
    if (previewDelay.current !== null) clearTimeout(previewDelay.current);
    if (previewRef.current !== null) {
      previewDelay.current = null;
      setPreviewed(id);
      return;
    }
    previewDelay.current = window.setTimeout(() => {
      previewDelay.current = null;
      setPreviewed(id);
    }, RAIL_PREVIEW_DELAY_MS);
  };

  const previewText = previewed === null
    ? ''
    : railPreviewText(exchanges.find((exchange) => exchange.id === previewed)?.text ?? '');

  const rove = (to: number) => {
    const next = Math.max(0, Math.min(exchanges.length - 1, to));
    setRoved(exchanges[next]?.id ?? null);
    const dot = dotRefs.current[next];
    /* `preventScroll`, then scroll the track by hand: the default would scroll
       every ancestor scrollport, the pane included. */
    dot?.focus({ preventScroll: true });
    keepInRailView(trackRef.current, dot);
  };

  return (
    <div
      className={styles.rail}
      /*
       * `group`, not `nav`, and the reason is the same one the composer's perch
       * gives: this sits inside a drawer that is already a `complementary`
       * landmark, and a second landmark inside it buys a reader one more entry
       * in their landmark list for a control they reach by Tab anyway. Named,
       * because a set of unlabelled buttons is a set of unlabelled buttons.
       */
      role="group"
      aria-label="Jump to an exchange"
      ref={railRef}
      /* The cool-down, on the group rather than on the track, so that the
         layer's own box — which is a child of this and overhangs the
         transcript — cannot count as leaving. It takes no events either
         (`pointer-events: none`), so the two guards are independent. */
      onPointerLeave={dropPreview}
    >
      <div className={styles.railTrack} data-nc-rail-track="" ref={trackRef}>
        {exchanges.map((exchange, index) => {
          const label = railLabel(exchange.text);
          /* The ordinal is in the name whether or not there is a prompt to add
             to it. Without it, five turns that all say "Continue" — which is
             what a long session with an agent is mostly made of — produce five
             buttons a screen reader cannot tell apart, and a rail whose whole
             job is "which one" would be answering "one of these five". */
          const ordinal = `exchange ${index + 1}`;
          return (
            <button
              key={exchange.id}
              /*
               * Writing `null` into the slot is not enough on its own. The
               * array is never shortened by the render, so its length is the
               * *historical maximum* number of exchanges: a conversation that
               * went to a hundred and was replaced by one of four left
               * `dotRefs.current` ninety-six slots long, every one of them a
               * strong reference to a button that had left the DOM, held until
               * the whole rail unmounted — and the spread's pass scanned all
               * hundred slots every frame to find four boxes. So the tail of
               * detached slots is dropped too. Trailing-only, because the slot
               * this is called for is not always the last one: an inline `ref`
               * closure is a new function on every render, so React detaches
               * every dot and re-attaches every dot on each pass, and trimming
               * to `index` would cut live entries out from under the ones that
               * have not been detached yet.
               *
               * **No test binds this, and none is added to.** What it changes
               * is retention and the per-frame scan's length, and both are
               * invisible from outside: reverting the trim to the bare
               * `dotRefs.current[index] = node` leaves all 52 cases in
               * `thread.browser.test.tsx` and all 17 in
               * `thread.coarse.browser.test.tsx` green, and an adversarial
               * shrink / grow / id-swap probe reading the full per-dot ink
               * profile came back byte-identical with the trim and without it —
               * every slot the scan skips is one whose `null` it would have
               * skipped anyway. Binding it would take a handle on the array
               * itself: a `data-` attribute carrying its length, or an exported
               * counter, both of which are production surface that exists only
               * to be read by a test. That trade is refused; an unasserted note
               * is the honest record.
               */
              ref={(node) => {
                dotRefs.current[index] = node;
                if (node !== null) return;
                while (dotRefs.current.length > 0 && dotRefs.current.at(-1) === null) {
                  dotRefs.current.length -= 1;
                }
              }}
              type="button"
              className={`${styles.railDot} ${exchange.id === active ? styles.railDotActive : ''}`}
              /* The prompt in the name, for everyone — and for a pointer, again
                 in the layer below, which this arms. `aria-current` rather than
                 a second class the screen reader cannot see: "the one you are
                 in" is a state, and it has one. */
              aria-label={label === '' ? `Jump to ${ordinal}` : `Jump to ${ordinal}: ${label}`}
              tabIndex={index === tabStop ? 0 : -1}
              {...(exchange.id === active ? { 'aria-current': true as const } : {})}
              onPointerEnter={(event) => {
                /* A finger has no hover: the first pointer event it sends is
                   the press, so a preview armed from one is a panel that
                   appears *after* the reader has already gone somewhere. */
                if (event.pointerType === 'touch') return;
                previewOnRest(exchange.id);
              }}
              onFocus={() => { setRoved(exchange.id); }}
              /* On the button rather than on the group: the group is not an
                 interactive element, and a key handler on it would be reached
                 only through the same buttons anyway. */
              onKeyDown={(event) => {
                const move = ARROW_MOVES[event.key];
                if (move === undefined) return;
                event.preventDefault();
                rove(move(index, exchanges.length));
              }}
              onClick={() => {
                /* The press is the answer the preview was offering. Leaving it
                   up would sit it over the transcript the jump has just
                   brought into view. */
                dropPreview();
                onJump(exchange.id);
              }}
            />
          );
        })}
      </div>
      {/* `aria-hidden`, and it is the whole reason this may exist at all — the
          same sentence is already the pressed button's accessible name, so this
          is a second *rendering* of one fact rather than a second channel. See
          `.railPreview` in the stylesheet for what it is not allowed to do to
          the layout. */}
      {previewText !== '' && (
        <div
          className={styles.railPreview}
          data-nc-rail-preview=""
          aria-hidden="true"
          ref={previewRef}
        >
          {previewText}
        </div>
      )}
    </div>
  );
}

/** Up/Down between neighbours, Home/End to the ends — the model every roving
 *  group in every toolkit uses, and nothing else bound so a reader's own
 *  shortcuts still reach the page. */
const ARROW_MOVES: Readonly<Record<string, ((from: number, count: number) => number) | undefined>> =
  Object.freeze({
    ArrowDown: (from: number) => from + 1,
    ArrowUp: (from: number) => from - 1,
    Home: () => 0,
    End: (_from: number, count: number) => count - 1,
  });

/** The smallest write to the rail's own `scrollTop` that puts `dot` inside
 *  `track` — nothing at all when it is already there. */
function keepInRailView(track: HTMLElement | null, dot: HTMLElement | null | undefined): void {
  if (track === null || dot == null) return;
  const dotBox = dot.getBoundingClientRect();
  const trackBox = track.getBoundingClientRect();
  if (dotBox.top < trackBox.top) track.scrollTop += dotBox.top - trackBox.top;
  else if (dotBox.bottom > trackBox.bottom) track.scrollTop += dotBox.bottom - trackBox.bottom;
}

/** One thing you said and everything that came back — as far as the rail needs
 *  it: something to point at, and the words to call it by. */
type Exchange = Readonly<{ id: string; text: string }>;

/**
 * Below this many, the rail does not exist.
 *
 * Five is where a transcript stops fitting in a drawer at a glance: four
 * exchanges of a sentence and a paragraph each are roughly one pane, so the
 * reader who wants the second one scrolls to it and can *see* where it is. The
 * number is here rather than inline because it is a product judgement about
 * when an aid earns its ink, and the next person to disagree with it should be
 * changing a named thing.
 */
export const EXCHANGE_RAIL_MIN = 5;

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
function observeResize(element: Element, onResize: () => void): () => void {
  if (typeof ResizeObserver === 'undefined') return () => {};
  const observer = new ResizeObserver(onResize);
  observer.observe(element);
  return () => { observer.disconnect(); };
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
 * How much of a prompt the floating preview may show — four times what the
 * accessible name carries, and the difference is the point.
 *
 * `RAIL_LABEL_MAX` is short because a screen reader announcing a button should
 * not read a paragraph, and that constraint is about *announcing*, not about
 * the prompt. A panel you glance at has the opposite problem: 60 characters cut
 * mid-clause is often exactly as ambiguous as the ordinal alone, which is the
 * case the preview exists for. 240 is a sentence or two — three lines at the
 * preview's `--text-xs` on a wide drawer, and measurably more at the 240px
 * floor `--panel-span` allows, which is why it is stated as a character count
 * and not as a number of lines. It is as much as can be read at a glance
 * without the layer becoming a thing you *read* rather than glance at.
 */
const RAIL_PREVIEW_MAX = 240;

/** The prompt as the floating layer may carry it. Same collapse as
 *  `railLabel`, a longer ceiling, and `''` for nothing to show. */
function railPreviewText(text: string): string {
  const line = text.replace(/\s+/g, ' ').trim();
  return line.length <= RAIL_PREVIEW_MAX ? line : `${line.slice(0, RAIL_PREVIEW_MAX - 1)}…`;
}

/**
 * How far the spread reaches, counted in dots.
 *
 * At one, only the dot under the pointer moves and the rail has a single bump
 * travelling along it — the degenerate case, and the one that does not help you
 * aim, because at the moment you can see which dot grew you are already on it.
 * Much past four and the *whole* rail visibly opens whenever a pointer is
 * anywhere near it, which is the loudness the density is being spent to get
 * away from, and the column's total growth (see below) starts to be worth more
 * than a screenful of the track.
 *
 * Four is the number, and it changed from three with the pitch. The falloff
 * reaches exactly zero at the fourth dot out, so what moves is the dot under
 * the pointer plus three neighbours a side — an arc of seven, eight when the
 * pointer sits between two centres. At the old 24px pitch three dots each way
 * was 72px of rail and already a long arc; at 12px it is 36px, which is short
 * enough that the spread reads as a kink rather than as a curve. Four restores
 * the arc's length in the reader's eye (48px each way) while keeping it well
 * inside the track.
 *
 * **The column's total growth is exactly this number times the opening**, and
 * that identity is worth knowing because it is what the track's length has to
 * absorb. Summed over the integers, `smoothstep(1 − |k|/n)` is `n`; at four
 * dots and a 12 → 28px opening that is `4 × 16 = 64px` of extra column while a
 * pointer is on the rail. The falloff being symmetric puts half of it on each
 * side of the pointer, which is what keeps the dot under the pointer still.
 *
 * It is a count of dots rather than of pixels for two reasons. It means the
 * same thing at either pitch; and, unlike a pixel distance, it cannot be
 * changed by the spread it is driving — which is the loop the envelope's own
 * note is about.
 */
const RAIL_SPREAD_SPAN = 4;

/**
 * How many times one frame may re-read the rail before giving up on settling.
 *
 * The envelope is read from the layout and then changes it, so a frame whose
 * starting layout was shaped around a *different* part of the list can answer
 * from geometry that its own answer invalidates. The full argument is on the
 * pass itself; the number is here because it is a bound on work per frame and
 * the next person to change it should be changing a named thing.
 *
 * Four, against a measured worst case of two. Every ordinary frame settles on
 * the second read — the first computes, the second confirms — because the
 * shoulders hold the dots still and a pointer that moved a few pixels is still
 * on the row it looked like it was on. The one case that needs a third is a
 * jump: a wheel over the rail, or history arriving, which moves every dot at
 * once under a pointer that has not moved. Two spare steps rather than one
 * because the cost of an unused step is a `getBoundingClientRect` pass that
 * changes nothing, and the cost of running out is a visibly wrong envelope.
 */
const RAIL_SETTLE_STEPS = 4;

/**
 * How long a pointer must rest on a dot before its prompt floats out.
 *
 * The two failures are on either side of it and are not symmetrical. Too short
 * and the layer fires on every dot a pointer crosses on its way to the
 * composer, which is a strobe over the transcript — the thing the reader is
 * actually looking at. Too long and it is a feature nobody discovers, because
 * nobody holds a mouse still on a 4px dot for a second on the off-chance. 450ms
 * sits between those two failures: comfortably longer than a pointer takes to
 * cross a dot at any speed anyone moves a mouse, and short enough to be found
 * by a reader who merely paused rather than one who waited on purpose.
 *
 * An earlier version of this note reached for platform tooltip delays —
 * "Windows' own is 500, the web's `title` is around a second" — as though the
 * number were derived from them. It was not; neither figure is citable from
 * anything in this repository, and both are wrong as stated (Chromium's `title`
 * delay is around 500ms, not a second, and Windows' documented `TTDT_INITIAL`
 * default is `GetDoubleClickTime() / 5`, not 500). Folklore stated as
 * measurement is the failure this file has already had once, so it is deleted
 * rather than softened.
 *
 * Named, and not inline, because it is the number the mutation check flips: at
 * 0 the layer appears on the first crossing, and the browser tier says so.
 */
const RAIL_PREVIEW_DELAY_MS = 450;

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
function ActivityLine({ activity, live }: {
  activity: ConversationActivity;
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
 * The composer is Astryx's ChatComposer: rounded well, auto-grow, send/stop
 * geometry, Enter-to-send with IME guard. We own the value and the send
 * callback so the kernel path stays a string.
 */
export function ChatComposer({
  onSend, onStop, onNewConversation, disabled = false, focusOnMount = false,
}: {
  onSend: (text: string) => void;
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
}) {
  const [draft, setDraft] = useState('');
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
        /* Astryx ChatComposerInput submits on Enter without an IME guard.
           Enter while composing accepts the candidate, it must not send. */
        if (event.key === 'Enter' && !event.shiftKey && event.nativeEvent.isComposing) {
          event.stopPropagation();
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
        /* Handed over whole — the "one interrupt at a time" rule is the
           router's, at the top of `interrupt()`. See the `onStop` prop note. */
        onStop={onStop}
        onSubmit={(value) => {
          const text = value.trim();
          if (text === '' || disabled || stopShown) return;
          onSend(text);
          setDraft('');
          /* The caret goes back to the field from the effect above, not from
             here: `onSend` may have already queued the `disabled` that takes
             the field away, and this handler runs before React flushes it. */
          wantsFieldFocus.current = true;
          /* A fresh request, so the effect's first run must not compare against
             a perch left over from an earlier one — see the effect's note. */
          parkedFocus.current = null;
          setSendCount((count) => count + 1);
        }}
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
export function ChatFooterNotice({ children }: { children: ReactNode }) {
  return <div role="alert" className={styles.footerNotice}>{children}</div>;
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
