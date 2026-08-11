// The conversation itself: a transcript and the box you write into.
//
// ── Why it does not look like a chat app ──────────────────────────────────
//
// **No bubbles.** A bubble is a variable-width column, so the prose inside it
// has no measure — every turn wraps at a different width. In the 372px this
// drawer actually has, a bubble with its own padding leaves ~330, and the text
// inside runs 45 characters a line against the 65–75 prose is read at. A bubble
// also spends a whole shape to encode one bit: who spoke.
//
// **No side-swapping.** Aligning your turns right destroys the one flush left
// edge that makes a narrow column readable, to gain the same one bit.
//
// **No per-turn labels, and no per-turn timestamps.** This is where the first
// version of this file was wrong, and it was wrong by measurement: in a strict
// alternation "YOU" and "AGENT" appeared eight times down four exchanges, and
// "now" eight times beside them. Sixteen lines of chrome restating the two
// facts the reader already had — that they are in this conversation, and that
// turns alternate.
//
// What carries "who" instead is **register**, which is free: what you wrote is
// interface type at full ink and medium weight, a directive; what came back is
// body tone at normal weight, the substance. What carries "when" is a
// separator, printed only where the conversation actually stopped and started
// again (`CONVERSATION_GAP_MS`), which is the only time the answer is not
// "just now".
//
// The unit is the **exchange** — one thing you said and everything that came
// back — and the layout groups by it: tight inside, loose between.

import { useEffect, useRef, type FormEvent, type KeyboardEvent } from 'react';

import { useState } from '../../../ui/state/public.ts';

import {
  isLiveConversation, opensAfterGap, opensExchange,
  type Conversation, type ConversationTurn,
} from '../../../../../core/domain/conversation.ts';
import styles from './thread.module.css';

export type ChatThreadProps = Readonly<{
  conversation: Conversation;
  turns: readonly ConversationTurn[];
  /** True while a turn is in flight; the composer stays usable, the dot pulses. */
  pending?: boolean;
}>;

export function ChatThread({ conversation, turns, pending = false }: ChatThreadProps) {
  const live = pending || isLiveConversation(conversation.state);
  const endRef = useRef<HTMLDivElement | null>(null);

  /*
   * Follow the newest turn. Keyed on the count rather than the array so a
   * re-render that changes nothing does not yank the view out from under
   * someone reading back through the thread.
   *
   * `block: 'end'` with no smooth behaviour: this is the app's own chrome
   * moving, and principle 3 ("stay quiet through continuous change") applies to
   * it as much as to anything else.
   */
  useEffect(() => {
    // Optional call: `scrollIntoView` is a layout API, and the DOM this runs
    // in during tests has no layout to scroll.
    endRef.current?.scrollIntoView?.({ block: 'end' });
  }, [turns.length]);

  if (turns.length === 0) {
    return (
      <div className={styles.empty} data-nc-thread-empty="">
        <p className={styles.emptyLead}>Nothing said yet.</p>
        <p className={styles.emptyHint}>Write below and it starts here.</p>
      </div>
    );
  }

  return (
    <div className={styles.thread} data-nc-thread="">
      {turns.map((turn, index) => {
        const last = index === turns.length - 1;
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
              {/* The one live mark, and it is the same 6px accent dot that means
                  "running" on a wave row. One vocabulary for "something is
                  happening", not a second one for chat. */}
              {live && last && turn.author === 'agent' && (
                <span className={styles.live} aria-label="Working" />
              )}
            </p>
          </div>
        );
      })}
      {/* A reply that has not arrived yet still gets a place to arrive in. */}
      {live && turns[turns.length - 1]?.author === 'you' && (
        <p className={styles.reply}><span className={styles.live} aria-label="Working" /></p>
      )}
      <div ref={endRef} aria-hidden="true" />
    </div>
  );
}

/**
 * The composer.
 *
 * Enter sends and Shift+Enter breaks the line, which is the convention every
 * agent surface uses; the button exists anyway because a keyboard convention is
 * not an affordance, and it is the one primary action this surface has (§4.1 —
 * at most one per surface).
 *
 * The field grows with what you type, to a ceiling. A fixed three-row box is
 * wrong in both directions: it wastes two rows of a 396px drawer for the
 * one-line instruction that is the common case, and it still needs a scrollbar
 * for the long one.
 */
export function ChatComposer({ onSend, disabled = false }: {
  onSend: (text: string) => void;
  disabled?: boolean;
}) {
  const [draft, setDraft] = useState('');
  const fieldRef = useRef<HTMLTextAreaElement | null>(null);
  const ready = draft.trim() !== '' && !disabled;

  const send = () => {
    if (!ready) return;
    onSend(draft.trim());
    setDraft('');
    // Focus stays in the field: sending one message is almost never the end of
    // what you came to say.
    fieldRef.current?.focus();
  };

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key !== 'Enter' || event.shiftKey) return;
    event.preventDefault();
    send();
  };

  return (
    <form
      className={styles.composer}
      data-nc-composer=""
      onSubmit={(event: FormEvent) => { event.preventDefault(); send(); }}
    >
      <textarea
        ref={fieldRef}
        className={styles.field}
        value={draft}
        rows={1}
        aria-label="Message"
        placeholder="Say something"
        disabled={disabled}
        onChange={(event) => setDraft(event.target.value)}
        onKeyDown={onKeyDown}
      />
      {/*
        `aria-disabled`, not `disabled` — §5.1. A truly disabled button drops
        focus the moment it becomes unusable, which here is the moment you send:
        focus would land on `<body>` mid-conversation.
      */}
      <button
        type="submit"
        data-nc-action="primary"
        aria-disabled={ready ? undefined : 'true'}
        className={styles.send}
      >
        Send
      </button>
    </form>
  );
}

/** A wall clock, not a relative time: the separator exists to say *when*, and
 *  "3h" is only useful when you already know when now is. */
function clockTime(atMs: number): string {
  return new Date(atMs).toLocaleTimeString('en-US', { hour: 'numeric', minute: '2-digit' });
}
