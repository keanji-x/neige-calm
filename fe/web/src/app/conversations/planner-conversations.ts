import { useMemo } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import type { AgentProvider } from '../../../../core/api/generated/wire.ts';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { Conversation, TranscriptEntry } from '../../../../core/domain/conversation.ts';
import { buildConversationTranscript } from '../../../../core/domain/conversation-display.ts';
import { writeFailureOf } from '../../../../core/domain/failure-class.ts';
import {
  PLANNER_HISTORY_KIND, parsePlannerHistoryKey, plannerConversationsOperation, plannerHistoryRows,
  plannerNewConversationFailureText, startPlannerConversationOperation,
} from '../../../../core/domain/planner-conversation.ts';
import { readErrorText } from '../../../../core/domain/read-failure.ts';
import { useCommittedCallback } from '../../ui/state/committed-callback.ts';
import { useCurrentUser } from '../auth/current-user.tsx';
import { mintIdempotencyKey } from '../providers/idempotency-key.ts';
import { plannerHistoryItemsQueryOptions, queryKeys, runOperation } from '../providers/queries.ts';
import { useRecoveryMutation } from '../providers/recovery-mutation.ts';
import type { PlannerNewConversation } from '../../features/chat/list/planner-conversation.tsx';
import type { ConversationStore } from './contracts.ts';
import { useConversationCommands } from './public.tsx';
import { useTranscriptHistory, useTranscriptReads } from './history.ts';

/**
 * A Track's Planner conversations (#2774): the earlier ones as archived, read-only rows, and the person's start of a new,
 * blank one. `start` mints a new key for each confirmed press, rejects with the one line to show, and on success opens
 * the Planner's row with its composer focused.
 */
export function usePlannerConversations({ transport, unauthorized, trackId, trackTitle, planner }: Readonly<{
  transport: ApiTransportPort;
  unauthorized: UnauthorizedChannel;
  trackId: string;
  trackTitle: string;
  planner: Readonly<{ cardId: string; provider: AgentProvider }> | null;
}>) {
  const client = useQueryClient();
  const registry = useConversationCommands();
  const cardId = planner?.cardId ?? '';
  const provider = planner?.provider;
  const list = useQuery({
    queryKey: queryKeys.plannerConversations(cardId),
    queryFn: ({ signal }: { signal: AbortSignal }) => runOperation(transport, { ...plannerConversationsOperation(cardId), signal }, unauthorized),
    enabled: planner !== null,
  });
  const rows = useMemo<readonly Conversation[]>(() => provider === undefined || list.data === undefined ? []
    : plannerHistoryRows({ cardId, trackId, trackTitle, provider }, list.data), [cardId, list.data, provider, trackId, trackTitle]);
  const mutation = useRecoveryMutation(transport, {
    mutationFn: ({ cardId: target, key }: { cardId: string; key: string }, admitted: ApiTransportPort) =>
      runOperation(admitted, startPlannerConversationOperation(target, key), unauthorized),
    /* Awaited before the press settles, whatever the answer: an unconfirmed start may have cut over, and a
       confirmed one opens on the blank transcript rather than the old one. */
    onSettled: (_data, _error, { cardId: target }) => Promise.all([
      client.invalidateQueries({ queryKey: queryKeys.harnessItems(target) }),
      client.invalidateQueries({ queryKey: queryKeys.plannerRun(target) }),
      client.invalidateQueries({ queryKey: queryKeys.plannerConversations(target) }),
      client.invalidateQueries({ queryKey: queryKeys.trackDetail(trackId) }),
    ]),
  });
  const { mutateAsync } = mutation;
  const start = useCommittedCallback(trackId, async (target: string) => {
    try { await mutateAsync({ cardId: target, key: mintIdempotencyKey() }); }
    catch (error) { throw new Error(plannerNewConversationFailureText(writeFailureOf(error)), { cause: error }); }
    registry.requestOpen(target, { focusComposer: true });
  });
  return { rows, start };
}

/**
 * What the lists are handed: `start` where the host can start one, and the Planner row held while its own turn, queue
 * or send is in flight. The hold is a hint only; the server decides.
 */
export function usePlannerNewConversationAction(start: ((cardId: string) => Promise<void>) | undefined, plannerCardId: string | null,
  openId: string | null, store: Pick<ConversationStore, 'working' | 'sending' | 'pendingQueue' | 'pendingQueueOverflow' | 'pendingSends' | 'pendingInputs'>,
): PlannerNewConversation | undefined {
  const held = plannerCardId !== null && openId === plannerCardId && (store.working || store.sending
    || store.pendingQueue.length > 0 || store.pendingQueueOverflow > 0 || store.pendingSends.length > 0
    || store.pendingInputs.length > 0);
  return useMemo(() => start === undefined ? undefined
    : { start, held: new Set(held && plannerCardId !== null ? [plannerCardId] : []) }, [start, held, plannerCardId]);
}

export type PlannerHistoryThread = Readonly<{
  turns: readonly TranscriptEntry[];
  ready: boolean;
  loading: boolean;
  error: string | null;
  retry: () => void;
  earlier: Readonly<{ cursor: number | null; pending: boolean; failed: boolean; load: () => boolean }>;
}>;

/**
 * The open earlier conversation's transcript, through the same windowed history the current one uses; `null` when the
 * open row is not one. It reads only `harness/items?contract=conversation_v1&conversation=k`: no run, queue or model read is made for it.
 */
export function usePlannerHistoryThread(transport: ApiTransportPort, unauthorized: UnauthorizedChannel,
  row: Conversation | null): PlannerHistoryThread | null {
  const registry = useConversationCommands();
  const viewerId = useCurrentUser()?.userId ?? null;
  const target = row?.kind === PLANNER_HISTORY_KIND ? parsePlannerHistoryKey(row.id) : null;
  const reads = useTranscriptReads(registry.nextRead);
  const cardId = target?.cardId ?? '';
  const ordinal = target?.ordinal ?? 0;
  const options = useMemo(() => reads.track(plannerHistoryItemsQueryOptions(transport, cardId, ordinal, unauthorized)),
    [reads, transport, cardId, ordinal, unauthorized]);
  const history = useTranscriptHistory(options, target !== null, reads);
  const { loadedRows, refetch, isFetching, error, loadEarlier } = history;
  const { cursor, pending, failed } = history.earlier;
  const ready = history.data !== undefined;
  const open = target !== null;
  return useMemo(() => open ? {
    turns: buildConversationTranscript(loadedRows, viewerId), ready, loading: isFetching,
    error: error === null ? null : readErrorText(error, 'The conversation history could not be loaded.'),
    retry: () => { void refetch(); },
    earlier: { cursor, pending, failed, load: loadEarlier },
  } : null, [open, loadedRows, viewerId, ready, isFetching, error, refetch, cursor, pending, failed, loadEarlier]);
}
