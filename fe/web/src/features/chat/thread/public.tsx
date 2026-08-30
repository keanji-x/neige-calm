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

import { useEffect, useRef, type ReactNode } from 'react';
import {
  ChatComposer as AstryxChatComposer,
  ChatComposerInput,
  ChatSendButton,
} from '@astryxdesign/core/Chat';

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
 * The composer is Astryx's ChatComposer: rounded well, auto-grow, send/stop
 * geometry, Enter-to-send with IME guard. We own the value and the send
 * callback so the kernel path stays a string.
 */
export function ChatComposer({ onSend, onStop, stopping = false, disabled = false }: {
  onSend: (text: string) => void;
  onStop?: () => void;
  stopping?: boolean;
  disabled?: boolean;
}) {
  const [draft, setDraft] = useState('');
  const stopShown = onStop != null;

  return (
    <div
      className={styles.composer}
      data-nc-composer=""
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
        onStop={onStop}
        onSubmit={(value) => {
          const text = value.trim();
          if (text === '' || disabled || stopShown) return;
          onSend(text);
          setDraft('');
        }}
        input={<ChatComposerInput label="Message" placeholder="Say something" />}
        sendButton={<ChatSendButton isDisabled={stopping} />}
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

/**
 * ── Reset, at the end of the transcript ───────────────────────────────────
 *
 * **Where it is.** The last line of the drawer's body, after `<ChatThread>`,
 * in the scrolling content — not in the card's corner, where it used to be a
 * red 28px glyph 2px from the close.
 *
 * The corner was the wrong place for it twice over. It put the only
 * destructive control on the surface at the top of a panel whose top is what
 * you look at first, and it painted `--error-text` above a *blank*
 * conversation, so the loudest thing on a fresh drawer was a warning about
 * something that had not happened yet. The end of the transcript is where the
 * thought "this thread is done" actually occurs: you have just read the last
 * reply, and the reset is one line under it. The transcript follows the newest
 * turn on its own (see the scroll effect above), so the end of it is where the
 * reader already is.
 *
 * **The price, stated plainly: it scrolls away.** It is content, so scrolling
 * back through history puts it off screen — unlike the close, which floats and
 * is always there. That is the intended shape, not a defect, and it is
 * affordable for three reasons that hold together: it is done at most once per
 * thread, it is never urgent (nothing is waiting on a reset), and the way back
 * to it is the same gesture the reader was already making — scroll to the
 * bottom, which is also where the composer they are aiming for lives. The
 * `ConfirmDialog` behind it is unchanged, so even a mis-click at the end of a
 * long scroll costs one Escape.
 *
 * **What it looks like.** Words, not a glyph: `Reset conversation`, the whole
 * of what the icon's `aria-label` used to carry, now readable by everyone
 * rather than only by a screen reader. `data-nc-action="destructive"` is
 * §4.3's tier — `--error-text` at rest, red before the pointer arrives — and
 * it is the shared vocabulary from base.css rather than a colour minted here.
 * The stylesheet drops it to the caption rank the activity lines use; the
 * reason is there.
 *
 * It is only rendered when there is a transcript to throw away; the router
 * owns that condition, and says why.
 */
export function ChatResetAction({ onClick }: { onClick: () => void }) {
  return (
    <button
      type="button"
      data-nc-action="destructive"
      className={styles.resetAction}
      onClick={onClick}
    >
      Reset conversation
    </button>
  );
}

/** A wall clock, not a relative time: the separator exists to say *when*, and
 *  "3h" is only useful when you already know when now is. */
function clockTime(atMs: number): string {
  return new Date(atMs).toLocaleTimeString('en-US', { hour: 'numeric', minute: '2-digit' });
}
