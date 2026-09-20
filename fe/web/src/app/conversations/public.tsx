import {
  createContext, useCallback, useContext, useMemo, useRef, type ReactNode,
} from 'react';

import type { ModelSelection, Conversation, OptimisticConversationTurn, TranscriptEntry } from '../../../../core/domain/conversation.ts';
import { useReducer, useState } from '../../ui/state/public.ts';

/**
 * A conversation being written. The key belongs to the draft for its whole lifetime: a failed create
 * keeps `key` and `sentText`, since retrying an ambiguous request under a new key would mint a second conversation.
 */
export type ConversationDraft = Readonly<{
  /** The Track this draft belongs to. */
  scopeId: string;
  /** Chosen before sending and locked while delivery is unconfirmed. */
  model: ModelSelection;
  /** Identifies the draft to the server; minted once, never once per send. */
  key: string;
  /** The words the drawer is holding after its composer clears on send. */
  text: string | null;
  /** The words actually posted under `key`, or null before any POST. */
  sentText: string | null;
  /** The create/recovery chain for this key has not settled yet. */
  creating: boolean;
  error: string | null;
  remedy: 'retry' | 'new-conversation' | null;
}>;

export type ConversationDraftId = Readonly<Pick<ConversationDraft, 'scopeId' | 'key'>>;

/** One slot per Track: unfinished draft work, or the row that work became — mutually exclusive by construction. */
type DraftSlot = Readonly<
  | { kind: 'held'; draft: ConversationDraft }
  | { kind: 'adopted'; conversationId: string }
>;

type DraftSlots = Readonly<Record<string, DraftSlot>>;

type DraftMove = Readonly<
  | { kind: 'start'; draft: ConversationDraft }
  | { kind: 'edit'; from: ConversationDraftId; next: (current: ConversationDraft) => ConversationDraft }
  | { kind: 'adopt'; from: ConversationDraftId; conversationId: string }
  | { kind: 'discard'; from: ConversationDraftId }
  | { kind: 'discard-unsent'; scopeId: string }
  | { kind: 'finish-adoption'; scopeId: string; conversationId: string }
>;

const slotHolds = (slot: DraftSlot | undefined, id: ConversationDraftId): slot is Extract<DraftSlot, { kind: 'held' }> =>
  slot?.kind === 'held' && slot.draft.scopeId === id.scopeId && slot.draft.key === id.key;

function withoutSlot(slots: DraftSlots, scopeId: string): DraftSlots {
  const next = { ...slots };
  delete next[scopeId];
  return next;
}

function moveDraft(slots: DraftSlots, move: DraftMove): DraftSlots {
  switch (move.kind) {
    case 'start':
      return { ...slots, [move.draft.scopeId]: { kind: 'held', draft: move.draft } };
    case 'edit': {
      const slot = slots[move.from.scopeId];
      return slotHolds(slot, move.from)
        ? { ...slots, [move.from.scopeId]: { kind: 'held', draft: move.next(slot.draft) } }
        : slots;
    }
    case 'adopt': {
      const slot = slots[move.from.scopeId];
      return slotHolds(slot, move.from)
        ? { ...slots, [move.from.scopeId]: { kind: 'adopted', conversationId: move.conversationId } }
        : slots;
    }
    case 'discard':
      return slotHolds(slots[move.from.scopeId], move.from)
        ? withoutSlot(slots, move.from.scopeId)
        : slots;
    case 'discard-unsent': {
      const slot = slots[move.scopeId];
      return slot?.kind === 'held' && slot.draft.sentText === null
        ? withoutSlot(slots, move.scopeId)
        : slots;
    }
    case 'finish-adoption': {
      const slot = slots[move.scopeId];
      return slot?.kind === 'adopted' && slot.conversationId === move.conversationId
        ? withoutSlot(slots, move.scopeId)
        : slots;
    }
  }
}

/** A failed request keeps its words and delivery witness across drawer remounts.
 * It is recovery work, never a confirmed transcript or conversation title. */
export type FailedConversationSend = Readonly<{
  echo: OptimisticConversationTurn;
  message: string;
  delivery: 'rejected' | 'unknown' | 'refused';
}>;

export type RememberedConversation = Readonly<{
  conversation: Conversation;
  turns: readonly TranscriptEntry[];
}>;

export type ConversationRegistry = Readonly<{
  conversations: readonly Conversation[];
  turnsOf: (conversationId: string) => readonly TranscriptEntry[];
  remember: (conversation: Conversation, turns: readonly TranscriptEntry[]) => void;
  /**
   * Amend an entry that already exists, reading it inside the state updater rather than through the
   * caller's snapshot (an async writer's captured render can be stale by the time it lands). `amend`
   * must be pure: React may call it more than once. Existing only, so it creates nothing `rememberOn` declined.
   */
  updateExisting: (
    conversationId: string, amend: (entry: RememberedConversation) => RememberedConversation,
  ) => void;
  requestedOpenId: string | null;
  /** Carried beside the id: only the open a just-created track makes wants the caret. */
  requestedOpenFocusesComposer: boolean;
  requestOpen: (conversationId: string, options?: { focusComposer?: boolean }) => void;
  clearOpenRequest: () => void;
  /** Failed first-message attempts live here, above every route remount. */
  draftOf: (scopeId: string) => ConversationDraft | null;
  startDraft: (draft: ConversationDraft) => void;
  editDraft: (
    from: ConversationDraftId, next: (current: ConversationDraft) => ConversationDraft,
  ) => void;
  /** Atomically retire `from` and leave its resulting row for that route to open. */
  adoptDraft: (from: ConversationDraftId, conversationId: string) => void;
  discardDraft: (from: ConversationDraftId) => void;
  discardUnsentDraft: (scopeId: string) => void;
  adoptedDraftIdOf: (scopeId: string) => string | null;
  finishDraftAdoption: (scopeId: string, conversationId: string) => void;
  /** One in-flight send per conversation across route/store remounts. */
  pendingSendIds: ReadonlySet<string>;
  /** Failed attempts keyed by the conversation that owns their recovery. */
  failedSends: Readonly<Record<string, FailedConversationSend>>;
  tryBeginSend: (conversationId: string) => boolean;
  finishSend: (conversationId: string, failure: FailedConversationSend | null) => void;
  clearFailedSend: (conversationId: string, echoId: string) => void;
  /* Deliberately no "open the planner conversation of track W" slot: the track being left is still
       mounted when a create states it, so that intent travels in the history entry instead. */
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
  /* Keyed by Track because leaving one Track may legitimately start another draft before the first failure is retried. */
  const [draftSlots, moveDraftTo] = useReducer(moveDraft, {} as DraftSlots);
  const [openRequest, setOpenRequest] = useState<
    { id: string; focusComposer: boolean } | null
  >(null);
  const pendingSendIdsRef = useRef<ReadonlySet<string>>(new Set());
  const [pendingSendIds, setPendingSendIds] = useState<ReadonlySet<string>>(() => new Set());
  const [failedSends, setFailedSends] = useState<Readonly<Record<string, FailedConversationSend>>>({});
  const tryBeginSend = useCallback((conversationId: string) => {
    if (pendingSendIdsRef.current.has(conversationId)) return false;
    const next = new Set(pendingSendIdsRef.current);
    next.add(conversationId);
    pendingSendIdsRef.current = next;
    setPendingSendIds(next);
    setFailedSends((current) => {
      if (!(conversationId in current)) return current;
      const withoutPrevious = { ...current };
      delete withoutPrevious[conversationId];
      return withoutPrevious;
    });
    return true;
  }, []);
  const finishSend = useCallback((conversationId: string, failure: FailedConversationSend | null) => {
    if (!pendingSendIdsRef.current.has(conversationId)) return;
    const next = new Set(pendingSendIdsRef.current);
    next.delete(conversationId);
    pendingSendIdsRef.current = next;
    setPendingSendIds(next);
    if (failure !== null) {
      setFailedSends((current) => ({ ...current, [conversationId]: failure }));
    }
  }, []);
  const clearFailedSend = useCallback((conversationId: string, echoId: string) => {
    setFailedSends((current) => {
      if (current[conversationId]?.echo.id !== echoId) return current;
      const next = { ...current };
      delete next[conversationId];
      return next;
    });
  }, []);
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
  const draftOf = useCallback((scopeId: string) => {
    const slot = draftSlots[scopeId];
    return slot?.kind === 'held' ? slot.draft : null;
  }, [draftSlots]);
  const startDraft = useCallback((draft: ConversationDraft) => {
    moveDraftTo({ kind: 'start', draft });
  }, []);
  const editDraft = useCallback((
    from: ConversationDraftId, next: (current: ConversationDraft) => ConversationDraft,
  ) => {
    moveDraftTo({ kind: 'edit', from, next });
  }, []);
  const adoptDraft = useCallback((from: ConversationDraftId, conversationId: string) => {
    moveDraftTo({ kind: 'adopt', from, conversationId });
  }, []);
  const discardDraft = useCallback((from: ConversationDraftId) => {
    moveDraftTo({ kind: 'discard', from });
  }, []);
  const discardUnsentDraft = useCallback((scopeId: string) => {
    moveDraftTo({ kind: 'discard-unsent', scopeId });
  }, []);
  const adoptedDraftIdOf = useCallback((scopeId: string) => {
    const slot = draftSlots[scopeId];
    return slot?.kind === 'adopted' ? slot.conversationId : null;
  }, [draftSlots]);
  const finishDraftAdoption = useCallback((scopeId: string, conversationId: string) => {
    moveDraftTo({ kind: 'finish-adoption', scopeId, conversationId });
  }, []);
  const requestedOpenId = openRequest?.id ?? null;
  const requestedOpenFocusesComposer = openRequest?.focusComposer ?? false;
  const conversations = useMemo(() => Object.values(entries).map(({ conversation }) => conversation), [entries]);
  const turnsOf = useCallback((conversationId: string) => entries[conversationId]?.turns ?? [], [entries]);
  const value = useMemo<ConversationRegistry>(
    () => ({
      conversations, turnsOf, remember, updateExisting,
      requestedOpenId, requestedOpenFocusesComposer, requestOpen, clearOpenRequest,
      draftOf, startDraft, editDraft, adoptDraft, discardDraft, discardUnsentDraft,
      adoptedDraftIdOf, finishDraftAdoption,
      pendingSendIds, failedSends, tryBeginSend, finishSend, clearFailedSend,
    }),
    [adoptDraft, adoptedDraftIdOf, clearOpenRequest, conversations, discardDraft,
      discardUnsentDraft, draftOf, editDraft, finishDraftAdoption, finishSend, pendingSendIds,
      remember, requestOpen, failedSends, clearFailedSend,
      requestedOpenFocusesComposer, requestedOpenId, startDraft, tryBeginSend, turnsOf,
      updateExisting],
  );
  return <ConversationContext.Provider value={value}>{children}</ConversationContext.Provider>;
}

export function useConversationRegistry(): ConversationRegistry {
  const value = useContext(ConversationContext);
  if (!value) throw new Error('useConversationRegistry() requires <ConversationProvider> above the route outlet.');
  return value;
}
