import { createContext, useCallback, useContext, useSyncExternalStore, type ReactNode } from 'react';
import { EMPTY_COMPOSER, isComposerEmpty } from '../../../../core/domain/conversation-composer.ts';
import { NO_UPLOAD } from '../../features/planner/attachments.tsx';
import { useState } from '../../ui/state/public.ts';
import { createConversationOwner, NO_SENDS, NO_ROWS, NO_TURNS, type ConversationSnapshot } from './registry-owner.ts';

export type { ConversationDraft, ConversationDraftId, ConversationEdit, QueuedConversationEdit, EditNotice,
  RefusedSendNotice, RememberedConversation, ConversationRegistry } from './registry-model.ts';

const ConversationContext = createContext<ReturnType<typeof createConversationOwner> | null>(null);

export function ConversationProvider({ children }: { children: ReactNode }) {
  const [owner] = useState(createConversationOwner);
  return <ConversationContext.Provider value={owner}>{children}</ConversationContext.Provider>;
}

function useOwner() {
  const owner = useContext(ConversationContext);
  if (owner === null) throw new Error('ConversationProvider is required');
  return owner;
}

/** Stable commands and imperative reads. Render dependencies use the named snapshot hooks below. */
export function useConversationCommands() { return useOwner().commands; }

function useSnapshot<T>(select: (snapshot: ConversationSnapshot) => T, active = true): T {
  const owner = useOwner();
  const subscribe = useCallback((listener: () => void) => active ? owner.subscribe(select, listener) : () => {}, [active, owner, select]);
  const getSnapshot = useCallback(() => select(owner.getSnapshot()), [owner, select]);
  return useSyncExternalStore(subscribe, getSnapshot);
}

export function useConversationComposerText(cardId: string | null) {
  return useSnapshot(useCallback((state: ConversationSnapshot) => cardId === null ? '' : state.composers[cardId]?.text ?? '', [cardId]), cardId !== null);
}

export function useConversationAttachments(cardId: string | null) {
  return useSnapshot(useCallback((state: ConversationSnapshot) => cardId === null ? EMPTY_COMPOSER.attachments : state.composers[cardId]?.attachments ?? EMPTY_COMPOSER.attachments, [cardId]), cardId !== null);
}

export function useConversationComposerEmpty(cardId: string | null) {
  return useSnapshot(useCallback((state: ConversationSnapshot) => isComposerEmpty(cardId === null ? EMPTY_COMPOSER : state.composers[cardId] ?? EMPTY_COMPOSER), [cardId]), cardId !== null);
}

export function useNewConversationText(scopeId: string | null) {
  return useSnapshot(useCallback((state: ConversationSnapshot) => scopeId === null ? '' : state.newComposers[scopeId] ?? '', [scopeId]), scopeId !== null);
}

export function useConversationDraftSlot(scopeId: string | null) {
  return useSnapshot(useCallback((state: ConversationSnapshot) => scopeId === null ? null : state.drafts[scopeId] ?? null, [scopeId]), scopeId !== null);
}

export function useConversationDraft(scopeId: string | null) {
  const slot = useConversationDraftSlot(scopeId);
  return slot?.kind === 'held' ? slot.draft : null;
}

export function useConversationSendOps(cardId: string | null) {
  return useSnapshot(useCallback((state: ConversationSnapshot) => cardId === null ? NO_SENDS : state.outboxes[cardId] ?? NO_SENDS, [cardId]), cardId !== null);
}

export function useConversationSpentRows(cardId: string | null) {
  return useSnapshot(useCallback((state: ConversationSnapshot) => cardId === null ? NO_ROWS : state.spentRows[cardId] ?? NO_ROWS, [cardId]), cardId !== null);
}

export function useConversationRememberedTurns(cardId: string | null) {
  return useSnapshot(useCallback((state: ConversationSnapshot) => cardId === null ? NO_TURNS : state.entries[cardId]?.turns ?? NO_TURNS, [cardId]), cardId !== null);
}

export function useRememberedConversations() {
  return useSnapshot(useCallback((state: ConversationSnapshot) => state.conversations, []));
}

export function useConversationOpenRequest() {
  return useSnapshot(useCallback((state: ConversationSnapshot) => state.openRequest, []));
}

export function useConversationEditState(cardId: string | null) {
  return useSnapshot(useCallback((state: ConversationSnapshot) => cardId === null ? null : state.edits[cardId] ?? null, [cardId]), cardId !== null);
}

export function useConversationEditNotice(cardId: string | null) {
  return useSnapshot(useCallback((state: ConversationSnapshot) => cardId === null ? null : state.notices[cardId] ?? null, [cardId]), cardId !== null);
}

export function useConversationUpload(cardId: string | null) {
  return useSnapshot(useCallback((state: ConversationSnapshot) => cardId === null ? NO_UPLOAD : state.uploads[cardId] ?? NO_UPLOAD, [cardId]), cardId !== null);
}

export function useConversationQueueWriteOut(cardId: string | null) {
  return useSnapshot(useCallback((state: ConversationSnapshot) => cardId !== null && (state.queueWrites[cardId] ?? 0) > 0, [cardId]), cardId !== null);
}

export function useConversationRestartOut(cardId: string | null) {
  return useSnapshot(useCallback((state: ConversationSnapshot) => cardId !== null && state.restarts[cardId] === true, [cardId]), cardId !== null);
}
