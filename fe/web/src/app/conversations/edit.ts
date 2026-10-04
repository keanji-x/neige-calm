import { useEffect } from 'react';

import type { ConversationTurnOutcome, TranscriptEntry } from '../../../../core/domain/conversation.ts';
import { replacingTurn, type ReplacedTurn } from '../../../../core/domain/conversation-outbox.ts';
import { editedTurnRefill, isComposerEmpty, isLatestTurn } from '../../../../core/domain/conversation-rewind.ts';
import { useConversationRegistry } from './public.tsx';

/** Beside a refused replace while its words are still in the composer: they are a new message now. */
const REFILLED_NOTE = 'Your message is back in the composer; sending adds a new one.';

/**
 * Edit (#1923) as a composer mode for the open conversation. The click puts the turn's message in that conversation's
 * composer and asks the server nothing; the turn stays on screen. Send in edit mode is one keyed send that names the
 * turn it replaces (#2043): the outbox holds it from the press, and the server removes the turn and queues the
 * message in one commit, or refuses and changes nothing. Send outside edit mode is an ordinary new message.
 */
export function useConversationEdit({ conversationId, transcript, historyReady, focusComposer }: {
  conversationId: string | null;
  /** That conversation's transcript as the store holds it, the edited turn included while a read still shows it. */
  transcript: readonly TranscriptEntry[];
  /** Whether `transcript` is a read rather than a remembered fallback. */
  historyReady: boolean;
  /** Asks for the caret in that conversation's composer if it is the one shown. */
  focusComposer: (conversationId: string) => void;
}) {
  const { editOf, beginEdit, cancelEdit, leaveEdit, editNoticeOf, outboxOf, composerOf } = useConversationRegistry();
  const held = conversationId === null ? null : editOf(conversationId);
  const replacing = conversationId === null ? null : replacingTurn(outboxOf(conversationId));
  /* A turn that is no longer the latest cannot be replaced: the server only ever removes the latest. */
  const stale = historyReady && held !== null && !isLatestTurn(transcript, held.outcomeId) ? held.outcomeId : null;
  useEffect(() => {
    if (conversationId !== null && stale !== null) leaveEdit(conversationId, stale);
  }, [conversationId, leaveEdit, stale]);
  /** Enter edit mode for the turn that ends at `outcome`, the conversation's latest. */
  const start = (outcome: ConversationTurnOutcome) => {
    if (conversationId === null) return;
    const refill = editedTurnRefill(transcript, outcome.id);
    if (refill === null || !beginEdit(conversationId, { turnId: outcome.turnId, outcomeId: outcome.id, refill })) return;
    focusComposer(conversationId);
  };
  const notice = conversationId === null ? null : editNoticeOf(conversationId);
  const cancel = () => { if (conversationId !== null) cancelEdit(conversationId); };
  return {
    /** This conversation's Edit while one is held: nothing else acts on the conversation meanwhile. */
    held,
    /** What a Send pressed in `pressedIn` replaces: the held Edit's turn there, or `null` for a new message. */
    replacesIn: (pressedIn: string): ReplacedTurn | null => {
      const edit = editOf(pressedIn);
      return edit === null ? null : { turnId: edit.turnId, outcomeId: edit.outcomeId };
    },
    /** The edit bar while in edit mode. */
    bar: held === null ? undefined : { preview: held.refill.text, onCancel: cancel },
    /** A replace is out: Send is a spinner and the composer waits. */
    replacing: replacing !== null,
    /** The outcome of the turn shown marked: being edited, or being replaced until the server answers. */
    marked: held?.outcomeId ?? replacing?.outcomeId ?? null,
    /** What the last Edit came to, as shown above the composer; a turn that moved on is news, not a failure. The
     * refused line says where the words are only while they are there. */
    notice: notice === null ? null : notice.kind === 'refused'
      ? { tone: 'error', lines: [`Edit failed: ${notice.message}`,
        ...(conversationId === null || isComposerEmpty(composerOf(conversationId)) ? [] : [REFILLED_NOTE])] } as const
      : { tone: 'neutral', lines: ['This message can no longer be replaced; sending adds a new one.'] } as const,
    start,
  };
}
