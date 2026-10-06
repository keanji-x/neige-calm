import {
  createContext, useCallback, useContext, useMemo, useRef, type ReactNode,
} from 'react';

import type { SideConversation } from '../../../../core/domain/conversation.ts';
import {
  isOptimisticConversationTurn, TOO_MANY_IMAGES, type ModelSelection, type Conversation, type TranscriptEntry,
} from '../../../../core/domain/conversation.ts';
import { beginSendOp, withConfirmedSends, type RetiredSend, type SendOp } from '../../../../core/domain/conversation-outbox.ts';
import {
  EMPTY_COMPOSER, isSameComposer, tookEveryImage, withRefill, type ComposerContent,
} from '../../../../core/domain/conversation-composer.ts';
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
 * words are back in the composer (#2068). `dormant`: refused because the session cannot be resumed, so a fresh session
 * is offered; `restarted`: that fresh session started (#2192).
 */
export type EditNotice = Readonly<
  | { kind: 'stale' } | { kind: 'refused'; message: string; edit: boolean } | { kind: 'dormant'; edit: boolean }
  | { kind: 'restarted' }
>;

/** A refused send's notice, by the class its failure was read as. */
export type RefusedSendNotice = Extract<EditNotice, { kind: 'refused' | 'dormant' }>;

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
  /** Change one conversation's composer, whichever conversation is shown; returns it as written. */
  editComposer: (conversationId: string, next: (current: ComposerContent) => ComposerContent) => ComposerContent;
  /** Give words and images back to one conversation's composer, added to what it holds (`withRefill`); the images the
   * cap left out are said there, whatever the composer already held (#2068). */
  refillComposer: (conversationId: string, refill: ComposerContent) => void;
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
  noteRefusedSend: (conversationId: string, notice: RefusedSendNotice) => void;
  /** A fresh session started for that conversation (#2192): its notice replaces whatever the strip said. */
  noteRestarted: (conversationId: string) => void;
  editNoticeOf: (conversationId: string) => EditNotice | null;
  /** One card's image uploads, held here so a remount or another route sees an upload still in flight. */
  uploadOf: (cardId: string) => UploadState;
  editUpload: (cardId: string, next: (current: UploadState) => UploadState) => void;
  /** Whether a write to one card's queued entries (a delete or a steer) is unanswered, held here so a remount or another
   * route sees it still out (#2068). */
  queueWriteOutOf: (cardId: string) => boolean;
  /** Run one write to that card's queued entries, counted as out until it settles. */
  holdQueueWrite: <T>(cardId: string, write: () => Promise<T>) => Promise<T>;
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
  const [spentRows, setSpentRows] = useState<Readonly<Record<string, readonly string[]>>>({});
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
    /* With no send left there is nothing a row could stand for twice, whichever way the last one left (#2068). */
    if (after.length === 0) {
      setSpentRows((current) => {
        if (!(conversationId in current)) return current;
        const kept = { ...current };
        delete kept[conversationId];
        return kept;
      });
    }
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
  const retireSends = useCallback((conversationId: string, retired: readonly RetiredSend[]) => {
    const keys = new Set(retired.map(({ key }) => key));
    const left = editOutbox(conversationId, (current) => {
      const kept = current.filter((op) => !keys.has(op.key));
      return kept.length === current.length ? current : kept;
    });
    const rows = retired.flatMap(({ row }) => row === null ? [] : [row]);
    /* An emptied outbox has already dropped its rows in `editOutbox`. */
    if (left.length === 0 || rows.length === 0) return;
    setSpentRows((current) => ({ ...current, [conversationId]: [...current[conversationId] ?? [], ...rows] }));
  }, [editOutbox]);
  const spentRowsOf = useCallback((conversationId: string) => spentRows[conversationId] ?? NO_ROWS, [spentRows]);
  const nextRead = useCallback(() => { readsStarted.current += 1; return readsStarted.current; }, []);
  /* Written through the ref first, as the outbox is: a refill reads back what it wrote. */
  const composersRef = useRef<Readonly<Record<string, ComposerContent>>>({});
  const [composers, setComposers] = useState(composersRef.current);
  const editComposer = useCallback((conversationId: string, next: (current: ComposerContent) => ComposerContent) => {
    const before = composersRef.current[conversationId] ?? EMPTY_COMPOSER;
    const after = next(before);
    if (after === before) return before;
    const updated = { ...composersRef.current };
    /* An empty composer is no entry, so the map holds only conversations with something unsent. */
    if (after.text === '' && after.attachments.length === 0) delete updated[conversationId]; else updated[conversationId] = after;
    composersRef.current = updated;
    setComposers(updated);
    return after;
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
  const refillComposer = useCallback((conversationId: string, refill: ComposerContent) => {
    const merged = editComposer(conversationId, (current) => withRefill(current, refill));
    if (!tookEveryImage(merged, refill)) editUpload(conversationId, (current) => ({ ...current, refusal: TOO_MANY_IMAGES }));
  }, [editComposer, editUpload]);
  const [queueWrites, setQueueWrites] = useState<Readonly<Record<string, number>>>({});
  const holdQueueWrite = useCallback(<T,>(cardId: string, write: () => Promise<T>): Promise<T> => {
    const count = (step: 1 | -1) => setQueueWrites((current) => {
      const out = (current[cardId] ?? 0) + step;
      const updated = { ...current };
      if (out === 0) delete updated[cardId]; else updated[cardId] = out;
      return updated;
    });
    return (async () => {
      /* Counted after a tick: a press's `clickAction` runs in an async transition, which would hold an update made
         inside it until that action ends, when the write it counts is already answered. */
      await Promise.resolve();
      count(1);
      try { return await write(); } finally { count(-1); }
    })();
  }, []);
  const queueWriteOutOf = useCallback((cardId: string) => cardId in queueWrites, [queueWrites]);
  const noteEdit = useCallback((conversationId: string, notice: EditNotice) => {
    setEditNotices((current) => ({ ...current, [conversationId]: notice }));
  }, []);
  const beginEdit = useCallback((conversationId: string, edit: ConversationEdit) => {
    if (conversationId in editsRef.current) return false;
    /* What the click puts in the composer, as a pick: no more images than a message carries (#2068). */
    writeEdit(conversationId, { ...edit, refill: withRefill(EMPTY_COMPOSER, edit.refill) });
    refillComposer(conversationId, edit.refill);
    clearEditNotice(conversationId);
    return true;
  }, [clearEditNotice, refillComposer, writeEdit]);
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
  const noteRefusedSend = useCallback((conversationId: string, notice: RefusedSendNotice) => {
    noteEdit(conversationId, notice);
  }, [noteEdit]);
  const noteRestarted = useCallback((conversationId: string) => {
    noteEdit(conversationId, { kind: 'restarted' });
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
      composerOf, editComposer, refillComposer, newConversationComposerOf, editNewConversationComposer,
      editOf, beginEdit, cancelEdit, leaveEdit, noteRefusedSend, noteRestarted, editNoticeOf, uploadOf, editUpload,
      queueWriteOutOf, holdQueueWrite,
    }),
    [adoptDraft, adoptedDraftIdOf, clearOpenRequest, conversations, discardDraft,
      composerOf, discardUnsentDraft, draftOf, editComposer, refillComposer, queueWriteOutOf, holdQueueWrite, editDraft, editNewConversationComposer, newConversationComposerOf, editUpload, finishDraftAdoption, editOf, beginEdit, cancelEdit, leaveEdit, noteRefusedSend, noteRestarted, editNoticeOf, uploadOf, outboxOf, beginSend, editOutbox, retireSends, spentRowsOf, nextRead,
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
