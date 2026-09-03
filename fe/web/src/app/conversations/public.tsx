import {
  createContext, useCallback, useContext, useMemo, type ReactNode,
} from 'react';

import type { Conversation, TranscriptEntry } from '../../../../core/domain/conversation.ts';
import { useReducer, useState } from '../../ui/state/public.ts';

/**
 * A conversation being written: one value, not five pieces of state.
 *
 * The key belongs to this draft for its whole lifetime. In particular, a
 * failed create keeps both `key` and `sentText` while its route is unmounted:
 * the server may have committed an ambiguous request, and retrying it with a
 * new key would mint a second conversation.
 */
export type ConversationDraft = Readonly<{
  /** The Track this draft belongs to. */
  scopeId: string;
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

/**
 * The provider owns one independent slot per Track. A slot is either unfinished
 * draft work or the row that work became; making it a union keeps those two
 * outcomes mutually exclusive in the same reducer transition.
 */
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
  /* There is deliberately no "open the planner conversation of track W" slot here
     (#1211 S2). It was one, and a global slot cannot own that intent: the track
     the reader is leaving is still mounted when a create states it, and every
     track route body can read and clear a slot addressed to a different one.
     The intent now travels in the history entry the create navigates to —
     `app/router/navigation.ts`, `usePlannerOpenIntent` — and reaches this
     registry only as the ordinary `requestOpen` the target route issues once
     it knows its own planner card. */
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
  /* Drafts are keyed by Track because leaving one Track may legitimately start
     another draft before the first failure is retried. This provider is above
     the route outlet, so both slots survive those route component lifetimes. */
  const [draftSlots, moveDraftTo] = useReducer(moveDraft, {} as DraftSlots);
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
    }),
    [adoptDraft, adoptedDraftIdOf, clearOpenRequest, conversations, discardDraft,
      discardUnsentDraft, draftOf, editDraft, finishDraftAdoption, remember, requestOpen,
      requestedOpenFocusesComposer, requestedOpenId, startDraft, turnsOf, updateExisting],
  );
  return <ConversationContext.Provider value={value}>{children}</ConversationContext.Provider>;
}

export function useConversationRegistry(): ConversationRegistry {
  const value = useContext(ConversationContext);
  if (!value) throw new Error('useConversationRegistry() requires <ConversationProvider> above the route outlet.');
  return value;
}
