import { useEffect, useRef } from 'react';
import type { Conversation } from '../../../../core/domain/conversation.ts';
import type { ConversationDraft, ConversationRegistry } from './public.tsx';
import type { ConversationStore } from './contracts.ts';
import type { useConversationViewTarget } from '../providers/ui-preferences.tsx';

type SetOpenTarget = ReturnType<typeof useConversationViewTarget>[1];

export function useConversationDraftRetention({ discardUnsentDraft, sourceScopeId }: Readonly<{
  discardUnsentDraft: ConversationRegistry['discardUnsentDraft'];
  sourceScopeId: string;
}>) {
  /* Preserve only a draft whose request actually left the browser; an untouched or
       locally refused draft has no server identity. */
  useEffect(() => {
    return () => { discardUnsentDraft(sourceScopeId); };
  }, [discardUnsentDraft, sourceScopeId]);
}

export function useConversationDraftAdoption({ adoptedDraftId, registry, rows, sourceScopeId, setOpenTarget }: Readonly<{
  adoptedDraftId: string | null;
  registry: Pick<ConversationRegistry, 'finishDraftAdoption'>;
  rows: readonly Conversation[];
  sourceScopeId: string;
  setOpenTarget: SetOpenTarget;
}>) {
  /* Adoption: the reducer moves a matching `{ scopeId, key }` from `held` to
       `adopted`; if the create settles while unmounted, the outcome waits here. */
  useEffect(() => {
    if (adoptedDraftId === null) return;
    if (!rows.some((row) => row.id === adoptedDraftId)) return;
    setOpenTarget({ kind: 'row', id: adoptedDraftId });
    registry.finishDraftAdoption(sourceScopeId, adoptedDraftId);
  }, [adoptedDraftId, registry, rows, sourceScopeId, setOpenTarget]);
}

export function useRequestedConversationOpen({ registry, rows, setOpenTarget, setComposerFocusFor, inline }: Readonly<{
  registry: Pick<ConversationRegistry, 'requestedOpenId' | 'requestedOpenFocusesComposer' | 'clearOpenRequest'>;
  rows: readonly Conversation[];
  setOpenTarget: SetOpenTarget;
  setComposerFocusFor: (id: string | null) => void;
  inline: boolean | undefined;
}>) {
  /* Consumed only when the rows are loaded AND contain the id, never cleared on
       absence: the list arrives a round trip after the request, and the id may
       belong to another track. */
  useEffect(() => {
    const requestedOpenId = registry.requestedOpenId;
    if (requestedOpenId === null || inline === true) return;
    /* Captured here, not read at render time: the request is cleared in the same
           commit that opens the row. */
    const focusComposer = registry.requestedOpenFocusesComposer;
    if (!rows.some((row) => row.id === requestedOpenId)) return;
    setOpenTarget({ kind: 'row', id: requestedOpenId });
    if (focusComposer) setComposerFocusFor(requestedOpenId);
    registry.clearOpenRequest();
  }, [registry, rows, setOpenTarget, inline]);
}

export function useConversationEscape({ open, store }: Readonly<{
  open: Conversation | null;
  store: Pick<ConversationStore, 'working' | 'stopping' | 'interrupt'>;
}>) {
  useEffect(() => {
    if (open === null) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape' || event.defaultPrevented || event.isComposing || event.keyCode === 229) return;
      if (!store.working || store.stopping) return;
      const target = event.target;
      if (!(target instanceof Element)) return;
      const region = target.closest('[data-nc-drawer]');
      if (region === null || region.id !== `conversation-${open.id}`) return;
      /* The source panel is a second `complementary` on the same track, and its Escape
               must not reach the planner; the region is asked whether it holds the panel's marker. */
      if (region.querySelector('[data-nc-report-source]') !== null) return;
      /* An open `/` or `@` menu owns Escape first; this capture-phase listener would otherwise
               take it. The menu says it is open through `aria-expanded` on the combobox. */
      if (target.closest('[role="combobox"][aria-expanded="true"]') !== null) return;
      /* Native overlays own their first Escape; let their standard dismissal stack run. */
      if (typeof HTMLElement.prototype.showPopover === 'function'
        && document.querySelector('[popover]:popover-open') !== null) return;
      event.preventDefault();
      event.stopImmediatePropagation();
      store.interrupt();
    };
    document.addEventListener('keydown', onKeyDown, true);
    return () => document.removeEventListener('keydown', onKeyDown, true);
  }, [open, store]);
}

export function useConversationDraftAutoSend({ draftOpen, draft, creating, registry, sendDraft }: Readonly<{
  draftOpen: boolean;
  draft: ConversationDraft | null;
  creating: boolean;
  registry: Pick<ConversationRegistry, 'editDraft'>;
  sendDraft: (text: string) => void;
}>) {
  const autoSent = useRef<string | null>(null);
  const sendDraftRef = useRef(sendDraft);
  sendDraftRef.current = sendDraft;
  useEffect(() => {
    if (!draftOpen || draft?.autoSend !== true || draft.text === null || draft.sentText !== null
      || creating || autoSent.current === draft.key) return;
    autoSent.current = draft.key;
    // Consume the one-shot intent before delivery; retry keys must never re-arm it.
    registry.editDraft(draft, (current) => ({ ...current, autoSend: false }));
    sendDraftRef.current(draft.text);
  }, [draftOpen, draft, creating, registry]);
}
