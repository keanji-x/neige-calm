import { useCallback } from 'react';

import type { OptimisticConversationTurn } from '../../../../core/domain/conversation.ts';
import { useState } from '../../ui/state/public.ts';

const NO_ECHOES: readonly OptimisticConversationTurn[] = Object.freeze([]);

type Echoes = readonly OptimisticConversationTurn[];

/**
 * The open conversation's optimistic messages, keyed by card: no card's transcript holds another card's echo, not even
 * for the render between a switch and the effect that forgets the old card's. `update` writes the card it was made for.
 */
export function useCardEchoes(cardId: string) {
  const [byCard, setByCard] = useState<Readonly<Record<string, Echoes>>>({});
  const update = useCallback((next: (current: Echoes) => Echoes) => {
    setByCard((all) => {
      const before = all[cardId] ?? NO_ECHOES;
      const after = next(before);
      return after === before ? all : { ...all, [cardId]: after };
    });
  }, [cardId]);
  const forgetAll = useCallback(() => { setByCard({}); }, []);
  return [byCard[cardId] ?? NO_ECHOES, update, forgetAll] as const;
}
