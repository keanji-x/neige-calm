import type { KeepConversationInputRequest, AgentProvider } from '../../../../core/api/generated/wire.ts';
import type { SideConversation, PendingQueueEntry, PlannerQueueEditOutcome, ModelSelection, Conversation, CreatedConversation, TranscriptEntry } from '../../../../core/domain/conversation.ts';
import type { SubmittedConversationInput } from '../../../../core/domain/conversation-initial-input.ts';
import type { RetiredSend, SendOp, ConversationOutboxOp } from '../../../../core/domain/conversation-outbox.ts';
import type { ComposerContent } from '../../../../core/domain/conversation-composer.ts';
import type { UploadState } from '../../features/planner/attachments.tsx';

export type ConversationOpenRequest = Readonly<{ id: string; focusComposer: boolean }>;

/**
 * A conversation being written. The key belongs to the draft for its whole lifetime: a failed create
 * keeps `key` and `submitted`, since retrying an ambiguous request under a new key would mint a second conversation.
 */
export type ConversationDraft = Readonly<{
  /** The Track this draft belongs to. */
  scopeId: string;
  /** Frozen source attached to this draft and every retry of it. */
  side?: SideConversation;
  /** `/side question` sends the prepared question once; recovery thereafter is explicit. */
  autoSend?: boolean;
  /** Chosen before sending and locked while delivery is unconfirmed. */
  provider: AgentProvider;
  model: ModelSelection;
  /** Identifies the draft to the server; minted once, never once per send. */
  key: string;
  /** The words the drawer is holding after its composer clears on send. */
  text: string | null;
  /** Original dispatched body and client display time; retries preserve this exact fact. */
  submitted: SubmittedConversationInput | null;
  /** The create/recovery chain for this key has not settled yet. */
  creating: boolean;
  error: string | null;
  remedy: 'retry' | 'new-conversation' | null;
}>;

export type ConversationDraftId = Readonly<Pick<ConversationDraft, 'scopeId' | 'key'>>;

/** One slot per Track: unfinished draft work, or the row that work became — mutually exclusive by construction. */
export type DraftSlot = Readonly<
  | { kind: 'held'; draft: ConversationDraft }
  | { kind: 'adopted'; conversationId: string }
>;

export type DraftSlots = Readonly<Record<string, DraftSlot>>;

export type DraftMove = Readonly<
  | { kind: 'start'; draft: ConversationDraft }
  | { kind: 'edit'; from: ConversationDraftId; next: (current: ConversationDraft) => ConversationDraft }
  | { kind: 'adopt'; from: ConversationDraftId; conversationId: string }
  | { kind: 'discard'; from: ConversationDraftId }
  | { kind: 'discard-unsent'; scopeId: string }
  | { kind: 'finish-adoption'; scopeId: string; conversationId: string }
>;

export const slotHolds = (slot: DraftSlot | undefined, id: ConversationDraftId): slot is Extract<DraftSlot, { kind: 'held' }> =>
  slot?.kind === 'held' && slot.draft.scopeId === id.scopeId && slot.draft.key === id.key;

function withoutSlot(slots: DraftSlots, scopeId: string): DraftSlots {
  const next = { ...slots };
  delete next[scopeId];
  return next;
}

export function moveDraft(slots: DraftSlots, move: DraftMove): DraftSlots {
  switch (move.kind) {
    case 'start':
      return { ...slots, [move.draft.scopeId]: { kind: 'held', draft: move.draft } };
    case 'edit': {
      const slot = slots[move.from.scopeId];
      if (!slotHolds(slot, move.from)) return slots;
      const draft = move.next(slot.draft);
      return draft === slot.draft ? slots : { ...slots, [move.from.scopeId]: { kind: 'held', draft } };
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
      return slot?.kind === 'held' && slot.draft.submitted === null
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
export type ConversationEdit = Readonly<{ refill: ComposerContent }> & Readonly<
  | { kind: 'turn'; turnId: string; outcomeId: string }
  | { kind: 'queued'; entry: PendingQueueEntry }
>;
export type QueuedConversationEdit = Extract<ConversationEdit, { kind: 'queued' }>;

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

export type KeepInputIntent = Readonly<{ key: string; args: Readonly<KeepConversationInputRequest>; status: 'sending' | 'unknown' }>;

export type ConversationRegistry = Readonly<{
  beginKeep: (card: string, args: KeepConversationInputRequest, key: string) => KeepInputIntent | null;
  finishKeep: (card: string, key: string, status: 'unknown' | 'done') => void;
  turnsOf: (conversationId: string) => readonly TranscriptEntry[];
  /** Stored canonical source only. Never feed the display's outbox fold back into memory. */
  rememberedTurnsOf: (conversationId: string) => readonly TranscriptEntry[];
  remember: (conversation: Conversation, turns: readonly TranscriptEntry[]) => void;
  /**
   * Amend an existing entry using the owner's current canonical fact, never an async caller's captured render.
   * `amend` must be pure. Creates nothing that `rememberOn` declined.
   */
  updateExisting: (
    conversationId: string, amend: (entry: RememberedConversation) => RememberedConversation,
  ) => void;
  requestOpen: (conversationId: string, options?: { focusComposer?: boolean }) => void;
  clearOpenRequest: () => void;
  /** Failed first-message attempts live here, above every route remount. */
  draftOf: (scopeId: string) => ConversationDraft | null;
  startDraft: (draft: ConversationDraft) => void;
  editDraft: (
    from: ConversationDraftId, next: (current: ConversationDraft) => ConversationDraft,
  ) => void;
  /** Import original acceptance and adopt only this still-held scope/key before list publication. */
  acceptDraft: (from: ConversationDraftId, result: CreatedConversation, submitted: SubmittedConversationInput) => boolean;
  discardDraft: (from: ConversationDraftId) => void;
  discardUnsentDraft: (scopeId: string) => void;
  adoptedDraftIdOf: (scopeId: string) => string | null;
  finishDraftAdoption: (scopeId: string, conversationId: string) => void;
  /** Each conversation's keyed sends not yet shown by the server, above every route remount. */
  outboxOf: (conversationId: string) => readonly ConversationOutboxOp[];
  /** Start a send there (`beginSendOp`): false while that conversation cannot take it. Clears its Edit notice, and
   * ends the Edit the send replaces the turn of. */
  beginSend: (conversationId: string, op: SendOp) => boolean;
  /** Change one conversation's outbox, whichever is shown; returns it as written. */
  editOutbox: (conversationId: string, next: (current: readonly ConversationOutboxOp[]) => readonly ConversationOutboxOp[]) => readonly ConversationOutboxOp[];
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
  /** Settle a queued edit only while this exact edit is still held; accepted saves clear only their submitted draft. */
  settleQueuedEdit: (conversationId: string, edit: QueuedConversationEdit, submitted: ComposerContent, outcome: PlannerQueueEditOutcome) => void;
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
  /** Whether a restart of one card's session is unanswered, held here so a remount or another route sees it still out
   * (#2192). */
  restartOutOf: (cardId: string) => boolean;
  /** Run one restart of that card's session, or `null` while one is out: each restart replaces the session the one
   * before it started. */
  holdRestart: (cardId: string, restart: () => Promise<void>) => Promise<void> | null;
  /* Deliberately no "open the planner conversation of track W" slot: the track being left is still
       mounted when a create states it, so that intent travels in the history entry instead. */
}>;
