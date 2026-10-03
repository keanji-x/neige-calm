import { useEffect } from 'react';

import { composerRefillFrom, type ComposerRefill, type PlannerRewind } from '../../../../core/domain/conversation-rewind.ts';
import type { PlannerAttachments } from '../../features/planner/attachments.tsx';
import { useConversationRegistry } from './public.tsx';

/**
 * Edit (#1923) for the open conversation. One rewind per conversation at a time; its refill waits
 * in the registry, so a switch or remount mid-request cannot drop it or hand it to another
 * conversation, and lands in this conversation's composer only while that holds nothing.
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
  const registry = useConversationRegistry();
  const { editOf, tryBeginEdit, finishEdit, takeRefill } = registry;
  const edit = conversationId === null ? null : editOf(conversationId);
  const refill = edit?.kind === 'ready' ? edit.refill : null;
  /* Nothing is ever merged into, or discarded from, a composer that holds words or images. */
  const composerEmpty = draft.trim() === '' && attachments.items.length === 0 && !attachments.busy;
  const { restore } = attachments;
  useEffect(() => {
    if (conversationId === null || refill === null || !composerEmpty) return;
    setDraft(refill.text);
    restore(refill.attachments);
    focusComposer();
    takeRefill(conversationId, refill);
  }, [composerEmpty, conversationId, focusComposer, refill, restore, setDraft, takeRefill]);
  const run = async (turnId: string) => {
    if (conversationId === null || !tryBeginEdit(conversationId)) return;
    let taken: ComposerRefill | null = null;
    try {
      taken = composerRefillFrom((await rewind(turnId)).input);
    } finally {
      finishEdit(conversationId, taken);
    }
  };
  return {
    /** The rewind is out: the composer is read-only until it answers. */
    requesting: edit?.kind === 'requesting',
    /** Offered only on an empty composer with no refill waiting; kept while the request is out so its answer has a view to land on. */
    run: composerEmpty && edit?.kind !== 'ready' ? run : undefined,
    /** No other write may race the rewind or overtake its refill. */
    idle: edit === null,
  };
}
