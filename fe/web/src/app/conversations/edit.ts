import { useCallback, useEffect, useRef, type Dispatch, type SetStateAction } from 'react';

import type { ConversationTurnOutcome, TranscriptEntry, PendingQueueEntry, PlannerQueueEditOutcome } from '../../../../core/domain/conversation.ts';
import { markedReplace, replacingTurn, type ReplacedTurn } from '../../../../core/domain/conversation-outbox.ts';
import { notSentMessage } from '../../../../core/domain/conversation-delivery.ts';
import { editedTurnRefill, isComposerEmpty, isLatestTurn } from '../../../../core/domain/conversation-composer.ts';
import { useState } from '../../ui/state/public.ts';
import { useConversationRegistry } from './public.tsx';

/** Beside a refused replace while its words are still in the composer: they are a new message now. */
export const REFILLED_NOTE = 'Your message is back in the composer; sending adds a new one.';

/** One retained composer edit flow. Turn edits replace a completed turn; queued edits update an existing entry in place. */
export function useConversationEdit({ conversationId, transcript, historyReady, focusComposer, changeComposer, queue }: {
  conversationId: string | null;
  transcript: readonly TranscriptEntry[];
  historyReady: boolean;
  focusComposer: (conversationId: string) => void;
  changeComposer: Dispatch<SetStateAction<string>>;
  queue: Readonly<{
    entries: readonly PendingQueueEntry[]; ready: boolean; busy: boolean;
    save: (entry: PendingQueueEntry, text: string) => Promise<PlannerQueueEditOutcome>;
  }>;
}) {
  const { editOf, beginEdit, cancelEdit, settleQueuedEdit, leaveEdit, editNoticeOf, outboxOf, composerOf } = useConversationRegistry();
  const held = conversationId === null ? null : editOf(conversationId);
  const ops = conversationId === null ? [] : outboxOf(conversationId);
  const replacing = replacingTurn(ops);
  const leases = useRef(new Set<string>());
  const [saving, setSaving] = useState<ReadonlySet<string>>(() => new Set());
  const [queuedResult, setQueuedResult] = useState<Readonly<{ cardId: string; entryId: string; outcome: PlannerQueueEditOutcome }> | null>(null);
  const [savedEntry, setSavedEntry] = useState<Readonly<{ cardId: string; entry: PendingQueueEntry }> | null>(null);
  const stale = historyReady && held?.kind === 'turn' && !isLatestTurn(transcript, held.outcomeId) ? held.outcomeId : null;
  useEffect(() => {
    if (conversationId !== null && stale !== null) leaveEdit(conversationId, stale);
  }, [conversationId, leaveEdit, stale]);
  const start = (outcome: ConversationTurnOutcome) => {
    if (conversationId === null) return;
    const refill = editedTurnRefill(transcript, outcome.id);
    if (refill === null || !beginEdit(conversationId, { kind: 'turn', turnId: outcome.turnId, outcomeId: outcome.id, refill })) return;
    focusComposer(conversationId);
  };
  const startQueued = (entry: PendingQueueEntry) => {
    if (conversationId === null || queue.busy || !queue.ready || !isComposerEmpty(composerOf(conversationId))) return;
    if (!beginEdit(conversationId, { kind: 'queued', entry, refill: { text: entry.text, attachments: [] } })) return;
    setQueuedResult(null);
    focusComposer(conversationId);
  };
  const queued = held?.kind === 'queued' ? held : null;
  const queuedMissing = queued !== null && (!queue.ready || !queue.entries.some(entry => entry.entry_id === queued.entry.entry_id));
  const queuedSaving = conversationId !== null && (saving.has(conversationId) || (queued !== null && queue.busy));
  const saveQueued = (text: string): false => {
    if (conversationId === null || queued === null || queuedMissing || queue.busy || leases.current.has(conversationId) || text.trim() === '') return false;
    const cardId = conversationId;
    const submitted = composerOf(cardId);
    leases.current.add(cardId); setSaving(new Set(leases.current)); setQueuedResult(null);
    void (async () => {
      let outcome: PlannerQueueEditOutcome;
      try { outcome = await queue.save(queued.entry, text); }
      catch (error) { outcome = { kind: 'failed', message: error instanceof Error ? error.message : 'Could not save this queued message.' }; }
      settleQueuedEdit(cardId, queued, submitted, outcome);
      if (outcome.kind === 'done') setSavedEntry({ cardId, entry: outcome.entry });
      else setQueuedResult({ cardId, entryId: queued.entry.entry_id, outcome });
      leases.current.delete(cardId); setSaving(new Set(leases.current));
    })();
    // The existing composer keeps an untaken draft; the acknowledged queue write ends this edit.
    return false;
  };
  const onComposerChange = useCallback<Dispatch<SetStateAction<string>>>(action => {
    if (conversationId === null || !leases.current.has(conversationId)) changeComposer(action);
  }, [conversationId, changeComposer]);
  const heldNotice = conversationId === null ? null : editNoticeOf(conversationId);
  const notice = heldNotice?.kind === 'dormant' || heldNotice?.kind === 'restarted' ? null : heldNotice;
  const cancel = () => { if (conversationId !== null && !queuedSaving) cancelEdit(conversationId); };
  return {
    held, queued, queuedMissing, queuedSaving, startQueued, saveQueued, onComposerChange,
    canEditQueued: held === null && conversationId !== null && isComposerEmpty(composerOf(conversationId)) && queue.ready && !queue.busy,
    savedQueuedEntry: savedEntry?.cardId === conversationId ? savedEntry.entry : null,
    queueFailure: queuedResult?.cardId === conversationId && queuedResult.entryId === queued?.entry.entry_id ? queuedResult.outcome : null,
    replacesIn: (pressedIn: string): ReplacedTurn | null => {
      const edit = editOf(pressedIn);
      return edit?.kind === 'turn' ? { turnId: edit.turnId, outcomeId: edit.outcomeId } : null;
    },
    bar: held === null ? undefined : { preview: held.refill.text, onCancel: cancel,
      ...(held.kind === 'queued' ? { submitLabel: 'Save queued message', canSubmit: !queuedMissing } : {}) },
    replacing: replacing !== null,
    marked: held?.kind === 'turn' ? held.outcomeId : markedReplace(ops)?.outcomeId ?? null,
    notice: notice === null ? null : notice.kind === 'refused'
      ? { tone: 'error', lines: notice.edit ? [`Edit failed: ${notice.message}`,
        ...(conversationId === null || isComposerEmpty(composerOf(conversationId)) ? [] : [REFILLED_NOTE])]
        : [notSentMessage(notice.message)] } as const
      : { tone: 'neutral', lines: ['This message can no longer be replaced; sending adds a new one.'] } as const,
    start,
  };
}
