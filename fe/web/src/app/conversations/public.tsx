import {
  createContext, useCallback, useContext, useMemo, useRef, type ReactNode,
} from 'react';

import type { ModelSelection, Conversation, OptimisticConversationTurn, TranscriptEntry } from '../../../../core/domain/conversation.ts';
import { EMPTY_COMPOSER, isSameComposer, withRefill, type ComposerContent } from '../../../../core/domain/conversation-rewind.ts';
import { NO_UPLOAD, type UploadState } from '../../features/planner/attachments.tsx';
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
  /** Whether its images came from the composer, which a delivered retry then clears (a Regenerate's never did). */
  fromComposer: boolean;
}>;

/**
 * An Edit of a conversation's latest turn (#1923). `editing`: its message is in the composer and Send replaces it;
 * `replacing`: that Send's rewind is out; `replaced`: the turn is gone and stays hidden until a read no longer shows it.
 */
export type ConversationEdit = Readonly<{
  /** The turn the rewind names. */
  turnId: string;
  /** Its outcome entry, keyed by row: a later turn reusing `turnId` is never taken for it. */
  outcomeId: string;
  /** What the click put in the composer. */
  refill: ComposerContent;
  phase: 'editing' | 'replacing' | 'replaced';
}>;

/** How a replacing Send's rewind answered. `refused`: the server said no and changed nothing; otherwise it may have landed. */
export type RewindAnswer = Readonly<{ removed: true } | { removed: false; message: string; refused: boolean }>;

/** What this conversation's last Edit came to, shown above its composer until its next Edit or send. */
export type EditNotice = Readonly<
  | { kind: 'refused'; message: string }
  | { kind: 'unreachable' }
  | { kind: 'stale' }
>;

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
  /** Each existing conversation's unsent words and images, kept across closing, switching and remounts. */
  composerOf: (conversationId: string) => ComposerContent;
  /** Change one conversation's composer, whichever conversation is shown. */
  editComposer: (conversationId: string, next: (current: ComposerContent) => ComposerContent) => void;
  /** Each Track's unsent words for a conversation not created yet, kept across closing, `+` and remounts.
   * Not `ConversationDraft.text`, which holds the words after the composer clears on send. */
  newConversationComposerOf: (scopeId: string) => string;
  editNewConversationComposer: (scopeId: string, next: (current: string) => string) => void;
  /** The Edit held for this conversation, if any; nothing else acts on the conversation meanwhile. */
  editOf: (conversationId: string) => ConversationEdit | null;
  /** Enter edit mode, one Edit per conversation; false while one is held. Puts the message in that conversation's composer. */
  beginEdit: (conversationId: string, edit: Pick<ConversationEdit, 'turnId' | 'outcomeId' | 'refill'>) => boolean;
  /** Leave edit mode and its notice; a composer still holding exactly the refill (no upload in flight) goes back to empty, one the reader changed is kept. */
  cancelEdit: (conversationId: string) => void;
  /** A Send pressed here: `replace` names the turn its rewind removes; `busy` while that rewind is out; `plain` outside edit mode. */
  beginReplace: (conversationId: string) => Readonly<{ kind: 'plain' } | { kind: 'busy' } | { kind: 'replace'; turnId: string }>;
  /** The replacing rewind answered: removed, refused (edit mode ends) or unknown (still editing, Send tries again). */
  finishReplace: (conversationId: string, answer: RewindAnswer) => void;
  /** The edited turn is no longer the latest: edit mode ends and the composer keeps what it holds. */
  leaveEdit: (conversationId: string, outcomeId: string) => void;
  /** A transcript read without the replaced turn landed: nothing is hidden or withheld any more. */
  forgetEdit: (conversationId: string, outcomeId: string) => void;
  editNoticeOf: (conversationId: string) => EditNotice | null;
  /** One card's image uploads, held here so a remount or another route sees an upload still in flight. */
  uploadOf: (cardId: string) => UploadState;
  editUpload: (cardId: string, next: (current: UploadState) => UploadState) => void;
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
  const [editNotices, setEditNotices] = useState<Readonly<Record<string, EditNotice>>>({});
  const clearEditNotice = useCallback((conversationId: string) => {
    setEditNotices((current) => {
      if (!(conversationId in current)) return current;
      const next = { ...current };
      delete next[conversationId];
      return next;
    });
  }, []);
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
    clearEditNotice(conversationId);
    return true;
  }, [clearEditNotice]);
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
  const [composers, setComposers] = useState<Readonly<Record<string, ComposerContent>>>({});
  const editComposer = useCallback((conversationId: string, next: (current: ComposerContent) => ComposerContent) => {
    setComposers((current) => {
      const before = current[conversationId] ?? EMPTY_COMPOSER;
      const after = next(before);
      if (after === before) return current;
      const updated = { ...current };
      /* An empty composer is no entry, so the map holds only conversations with something unsent. */
      if (after.text === '' && after.attachments.length === 0) delete updated[conversationId]; else updated[conversationId] = after;
      return updated;
    });
  }, []);
  const composerOf = useCallback((conversationId: string) => composers[conversationId] ?? EMPTY_COMPOSER, [composers]);
  const [newConversationComposers, setNewConversationComposers] = useState<Readonly<Record<string, string>>>({});
  const editNewConversationComposer = useCallback((scopeId: string, next: (current: string) => string) => {
    setNewConversationComposers((current) => {
      const before = current[scopeId] ?? '';
      const after = next(before);
      if (after === before) return current;
      const updated = { ...current };
      /* Empty is no entry, as for an existing conversation's composer. */
      if (after === '') delete updated[scopeId]; else updated[scopeId] = after;
      return updated;
    });
  }, []);
  const newConversationComposerOf = useCallback((scopeId: string) => newConversationComposers[scopeId] ?? '', [newConversationComposers]);
  const [uploads, setUploads] = useState<Readonly<Record<string, UploadState>>>({});
  const editUpload = useCallback((cardId: string, next: (current: UploadState) => UploadState) => {
    setUploads((current) => {
      const after = next(current[cardId] ?? NO_UPLOAD);
      const updated = { ...current };
      if (after.inFlight === 0 && after.refusal === null) delete updated[cardId]; else updated[cardId] = after;
      return updated;
    });
  }, []);
  const uploadOf = useCallback((cardId: string) => uploads[cardId] ?? NO_UPLOAD, [uploads]);
  const editsRef = useRef<Readonly<Record<string, ConversationEdit>>>({});
  const [edits, setEdits] = useState(editsRef.current);
  const writeEdit = useCallback((conversationId: string, edit: ConversationEdit | null) => {
    const next = { ...editsRef.current };
    if (edit === null) delete next[conversationId]; else next[conversationId] = edit;
    editsRef.current = next;
    setEdits(next);
  }, []);
  const noteEdit = useCallback((conversationId: string, notice: EditNotice) => {
    setEditNotices((current) => ({ ...current, [conversationId]: notice }));
  }, []);
  const beginEdit = useCallback((conversationId: string, edit: Pick<ConversationEdit, 'turnId' | 'outcomeId' | 'refill'>) => {
    if (conversationId in editsRef.current) return false;
    writeEdit(conversationId, { ...edit, phase: 'editing' });
    editComposer(conversationId, (current) => withRefill(current, edit.refill));
    clearEditNotice(conversationId);
    return true;
  }, [clearEditNotice, editComposer, writeEdit]);
  const cancelEdit = useCallback((conversationId: string) => {
    const edit = editsRef.current[conversationId];
    if (edit?.phase !== 'editing') return;
    writeEdit(conversationId, null);
    clearEditNotice(conversationId);
    /* An image still uploading is a change the composer does not show yet: keep everything. */
    if ((uploads[conversationId]?.inFlight ?? 0) > 0) return;
    editComposer(conversationId, (current) => isSameComposer(current, edit.refill) ? EMPTY_COMPOSER : current);
  }, [clearEditNotice, editComposer, uploads, writeEdit]);
  const beginReplace = useCallback((conversationId: string) => {
    const edit = editsRef.current[conversationId];
    if (edit === undefined || edit.phase === 'replaced') return { kind: 'plain' } as const;
    if (edit.phase === 'replacing') return { kind: 'busy' } as const;
    writeEdit(conversationId, { ...edit, phase: 'replacing' });
    clearEditNotice(conversationId);
    return { kind: 'replace', turnId: edit.turnId } as const;
  }, [clearEditNotice, writeEdit]);
  const finishReplace = useCallback((conversationId: string, answer: RewindAnswer) => {
    const edit = editsRef.current[conversationId];
    if (edit?.phase !== 'replacing') return;
    if (answer.removed) { writeEdit(conversationId, { ...edit, phase: 'replaced' }); return; }
    /* An answer the server did not give may have landed: stay in edit mode, so Send asks again. */
    writeEdit(conversationId, answer.refused ? null : { ...edit, phase: 'editing' });
    noteEdit(conversationId, answer.refused ? { kind: 'refused', message: answer.message } : { kind: 'unreachable' });
  }, [noteEdit, writeEdit]);
  const leaveEdit = useCallback((conversationId: string, outcomeId: string) => {
    const edit = editsRef.current[conversationId];
    if (edit?.phase !== 'editing' || edit.outcomeId !== outcomeId) return;
    writeEdit(conversationId, null);
    noteEdit(conversationId, { kind: 'stale' });
  }, [noteEdit, writeEdit]);
  const forgetEdit = useCallback((conversationId: string, outcomeId: string) => {
    const edit = editsRef.current[conversationId];
    if (edit?.phase === 'replaced' && edit.outcomeId === outcomeId) writeEdit(conversationId, null);
  }, [writeEdit]);
  const editOf = useCallback((conversationId: string) => edits[conversationId] ?? null, [edits]);
  const editNoticeOf = useCallback((conversationId: string) => editNotices[conversationId] ?? null, [editNotices]);
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
      composerOf, editComposer, newConversationComposerOf, editNewConversationComposer,
      editOf, beginEdit, cancelEdit, beginReplace, finishReplace, leaveEdit, forgetEdit, editNoticeOf, uploadOf, editUpload,
    }),
    [adoptDraft, adoptedDraftIdOf, clearOpenRequest, conversations, discardDraft,
      composerOf, discardUnsentDraft, draftOf, editComposer, editDraft, editNewConversationComposer, newConversationComposerOf, editUpload, finishDraftAdoption, forgetEdit, editOf, beginEdit, cancelEdit, beginReplace, finishReplace, leaveEdit, editNoticeOf, uploadOf, finishSend, pendingSendIds,
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
