import {
  createContext, useCallback, useContext, useMemo, type ReactNode,
} from 'react';

import type { Conversation, TranscriptEntry } from '../../../../core/domain/conversation.ts';
import { useState } from '../../ui/state/public.ts';

type RememberedConversation = Readonly<{
  conversation: Conversation;
  turns: readonly TranscriptEntry[];
}>;

export type ConversationRegistry = Readonly<{
  conversations: readonly Conversation[];
  turnsOf: (conversationId: string) => readonly TranscriptEntry[];
  remember: (conversation: Conversation, turns: readonly TranscriptEntry[]) => void;
  /* There is no `forget`. The only caller it ever had was the conversation
     reset, which is gone from the product (#1139) — a registry entry now
     leaves exactly one way, by the tab ending. Re-adding a removal door needs
     a caller that has a reason to slam it, not a symmetry argument. */
  requestedOpenId: string | null;
  requestOpen: (conversationId: string) => void;
  clearOpenRequest: () => void;
}>;

const ConversationContext = createContext<ConversationRegistry | null>(null);

function equalRecord(left: Readonly<Record<string, unknown>>, right: Readonly<Record<string, unknown>>): boolean {
  const keys = Object.keys(left);
  return keys.length === Object.keys(right).length && keys.every((key) => left[key] === right[key]);
}

function equalTurns(left: readonly TranscriptEntry[], right: readonly TranscriptEntry[]): boolean {
  return left.length === right.length && left.every((turn, index) => equalRecord(turn, right[index]));
}

function equalEntry(left: RememberedConversation | undefined, conversation: Conversation, turns: readonly TranscriptEntry[]): boolean {
  return left !== undefined && equalRecord(left.conversation, conversation) && equalTurns(left.turns, turns);
}

export function ConversationProvider({ children }: { children: ReactNode }) {
  const [entries, setEntries] = useState<Readonly<Record<string, RememberedConversation>>>({});
  const [requestedOpenId, setRequestedOpenId] = useState<string | null>(null);
  const remember = useCallback((conversation: Conversation, turns: readonly TranscriptEntry[]) => {
    setEntries((current) => equalEntry(current[conversation.id], conversation, turns)
      ? current
      : { ...current, [conversation.id]: { conversation, turns } });
  }, []);
  const requestOpen = useCallback((conversationId: string) => setRequestedOpenId(conversationId), []);
  const clearOpenRequest = useCallback(() => setRequestedOpenId(null), []);
  const conversations = useMemo(() => Object.values(entries).map(({ conversation }) => conversation), [entries]);
  const turnsOf = useCallback((conversationId: string) => entries[conversationId]?.turns ?? [], [entries]);
  const value = useMemo<ConversationRegistry>(
    () => ({
      conversations, turnsOf, remember, requestedOpenId, requestOpen, clearOpenRequest,
    }),
    [clearOpenRequest, conversations, remember, requestOpen, requestedOpenId, turnsOf],
  );
  return <ConversationContext.Provider value={value}>{children}</ConversationContext.Provider>;
}

export function useConversationRegistry(): ConversationRegistry {
  const value = useContext(ConversationContext);
  if (!value) throw new Error('useConversationRegistry() requires <ConversationProvider> above the route outlet.');
  return value;
}
