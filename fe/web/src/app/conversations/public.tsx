import {
  createContext, useCallback, useContext, useMemo, type ReactNode,
} from 'react';

import type { Conversation, TranscriptEntry } from '../../../../core/domain/conversation.ts';
import { useState } from '../../ui/state/public.ts';

export type RememberedConversation = Readonly<{
  conversation: Conversation;
  turns: readonly TranscriptEntry[];
}>;

export type ConversationRegistry = Readonly<{
  conversations: readonly Conversation[];
  turnsOf: (conversationId: string) => readonly TranscriptEntry[];
  remember: (conversation: Conversation, turns: readonly TranscriptEntry[]) => void;
  /**
   * Amend an entry **that already exists**, reading it at the moment of the
   * write rather than at the moment the caller decided to write.
   *
   * `conversations` and `turnsOf` are values off a render. An async writer —
   * `useConversationStore`'s send, whose promise settles two refreshes after
   * its POST returned 200 — closes over the render it started on, and by the
   * time it lands one of those refreshes may already have put a newer
   * transcript, turn count or live state into this entry. Reading through the
   * captured value and writing the merge back is a read-modify-write across an
   * await: it silently reverts whatever arrived in between.
   *
   * So the merge is handed *in*, and runs inside the state updater against
   * whatever the entry holds then. `amend` must therefore be a pure function of
   * its argument — React may call it more than once for one write.
   *
   * "Existing only" is the second half, and it is the reason this is not
   * `remember`: it is what keeps a write-through on the right side of the
   * `rememberOn` defence (`app/router/public.tsx`) — a conversation this tab
   * decided not to remember has no entry, so an amendment finds nothing and
   * creates nothing. That check has to happen *here*, at the write, for the
   * same reason the merge does: the caller's snapshot of "does an entry exist"
   * can be as stale as its snapshot of the entry.
   */
  updateExisting: (
    conversationId: string, amend: (entry: RememberedConversation) => RememberedConversation,
  ) => void;
  /* There is no `forget`. The only caller it ever had was the conversation
     reset, which is gone from the product (#1139) — a registry entry now
     leaves exactly one way, by the tab ending. Re-adding a removal door needs
     a caller that has a reason to slam it, not a symmetry argument. */
  requestedOpenId: string | null;
  /**
   * True while the pending open should also put the caret in the composer.
   *
   * Carried beside the id rather than folded into it because the two questions
   * have different answers: every open request names a conversation, and only
   * the one a just-created track makes wants the caret — a reader who opened a
   * row from Today asked to *read* it.
   */
  requestedOpenFocusesComposer: boolean;
  requestOpen: (conversationId: string, options?: { focusComposer?: boolean }) => void;
  clearOpenRequest: () => void;
  /* There is deliberately no "open the spec conversation of track W" slot here
     (#1211 S2). It was one, and a global slot cannot own that intent: the track
     the reader is leaving is still mounted when a create states it, and every
     track route body can read and clear a slot addressed to a different one.
     The intent now travels in the history entry the create navigates to —
     `app/router/navigation.ts`, `useSpecOpenIntent` — and reaches this
     registry only as the ordinary `requestOpen` the target route issues once
     it knows its own spec card. */
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
  const [openRequest, setOpenRequest] = useState<
    { id: string; focusComposer: boolean } | null
  >(null);
  const remember = useCallback((conversation: Conversation, turns: readonly TranscriptEntry[]) => {
    setEntries((current) => equalEntry(current[conversation.id], conversation, turns)
      ? current
      : { ...current, [conversation.id]: { conversation, turns } });
  }, []);
  const updateExisting = useCallback((
    conversationId: string, amend: (entry: RememberedConversation) => RememberedConversation,
  ) => {
    setEntries((current) => {
      const entry = current[conversationId];
      if (entry === undefined) return current;
      const next = amend(entry);
      return equalEntry(entry, next.conversation, next.turns)
        ? current
        : { ...current, [conversationId]: next };
    });
  }, []);
  const requestOpen = useCallback(
    (conversationId: string, options?: { focusComposer?: boolean }) =>
      setOpenRequest({ id: conversationId, focusComposer: options?.focusComposer ?? false }),
    [],
  );
  const clearOpenRequest = useCallback(() => setOpenRequest(null), []);
  const requestedOpenId = openRequest?.id ?? null;
  const requestedOpenFocusesComposer = openRequest?.focusComposer ?? false;
  const conversations = useMemo(() => Object.values(entries).map(({ conversation }) => conversation), [entries]);
  const turnsOf = useCallback((conversationId: string) => entries[conversationId]?.turns ?? [], [entries]);
  const value = useMemo<ConversationRegistry>(
    () => ({
      conversations, turnsOf, remember, updateExisting,
      requestedOpenId, requestedOpenFocusesComposer, requestOpen, clearOpenRequest,
    }),
    [clearOpenRequest, conversations, remember, requestOpen,
      requestedOpenFocusesComposer, requestedOpenId, turnsOf, updateExisting],
  );
  return <ConversationContext.Provider value={value}>{children}</ConversationContext.Provider>;
}

export function useConversationRegistry(): ConversationRegistry {
  const value = useContext(ConversationContext);
  if (!value) throw new Error('useConversationRegistry() requires <ConversationProvider> above the route outlet.');
  return value;
}
