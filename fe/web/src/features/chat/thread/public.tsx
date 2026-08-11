// The conversation itself: a transcript and the box you write into.
//
// ── Why it does not look like a chat app ──────────────────────────────────
//
// Three conventions are deliberately not used, and each is a measurement
// rather than a preference:
//
//   **No bubbles.** A bubble is a variable-width column, so the prose inside it
//   has no measure — every turn wraps at a different width. In the 372px this
//   drawer actually has, a bubble with its own padding leaves ~330, and the
//   text inside it runs 45 characters a line against the 65–75 prose is read
//   at. A bubble also spends a whole shape to encode one bit: who spoke.
//
//   **No side-swapping.** Aligning your turns right destroys the one flush left
//   edge that makes a narrow column readable, and it does it to gain the same
//   one bit a two-word label already carries.
//
//   **No avatars.** Identity here is binary and the label states it. A 24px
//   glyph restating a five-character word is the widest thing on the row.
//
// What replaces them is the vocabulary this app already has: an uppercase
// micro-label (the same tier the rail's sections and the card's module heads
// use), a relative time, and prose. Who spoke is carried by that label plus the
// ink weight of the body — `--text` for what you wrote, `--text-2` for what
// came back — which are two channels the system already spends elsewhere.

import { useEffect, useRef, type FormEvent, type KeyboardEvent } from 'react';

import { useState } from '../../../ui/state/public.ts';

import {
  isLiveConversation, labelledTurns,
  type Conversation, type ConversationTurn,
} from '../../../../../core/domain/conversation.ts';
import styles from './thread.module.css';

const AUTHOR_LABEL = Object.freeze({ you: 'You', agent: 'Agent' });

export type ChatThreadProps = Readonly<{
  conversation: Conversation;
  turns: readonly ConversationTurn[];
  /** True while a turn is in flight; the composer stays usable, the dot pulses. */
  pending?: boolean;
  nowMs?: number;
}>;

export function ChatThread({ conversation, turns, pending = false, nowMs }: ChatThreadProps) {
  const now = nowMs ?? Date.now();
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
      {labelledTurns(turns).map(([turn, labelled]) => (
        <article
          key={turn.id}
          className={`${styles.turn} ${labelled ? '' : styles.turnRun}`}
          data-nc-turn={turn.author}
        >
          {/* Only the first turn of a run is labelled — see `labelledTurns`. */}
          {labelled && (
            <p className={styles.meta}>
              <span className={styles.author}>{AUTHOR_LABEL[turn.author]}</span>
              <span className={styles.time}>{shortAge(turn.atMs, now)}</span>
              {/* The one live mark, and it is the same 6px accent dot that means
                  "running" on a wave row. One vocabulary for "something is
                  happening", not a second one for chat. */}
              {live && turn.author === 'agent' && turn === turns[turns.length - 1] && (
                <span className={styles.live} aria-label="Working" />
              )}
            </p>
          )}
          <p className={`${styles.body} ${turn.author === 'you' ? styles.bodyYou : ''}`}>
            {turn.text}
          </p>
        </article>
      ))}
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
      <div className={styles.composerActions}>
        {/*
          `aria-disabled`, not `disabled` — §5.1. A truly disabled button drops
          focus the moment it becomes unusable, which here is the moment you
          send: focus would land on `<body>` mid-conversation.
        */}
        <button
          type="submit"
          data-nc-action="primary"
          aria-disabled={ready ? undefined : 'true'}
          className={styles.send}
        >
          Send
        </button>
      </div>
    </form>
  );
}

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/** §2.2's relative time, floored to one unit. The same rule the rows use; a
 *  feature may not import a sibling domain, so it is re-declared, not shared. */
function shortAge(atMs: number, nowMs: number): string {
  const elapsed = Math.max(0, nowMs - atMs);
  if (elapsed >= DAY) return `${Math.floor(elapsed / DAY)}d`;
  if (elapsed >= HOUR) return `${Math.floor(elapsed / HOUR)}h`;
  if (elapsed >= MINUTE) return `${Math.floor(elapsed / MINUTE)}m`;
  return 'now';
}
