import { useEffect, useRef } from 'react';

import type { PlannerAttachment } from '../../../../core/api/generated/wire.ts';
import type { SendOutcome } from '../../../../core/domain/conversation.ts';
import { composerRefillFrom, type ComposerRefill, type PlannerRewind } from '../../../../core/domain/conversation-rewind.ts';
import type { PlannerAttachments } from '../../features/planner/attachments.tsx';
import { useConversationRegistry } from './public.tsx';

const sameImages = (left: readonly PlannerAttachment[], right: readonly PlannerAttachment[]) =>
  left.length === right.length && left.every((image, index) => image.id === right[index].id);

/**
 * Edit (#1923) for the open conversation. The server has already removed the turn, so its input is
 * owned by the registry, per conversation, until it is sent or the reader empties it: while shown it
 * follows the composer, on leaving the composer is cleared and the record stays, and it comes back
 * the next time that conversation's composer is shown empty.
 */
export function useConversationEdit({ conversationId, rewind, draft, setDraft, attachments, focusComposer }: {
  conversationId: string | null;
  /** Bound to `conversationId`'s card. */
  rewind: (turnId: string) => Promise<PlannerRewind>;
  draft: string;
  setDraft: (text: string) => void;
  attachments: Pick<PlannerAttachments, 'items' | 'busy' | 'restore'>;
  focusComposer: () => void;
}) {
  const { editOf, tryBeginEdit, finishEdit, holdRefill } = useConversationRegistry();
  const edit = conversationId === null ? null : editOf(conversationId);
  const refill = edit?.kind === 'ready' ? edit.refill : null;
  const { items, busy, restore } = attachments;
  const composerEmpty = draft.trim() === '' && items.length === 0 && !busy;
  /** The conversation whose refill this composer shows; unsettled until the composer has caught up with it. */
  const shown = useRef<{ id: string; settled: boolean } | null>(null);
  /** A send is out: the field it cleared is not the reader emptying it. */
  const sending = useRef(false);
  useEffect(() => {
    const held = shown.current;
    if (held !== null && held.id !== conversationId) {
      /* The shared draft must never carry a removed prompt into another conversation. */
      shown.current = null;
      setDraft('');
      return;
    }
    if (conversationId === null || refill === null) { shown.current = null; return; }
    if (held === null) {
      /* Never merged into words or images already in the composer: it waits until that is empty. */
      if (!composerEmpty) return;
      shown.current = { id: conversationId, settled: false };
      setDraft(refill.text);
      restore(refill.attachments);
      focusComposer();
      return;
    }
    if (!held.settled) {
      held.settled = draft === refill.text && sameImages(items, refill.attachments);
      return;
    }
    if (sending.current) return;
    if (composerEmpty) {
      shown.current = null;
      holdRefill(conversationId, null);
      return;
    }
    if (draft !== refill.text || !sameImages(items, refill.attachments)) {
      holdRefill(conversationId, { text: draft, attachments: items });
    }
  }, [composerEmpty, conversationId, draft, focusComposer, holdRefill, items, refill, restore, setDraft]);
  const run = async (turnId: string) => {
    if (conversationId === null || !tryBeginEdit(conversationId)) return;
    let taken: ComposerRefill | null = null;
    try {
      taken = composerRefillFrom((await rewind(turnId)).input);
    } finally {
      finishEdit(conversationId, taken);
    }
  };
  /** Until a send's outcome is known its cleared field is not the reader's doing; a delivered send retires the refill. */
  const send = (sentFrom: string, deliver: () => Promise<SendOutcome>): Promise<SendOutcome> => {
    sending.current = true;
    return deliver().then((outcome) => {
      if (outcome === 'delivered') holdRefill(sentFrom, null);
      return outcome;
    }).finally(() => { sending.current = false; });
  };
  return {
    /** The rewind is out: the composer is read-only until it answers. */
    requesting: edit?.kind === 'requesting',
    /** Offered only on an empty composer with no refill held; kept while the request is out so its answer has a view to land on. */
    run: composerEmpty && edit?.kind !== 'ready' ? run : undefined,
    /** No other write may race the rewind or overtake its refill. */
    idle: edit === null,
    send,
  };
}
