import { useEffect } from 'react';

import type { ConversationTurnOutcome, SendOutcome, TranscriptEntry } from '../../../../core/domain/conversation.ts';
import { editedTurnRefill, isLatestTurn, REWIND_FAILURES, type PlannerRewind } from '../../../../core/domain/conversation-rewind.ts';
import { classifyFailure } from '../../../../core/domain/failure-class.ts';
import { ApiError } from '../providers/queries.ts';
import { useConversationRegistry } from './public.tsx';

/**
 * Edit (#1923) as a composer mode for the open conversation. The click puts the turn's message in that conversation's
 * composer and asks the server nothing; the turn stays on screen. Send in edit mode replaces it: the rewind first, and
 * the message only after its 200. Send outside edit mode is an ordinary new message.
 */
export function useConversationEdit({ conversationId, transcript, historyReady, rewind, focusComposer }: {
  conversationId: string | null;
  /** That conversation's transcript as the store holds it, the edited turn included while a read still shows it. */
  transcript: readonly TranscriptEntry[];
  /** Whether `transcript` is a read rather than a remembered fallback. */
  historyReady: boolean;
  /** Bound to `conversationId`'s card. */
  rewind: (turnId: string) => Promise<PlannerRewind>;
  /** Asks for the caret in that conversation's composer if it is the one shown. */
  focusComposer: (conversationId: string) => void;
}) {
  const { editOf, beginEdit, cancelEdit, beginReplace, finishReplace, leaveEdit, forgetEdit, editNoticeOf } = useConversationRegistry();
  const held = conversationId === null ? null : editOf(conversationId);
  const read = historyReady && held !== null;
  /* A replaced turn stays hidden until a read without it lands, so a stale cache never shows it again. */
  const forgettable = read && held.phase === 'replaced' && !transcript.some((entry) => entry.id === held.outcomeId) ? held.outcomeId : null;
  /* A turn that is no longer the latest cannot be replaced: the rewind only ever removes the latest. */
  const stale = read && held.phase === 'editing' && !isLatestTurn(transcript, held.outcomeId) ? held.outcomeId : null;
  useEffect(() => {
    if (conversationId === null) return;
    if (forgettable !== null) forgetEdit(conversationId, forgettable);
    if (stale !== null) leaveEdit(conversationId, stale);
  }, [conversationId, forgettable, forgetEdit, leaveEdit, stale]);
  /** Enter edit mode for the turn that ends at `outcome`, the conversation's latest. */
  const start = (outcome: ConversationTurnOutcome) => {
    if (conversationId === null) return;
    const refill = editedTurnRefill(transcript, outcome.id);
    if (refill === null || !beginEdit(conversationId, { turnId: outcome.turnId, outcomeId: outcome.id, refill })) return;
    focusComposer(conversationId);
  };
  /**
   * A send pressed in `pressedIn`; `sendNow` is bound to that conversation and to what was pressed, as `rewind` is. In
   * edit mode it goes only after the rewind's 200; one that does not go reports `refused`, which puts its words back.
   */
  const send = (pressedIn: string, sendNow: () => Promise<SendOutcome>): Promise<SendOutcome> => {
    const mode = beginReplace(pressedIn);
    if (mode.kind === 'plain') return sendNow();
    if (mode.kind === 'busy') return Promise.resolve('refused');
    return rewind(mode.turnId).then(async (): Promise<SendOutcome> => {
      finishReplace(pressedIn, { removed: true });
      const outcome = await sendNow();
      return outcome === 'not-sent' ? 'refused' : outcome;
    }, (error: unknown): SendOutcome => {
      finishReplace(pressedIn, {
        removed: false, refused: classifyFailure(error instanceof ApiError ? error.failure : null, REWIND_FAILURES) === 'refused',
        message: error instanceof Error && error.message.trim() !== '' ? error.message : 'Could not replace the message.',
      });
      return 'refused';
    });
  };
  const notice = conversationId === null ? null : editNoticeOf(conversationId);
  const cancel = () => { if (conversationId !== null) cancelEdit(conversationId); };
  return {
    /** This conversation's Edit while one is held: nothing else acts on the conversation meanwhile. */
    held,
    /** The edit bar while in edit mode, its rewind included. */
    bar: held === null || held.phase === 'replaced' ? undefined : { preview: held.refill.text, onCancel: cancel },
    /** Send's rewind is out: Send is a spinner and the composer waits. */
    replacing: held?.phase === 'replacing',
    /** The outcome of the turn shown marked (being edited) or hidden (replaced, until a read without it lands). */
    marked: held === null || held.phase === 'replaced' ? null : held.outcomeId,
    hidden: held?.phase === 'replaced' ? held.outcomeId : null,
    /** What its last Edit came to, as shown above the composer; a turn that moved on is news, not a failure. */
    notice: notice === null ? null : notice.kind === 'refused'
      ? { tone: 'error', lines: [`Edit failed: ${notice.message}`, 'Your message is still in the composer; sending adds a new one.'] } as const
      : notice.kind === 'stale' ? { tone: 'neutral', lines: ['This message can no longer be replaced; sending adds a new one.'] } as const
        : { tone: 'error', lines: ['Couldn’t reach the server. Try again.'] } as const,
    start,
    send,
  };
}
