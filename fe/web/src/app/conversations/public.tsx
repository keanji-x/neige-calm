import {
  createContext, useCallback, useContext, useMemo, useRef, type ReactNode,
} from 'react';

import type { SideConversation } from '../../../../core/domain/conversation.ts';
import {
  isOptimisticConversationTurn, TOO_MANY_IMAGES, type ModelSelection, type Conversation, type TranscriptEntry,
} from '../../../../core/domain/conversation.ts';
import { beginSendOp, withConfirmedSends, type RetiredSend, type SendOp } from '../../../../core/domain/conversation-outbox.ts';
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
  /** Frozen source attached to this draft and every retry of it. */
  side?: SideConversation;
  /** `/side question` sends the prepared question once; recovery thereafter is explicit. */
  autoSend?: boolean;
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

/**
 * An Edit of a conversation's latest turn (#1923): its message is in the composer and Send replaces the turn. The
 * Send is one keyed send that names the turn (#2043); beginning it ends the Edit, and the outbox holds the rest.
 */
export type ConversationEdit = Readonly<{
  /** The turn the replace names. */
  turnId: string;
  /** Its outcome entry, keyed by row: a later turn reusing `turnId` is never taken for it. */
  outcomeId: string;
  /** What the click put in the composer. */
  refill: ComposerContent;
}>;

/**
 * How this conversation's last Edit or send ended, shown above its composer until its next Edit or send: a newer turn
 * arrived first, or the server refused the send (`edit`: the replace of a turn, which is untouched); a refused send's
 * words are back in the composer (#2068).
 */
export type EditNotice = Readonly<{ kind: 'stale' } | { kind: 'refused'; message: string; edit: boolean }>;

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
  /** Each conversation's keyed sends not yet shown by the server, above every route remount. */
  outboxOf: (conversationId: string) => readonly SendOp[];
  /** Start a send there (`beginSendOp`): false while that conversation cannot take it. Clears its Edit notice, and
   * ends the Edit the send replaces the turn of. */
  beginSend: (conversationId: string, op: SendOp) => boolean;
  /** Change one conversation's outbox, whichever is shown; returns it as written. */
  editOutbox: (conversationId: string, next: (current: readonly SendOp[]) => readonly SendOp[]) => readonly SendOp[];
  /** Remove the sends server state now carries; each row that retired one stands for no other send of the outbox (#2068). */
  retireSends: (conversationId: string, retired: readonly RetiredSend[]) => void;
  /** The rows that already retired a send of that conversation's outbox. */
  spentRowsOf: (conversationId: string) => readonly string[];
  /** The next number in the tab's one read order: a read numbered later started later, whichever view started it. */
  nextRead: () => number;
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
  beginEdit: (conversationId: string, edit: ConversationEdit) => boolean;
  /** Leave edit mode and its notice; a composer still holding exactly the refill (no upload in flight) goes back to empty, one the reader changed is kept. */
  cancelEdit: (conversationId: string) => void;
  /** The edited turn is no longer the latest: edit mode ends and the composer keeps what it holds. */
  leaveEdit: (conversationId: string, outcomeId: string) => void;
  /** The server refused a send (`edit`: an Edit's replace), changing nothing: say why above that conversation's composer. */
  noteRefusedSend: (conversationId: string, message: string, edit: boolean) => void;
  editNoticeOf: (conversationId: string) => EditNotice | null;
  /** One card's image uploads, held here so a remount or another route sees an upload still in flight. */
  uploadOf: (cardId: string) => UploadState;
  editUpload: (cardId: string, next: (current: UploadState) => UploadState) => void;
  /* Deliberately no "open the planner conversation of track W" slot: the track being left is still
       mounted when a create states it, so that intent travels in the history entry instead. */
}>;

const ConversationContext = createContext<ConversationRegistry | null>(null);

const NO_SENDS: readonly SendOp[] = Object.freeze([]);
const NO_ROWS: readonly string[] = Object.freeze([]);
const NO_TURNS: readonly TranscriptEntry[] = Object.freeze([]);

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
  const outboxesRef = useRef<Readonly<Record<string, readonly SendOp[]>>>({});
  const [outboxes, setOutboxes] = useState(outboxesRef.current);
  const readsStarted = useRef(0);
  const [editNotices, setEditNotices] = useState<Readonly<Record<string, EditNotice>>>({});
  const clearEditNotice = useCallback((conversationId: string) => {
    setEditNotices((current) => {
      if (!(conversationId in current)) return current;
      const next = { ...current };
      delete next[conversationId];
      return next;
    });
  }, []);
  /* Written through the ref first, so two presses in one tick see each other. */
  const editOutbox = useCallback((conversationId: string, next: (current: readonly SendOp[]) => readonly SendOp[]) => {
    const before = outboxesRef.current[conversationId] ?? NO_SENDS;
    const after = next(before);
    if (after === before) return before;
    const updated = { ...outboxesRef.current };
    if (after.length === 0) delete updated[conversationId]; else updated[conversationId] = after;
    outboxesRef.current = updated;
    setOutboxes(updated);
    return after;
  }, []);
  const editsRef = useRef<Readonly<Record<string, ConversationEdit>>>({});
  const [edits, setEdits] = useState(editsRef.current);
  const writeEdit = useCallback((conversationId: string, edit: ConversationEdit | null) => {
    const next = { ...editsRef.current };
    if (edit === null) delete next[conversationId]; else next[conversationId] = edit;
    editsRef.current = next;
    setEdits(next);
  }, []);
  const beginSend = useCallback((conversationId: string, op: SendOp) => {
    const begun = beginSendOp(outboxesRef.current[conversationId] ?? NO_SENDS, op);
    if (begun === null) return false;
    editOutbox(conversationId, () => begun);
    clearEditNotice(conversationId);
    /* The Edit's Send is now this op, which holds the words until the server answers: no ✕ can drop them (#2041). */
    if (op.replaces !== null && editsRef.current[conversationId]?.outcomeId === op.replaces.outcomeId) writeEdit(conversationId, null);
    return true;
  }, [clearEditNotice, editOutbox, writeEdit]);
  const outboxOf = useCallback((conversationId: string) => outboxes[conversationId] ?? NO_SENDS, [outboxes]);
  const [spentRows, setSpentRows] = useState<Readonly<Record<string, readonly string[]>>>({});
  const retireSends = useCallback((conversationId: string, retired: readonly RetiredSend[]) => {
    const keys = new Set(retired.map(({ key }) => key));
    const left = editOutbox(conversationId, (current) => {
      const kept = current.filter((op) => !keys.has(op.key));
      return kept.length === current.length ? current : kept;
    });
    setSpentRows((current) => {
      const updated = { ...current };
      /* With no send left there is nothing a row could stand for twice. */
      if (left.length === 0) delete updated[conversationId];
      else updated[conversationId] = [...current[conversationId] ?? [], ...retired.flatMap(({ row }) => row === null ? [] : [row])];
      return updated;
    });
  }, [editOutbox]);
  const spentRowsOf = useCallback((conversationId: string) => spentRows[conversationId] ?? NO_ROWS, [spentRows]);
  const nextRead = useCallback(() => { readsStarted.current += 1; return readsStarted.current; }, []);
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
  const noteEdit = useCallback((conversationId: string, notice: EditNotice) => {
    setEditNotices((current) => ({ ...current, [conversationId]: notice }));
  }, []);
  const beginEdit = useCallback((conversationId: string, edit: ConversationEdit) => {
    if (conversationId in editsRef.current) return false;
    /* What the composer can take of the turn: as a pick, no more images than a message carries, and it says so (#2068). */
    const refill = withRefill(EMPTY_COMPOSER, edit.refill);
    writeEdit(conversationId, { ...edit, refill });
    editComposer(conversationId, (current) => withRefill(current, refill));
    if (refill.attachments.length < edit.refill.attachments.length) {
      editUpload(conversationId, (current) => ({ ...current, refusal: TOO_MANY_IMAGES }));
    }
    clearEditNotice(conversationId);
    return true;
  }, [clearEditNotice, editComposer, editUpload, writeEdit]);
  const cancelEdit = useCallback((conversationId: string) => {
    const edit = editsRef.current[conversationId];
    if (edit === undefined) return;
    writeEdit(conversationId, null);
    clearEditNotice(conversationId);
    /* The Edit's word that the composer took fewer images than the turn holds goes with it. */
    editUpload(conversationId, (current) => current.refusal === TOO_MANY_IMAGES ? { ...current, refusal: null } : current);
    /* An image still uploading is a change the composer does not show yet: keep everything. */
    if ((uploads[conversationId]?.inFlight ?? 0) > 0) return;
    editComposer(conversationId, (current) => isSameComposer(current, edit.refill) ? EMPTY_COMPOSER : current);
  }, [clearEditNotice, editComposer, editUpload, uploads, writeEdit]);
  const leaveEdit = useCallback((conversationId: string, outcomeId: string) => {
    const edit = editsRef.current[conversationId];
    if (edit?.outcomeId !== outcomeId) return;
    writeEdit(conversationId, null);
    noteEdit(conversationId, { kind: 'stale' });
  }, [noteEdit, writeEdit]);
  const noteRefusedSend = useCallback((conversationId: string, message: string, edit: boolean) => {
    noteEdit(conversationId, { kind: 'refused', message, edit });
  }, [noteEdit]);
  const editOf = useCallback((conversationId: string) => edits[conversationId] ?? null, [edits]);
  const editNoticeOf = useCallback((conversationId: string) => editNotices[conversationId] ?? null, [editNotices]);
  const remember = useCallback((conversation: Conversation, given: readonly TranscriptEntry[]) => {
    /* What a read showed: a message still in an outbox is the outbox's, and `turnsOf` adds it back. */
    const turns = given.some(isOptimisticConversationTurn) ? given.filter((turn) => !isOptimisticConversationTurn(turn)) : given;
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
  const turnsOf = useCallback(
    (conversationId: string) => withConfirmedSends(entries[conversationId]?.turns ?? NO_TURNS, outboxes[conversationId] ?? NO_SENDS,
      spentRows[conversationId] ?? NO_ROWS),
    [entries, outboxes, spentRows],
  );
  const value = useMemo<ConversationRegistry>(
    () => ({
      conversations, turnsOf, remember, updateExisting,
      requestedOpenId, requestedOpenFocusesComposer, requestOpen, clearOpenRequest,
      draftOf, startDraft, editDraft, adoptDraft, discardDraft, discardUnsentDraft,
      adoptedDraftIdOf, finishDraftAdoption,
      outboxOf, beginSend, editOutbox, retireSends, spentRowsOf, nextRead,
      composerOf, editComposer, newConversationComposerOf, editNewConversationComposer,
      editOf, beginEdit, cancelEdit, leaveEdit, noteRefusedSend, editNoticeOf, uploadOf, editUpload,
    }),
    [adoptDraft, adoptedDraftIdOf, clearOpenRequest, conversations, discardDraft,
      composerOf, discardUnsentDraft, draftOf, editComposer, editDraft, editNewConversationComposer, newConversationComposerOf, editUpload, finishDraftAdoption, editOf, beginEdit, cancelEdit, leaveEdit, noteRefusedSend, editNoticeOf, uploadOf, outboxOf, beginSend, editOutbox, retireSends, spentRowsOf, nextRead,
      remember, requestOpen,
      requestedOpenFocusesComposer, requestedOpenId, startDraft, turnsOf,
      updateExisting],
  );
  return <ConversationContext.Provider value={value}>{children}</ConversationContext.Provider>;
}

export function useConversationRegistry(): ConversationRegistry {
  const value = useContext(ConversationContext);
  if (!value) throw new Error('useConversationRegistry() requires <ConversationProvider> above the route outlet.');
  return value;
}
