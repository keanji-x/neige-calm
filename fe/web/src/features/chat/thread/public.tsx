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

import { useEffect, useMemo, useRef, type ReactNode } from 'react';
import {
  ChatComposer as AstryxChatComposer,
  ChatComposerInput,
  ChatSendButton,
  type ChatComposerTrigger,
} from '@astryxdesign/core/Chat';
import { createStaticSource } from '@astryxdesign/core/Typeahead';

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

  /*
   * Follow the newest turn. Keyed on the count rather than the array so a
   * re-render that changes nothing does not yank the view out from under
   * someone reading back through the thread.
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
    scroller.scrollTop = scroller.scrollHeight;
  }, [turns.length]);

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
    <div className={styles.thread} data-nc-thread="">
      {turns.map((turn, index) => {
        const last = index === turns.length - 1;
        if (turn.author === 'activity') {
          return <ActivityLine key={turn.id} activity={turn} live={live && last} />;
        }
        return (
          <div key={turn.id} className={opensExchange(turns, index) ? styles.exchange : undefined}>
            {/* A time only where the conversation restarted. */}
            {opensAfterGap(turns, index) && index > 0 && (
              <p className={styles.gap}>{clockTime(turn.atMs)}</p>
            )}
            <p
              className={turn.author === 'you' ? styles.said : styles.reply}
              data-nc-turn={turn.author}
            >
              {turn.text}
              {live && last && turn.author === 'agent' && (
                <span className={styles.live} aria-label="Working" />
              )}
            </p>
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
  );
}

/**
 * One action, one line.
 *
 * The dot is the same 6px accent pulse a running wave row wears, and it is here
 * for the same reason it is there: it is the one place in the app that says
 * "this is happening right now". A running action is the honest place for it in
 * a transcript — before this existed, a four-minute turn spent entirely in
 * shell runs and a `report.write` looked from the drawer like nothing at all.
 */
function ActivityLine({ activity, live }: {
  activity: ConversationActivity;
  live: boolean;
}) {
  const running = activity.state === 'running';
  return (
    <p
      className={`${styles.activity} ${activity.state === 'failed' ? styles.activityFailed : ''}`}
      data-nc-state={activity.state}
    >
      <span>{activity.verb}</span>
      {activity.target !== null && <span className={styles.activityTarget}>{activity.target}</span>}
      {activity.state === 'failed' && <span className={styles.activityFailure}>Failed</span>}
      {running && live && <span className={styles.live} aria-label="Working" />}
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
  onSend, onStop, onNewConversation, disabled = false,
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
   * behaviour, because the rule it was reaching for is already enforced one
   * layer up, at the top of the router's `interrupt()`:
   * `if (!working || stopping) return;`. A second press is a no-op there, where
   * the state that decides it actually lives.
   *
   * Making the button genuinely unavailable instead is not on offer from out
   * here: `ChatSendButton` accepts neither `isDisabled` nor the `tooltip`
   * Astryx requires before it will render `aria-disabled` — see the `sendButton`
   * note below.
   */
  onStop?: () => void;
  /** Start a new conversation — the *same* callback the module head's `+`
   *  fires. Absent where the `+` is absent, and its absence is what keeps the
   *  `/` menu from existing at all. */
  onNewConversation?: () => void;
  disabled?: boolean;
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
  const wantsFieldFocus = useRef(false);

  /*
   * ── Put the caret back after a send, and keep trying until it lands ───────
   *
   * Load-bearing, not a nicety — see the `sendButton` note below: Send is a
   * natively disabled control the moment the draft empties, and a natively
   * disabled control that currently holds focus hands that focus to `<body>`.
   * Sending from the button is the one path that puts focus there first.
   *
   * **Why this is an effect and not a line at the end of `onSubmit`.** It was
   * that line, and on the app's own wiring it did nothing. The router passes
   * `disabled={store.sending}` at both call sites and `send()` opens with a
   * synchronous `setSending(true)`, so the real order inside one click is:
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
   * If the reader has moved focus somewhere else entirely in the meantime, the
   * request is dropped: yanking focus back into a box they have left is worse
   * than not restoring it.
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
    const active = document.activeElement;
    if (active !== null && active !== document.body && !root.contains(active)) {
      wantsFieldFocus.current = false;
      return;
    }
    const messageField = root.querySelector<HTMLElement>('[contenteditable="true"], textarea');
    messageField?.focus();
    if (messageField !== null && document.activeElement === messageField) {
      wantsFieldFocus.current = false;
      return;
    }
    if (!root.contains(document.activeElement)) root.focus({ preventScroll: true });
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
         * wants it regardless. See `returnFocusToField`. That leaves "Send is
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
