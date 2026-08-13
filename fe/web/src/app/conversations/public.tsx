import { createContext, useCallback, useContext, useMemo, type ReactNode } from 'react';

import type { Conversation, ConversationTurn } from '../../../../core/domain/conversation.ts';
import { useState } from '../../ui/state/public.ts';

type ConversationRecord = Readonly<{
  conversation: Conversation;
  turns: readonly ConversationTurn[];
}>;

type ConversationRegistry = Readonly<{
  conversations: readonly Conversation[];
  turnsOf: (conversationId: string) => readonly ConversationTurn[];
  remember: (conversation: Conversation, turns: readonly ConversationTurn[]) => void;
}>;

const ConversationContext = createContext<ConversationRegistry | null>(null);

export function ConversationProvider({ children }: { children: ReactNode }) {
  const [records, setRecords] = useState<ReadonlyMap<string, ConversationRecord>>(() => new Map());
  const remember = useCallback((conversation: Conversation, turns: readonly ConversationTurn[]) => {
    setRecords((current) => {
      const previous = current.get(conversation.id);
      if (previous !== undefined && previous.conversation.waveId === conversation.waveId &&
        previous.conversation.waveTitle === conversation.waveTitle && previous.conversation.title === conversation.title &&
        previous.conversation.state === conversation.state && previous.conversation.updatedAt === conversation.updatedAt &&
        previous.conversation.turns === conversation.turns && previous.turns.length === turns.length &&
        previous.turns.every((turn, index) => turn === turns[index])) return current;
      const next = new Map(current);
      next.set(conversation.id, { conversation, turns });
      return next;
    });
  }, []);
  const value = useMemo<ConversationRegistry>(() => ({
    conversations: [...records.values()].map(({ conversation }) => conversation),
    turnsOf: (conversationId) => records.get(conversationId)?.turns ?? [],
    remember,
  }), [records, remember]);
  return <ConversationContext.Provider value={value}>{children}</ConversationContext.Provider>;
}

export function useConversationRegistry(): ConversationRegistry {
  const value = useContext(ConversationContext);
  if (value === null) throw new Error('useConversationRegistry() requires <ConversationProvider>.');
  return value;
}
