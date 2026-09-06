import { useEffect, useRef } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { findIndependentTask, independentTaskFailureUncertain, independentTaskRevision, independentTaskUnavailableReason, startIndependentTaskOperation,
  type IndependentTaskIntent as Intent, type IndependentTaskRequest } from '../../../../core/domain/independent-task.ts';
import { trackDetailOperation, type CardWire, type TrackLifecycle } from '../../../../core/domain/track.ts';
import { IndependentTaskForm } from '../../features/track/independent-task/form.tsx';
import { useState } from '../../ui/state/public.ts';
import { ApiError, queryKeys, runOperation } from '../providers/queries.ts';
import { mintIdempotencyKey } from './idempotency-key.ts';

/** Session cache survives route unmounts. Writes always finish even if the form is closed. */
export function useIndependentTaskLaunch({ trackId, cards, lifecycle, transport, unauthorized, onCreated }: {
  lifecycle: TrackLifecycle; trackId: string; cards: readonly CardWire[]; transport: ApiTransportPort;
  unauthorized: UnauthorizedChannel; onCreated: (blockId: string) => void;
}) {
  const client = useQueryClient();
  const key = ['independent-task-intent', trackId];
  const state = useQuery<Intent>({ queryKey: key, queryFn: () => ({ phase: 'editing', goal: '' }),
    initialData: { phase: 'editing', goal: '' }, enabled: false, staleTime: Infinity, gcTime: Infinity });
  const intent = state.data;
  const [open, setOpen] = useState(false);
  const revealed = useRef<string | null>(null);
  const revision = independentTaskRevision(cards);
  useEffect(() => {
    if (intent.phase !== 'accepted' || revealed.current === intent.receipt.taskKey
      || findIndependentTask(cards, intent.request) === null) return;
    revealed.current = intent.receipt.taskKey;
    setOpen(false);
    onCreated(intent.receipt.blockId);
  }, [cards, intent, onCreated, setOpen]);
  const refresh = () => Promise.all([
    client.invalidateQueries({ queryKey: queryKeys.trackDetail(trackId) }),
    client.invalidateQueries({ queryKey: queryKeys.trackReport(trackId) }),
  ]);
  const reconcile = async (request: IndependentTaskRequest) => {
    const detail = await runOperation(transport, trackDetailOperation(trackId), unauthorized);
    const receipt = findIndependentTask(detail.cards, request);
    if (receipt === null) return false;
    client.setQueryData(queryKeys.trackDetail(trackId), detail);
    client.setQueryData<Intent>(key, () => ({ phase: 'accepted', request, receipt }));
    await refresh();
    return true;
  };
  const submit = async (checkOnly = false) => {
    const active = client.getQueryData<Intent>(key);
    if (active === undefined || active.phase === 'sending' || active.phase === 'accepted') return;
    const wasUncertain = active.phase === 'uncertain';
    const goal = active.phase === 'editing' ? active.goal : active.request.goal;
    if (!wasUncertain && (goal.trim() === '' || revision === null || independentTaskUnavailableReason(lifecycle) !== null)) return;
    const request = wasUncertain ? active.request
      : { key: `independent-${mintIdempotencyKey()}`, goal, ifDocRev: revision! };
    // This cache write is synchronous, so a second click cannot race React's render.
    client.setQueryData<Intent>(key, () => ({ phase: 'sending', request, message: null }));
    try {
      if (wasUncertain && await reconcile(request)) return;
      if (checkOnly) {
        client.setQueryData<Intent>(key, () => ({ phase: 'uncertain', request, message: 'No matching task is visible yet.' }));
        return;
      }
      const receipt = await runOperation(transport, startIndependentTaskOperation(trackId, request), unauthorized);
      client.setQueryData<Intent>(key, () => ({ phase: 'accepted', request, receipt }));
      await refresh();
    } catch (error: unknown) {
      const message = error instanceof Error ? error.message : 'The server response is unavailable.';
      // A rejected retry cannot disprove that the original uncertain write committed.
      const uncertain = wasUncertain || !(error instanceof ApiError) || independentTaskFailureUncertain(error.failure);
      client.setQueryData<Intent>(key, () => ({ phase: uncertain ? 'uncertain' : 'rejected', request, message }));
      if (uncertain && !checkOnly) {
        try { await reconcile(request); } catch { /* Keep the unchanged intent for explicit retry. */ }
      } else await refresh();
    }
  };
  return {
    open: () => {
      if (client.getQueryData<Intent>(key)?.phase === 'accepted') {
        client.setQueryData<Intent>(key, () => ({ phase: 'editing', goal: '' }));
      }
      setOpen(true);
    },
    form: <IndependentTaskForm open={open} intent={intent} lifecycle={lifecycle} revisionAvailable={revision !== null}
      onClose={() => setOpen(false)} onGoal={(goal) => {
        const current = client.getQueryData<Intent>(key);
        if (current?.phase === 'editing' || current?.phase === 'rejected') client.setQueryData<Intent>(key, () => ({ phase: 'editing', goal }));
      }} onSubmit={() => { void submit(); }} onCheck={() => { void submit(true); }} />,
  };
}
