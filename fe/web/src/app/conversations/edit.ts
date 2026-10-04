import { composerRefillFrom, type ComposerContent, type PlannerRewind } from '../../../../core/domain/conversation-rewind.ts';
import { useConversationRegistry } from './public.tsx';

/**
 * Edit (#1923) for the open conversation: one rewind per conversation at a time, its removed
 * message added to that conversation's own composer, whichever conversation is shown by then.
 */
export function useConversationEdit({ conversationId, rewind, focusComposer }: {
  conversationId: string | null;
  /** Bound to `conversationId`'s card. */
  rewind: (turnId: string) => Promise<PlannerRewind>;
  /** Asks for the caret in that conversation's composer if it is the one shown. */
  focusComposer: (conversationId: string) => void;
}) {
  const { isEditing, tryBeginEdit, finishEdit } = useConversationRegistry();
  const run = async (turnId: string) => {
    if (conversationId === null || !tryBeginEdit(conversationId)) return;
    const target = conversationId;
    let refill: ComposerContent | null = null;
    try {
      refill = composerRefillFrom((await rewind(turnId)).input);
    } finally {
      finishEdit(target, refill);
    }
    focusComposer(target);
  };
  return {
    /** The rewind is out: this conversation's composer is read-only until it answers. */
    requesting: conversationId !== null && isEditing(conversationId),
    run,
  };
}
