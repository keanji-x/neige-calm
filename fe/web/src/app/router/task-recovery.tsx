import { useQueries, useQuery, useQueryClient, type UseQueryOptions } from '@tanstack/react-query';
import type { ReportTaskRow } from '../../../../core/domain/report.ts';
import type { ApiFailure, ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { recoverTaskOperation, taskAttemptsOperation, type TaskRecoveryRequest, type TaskRecoveryView } from '../../../../core/domain/task-recovery.ts';
import { TaskRecoveryDetails } from '../../features/report/task/recovery.tsx';
import { ApiError, queryKeys, runOperation } from '../providers/queries.ts';
import { currentTaskExecution, type TaskRecoveryIntent as Intent } from '../../../../core/domain/task-execution.ts';
import { mintIdempotencyKey } from './idempotency-key.ts';

function uncertainFailure(failure: ApiFailure): boolean {
  return failure.kind === 'transport' || failure.kind === 'decode'
    || (failure.kind === 'http' && failure.status >= 500);
}

/** QueryClient retains uncertain intent across collapse/navigation within this app session. */
export function TaskRecovery({ trackId, taskKey, expanded, transport, unauthorized, openWorker, openableWorkerIds }: {
  trackId: string; taskKey: string; expanded: boolean;
  transport: ApiTransportPort; unauthorized: UnauthorizedChannel;
  openWorker: (cardId: string) => void; openableWorkerIds: ReadonlySet<string>;
}) {
  const client = useQueryClient();
  // Prefix follows existing task/report events without introducing a second event listener.
  const historyKey = taskHistoryKey(trackId, taskKey);
  const intentKey = taskRecoveryIntentKey(trackId, taskKey);
  const intentQuery = useQuery<Intent>({ queryKey: intentKey, queryFn: () => ({ phase: 'idle' }),
    initialData: { phase: 'idle' }, enabled: false, staleTime: Infinity, gcTime: Infinity });
  const intent = intentQuery.data;
  const history = useQuery({ queryKey: historyKey, enabled: expanded, retry: false,
    queryFn: ({ signal }) => runOperation(transport, { ...taskAttemptsOperation(trackId, taskKey), signal }, unauthorized),
    refetchInterval: (query) => expanded && !query.state.error && query.state.data !== undefined
      && !['done', 'canceled'].includes(query.state.data.current.status) ? 3000 : false,
  });
  const refresh = async () => {
    await Promise.all([
      history.refetch(),
      client.invalidateQueries({ queryKey: queryKeys.trackReport(trackId), exact: true }),
      client.invalidateQueries({ queryKey: queryKeys.trackDetail(trackId) }),
    ]);
  };
  const recover = async () => {
    // Read cache synchronously: two activations before the next render still make one request.
    const active = client.getQueryData<Intent>(intentKey);
    if (active?.phase === 'sending') return;
    const view = history.data;
    let request: TaskRecoveryRequest;
    if (active?.phase === 'uncertain') request = active.request;
    else {
      if (history.isError || history.isFetching || view === undefined || !view.recovery.allowed
        || view.current.status !== 'failed'
        || currentTaskExecution(view, active)?.status !== 'failed') return;
      request = { expected_attempt_id: view.current.attempt_id, idempotency_key: mintIdempotencyKey(),
        reason: 'User requested a new attempt under the unchanged task requirements.' };
    }
    client.setQueryData<Intent>(intentKey, () => ({ phase: 'sending', request }));
    const result = await runOperation(transport, recoverTaskOperation(trackId, taskKey, request), unauthorized)
      .then((value) => ({ status: 'ready', value } as const))
      .catch((error: unknown) => {
        if (!(error instanceof ApiError)) throw error;
        return { status: 'failed', error: error.failure } as const;
      });
    if (result.status === 'ready') {
      client.setQueryData<Intent>(intentKey, () => ({ phase: 'accepted', receipt: result.value }));
      await refresh();
    } else if (uncertainFailure(result.error)) {
      client.setQueryData<Intent>(intentKey, () => ({ phase: 'uncertain', request }));
    } else {
      client.setQueryData<Intent>(intentKey, () => ({ phase: 'rejected', message: result.error.message }));
      await refresh();
    }
  };
  const execution = currentTaskExecution(history.data, intent);
  const busy = intent.phase === 'sending';
  const canRecover = intent.phase === 'uncertain' || busy
    || (!history.isError && !history.isFetching && history.data?.recovery.allowed === true
      && history.data.current.status === 'failed'
      && execution?.status === 'failed');
  const current = history.data?.current;
  const accepted = intent.phase === 'accepted' && (current === undefined
    || current.attempt_id === intent.receipt.previous_attempt_id
    || (current.attempt_id === intent.receipt.attempt_id
      && ['pending', 'dispatched', 'awaiting_projection'].includes(current.status)));
  return <TaskRecoveryDetails current={execution} view={history.data} loading={history.isFetching} busy={busy}
    loadError={history.error instanceof ApiError ? history.error.message : history.isError ? 'History is unavailable.' : null}
    error={intent.phase === 'rejected' ? intent.message : intent.phase === 'uncertain'
      ? 'The recovery response could not be confirmed. Retry the same request to check its outcome.' : null}
    accepted={accepted} retryUncertain={intent.phase === 'uncertain'}
    onRefresh={() => { void refresh(); }} onRecover={canRecover ? () => { void recover(); } : undefined}
    openWorker={openWorker} openableWorkerIds={openableWorkerIds} />;
}

function taskHistoryKey(trackId: string, key: string) {
  return [...queryKeys.trackReport(trackId), 'attempts', key];
}
function taskRecoveryIntentKey(trackId: string, key: string) {
  return ['task-recovery-intent', trackId, key];
}

/** Cache observers only: TaskRecovery remains the sole history fetch/request owner.
 * No mirrored state or effect copies; every surface derives from the same two cache entries. */
export function useCurrentTaskRows(trackId: string, rows: readonly ReportTaskRow[]): readonly ReportTaskRow[] {
  const histories = useQueries({ queries: rows.map((row): UseQueryOptions<TaskRecoveryView> => ({
    queryKey: taskHistoryKey(trackId, row.key), enabled: false,
  })) });
  const intents = useQueries({ queries: rows.map((row): UseQueryOptions<Intent> => ({
    queryKey: taskRecoveryIntentKey(trackId, row.key), enabled: false, gcTime: Infinity,
  })) });
  return rows.map((row, index) => {
    if (row.state === 'withdrawn' || row.state === 'unreadable'
      || rows.filter((candidate) => candidate.key === row.key).length !== 1) return row;
    const execution = currentTaskExecution(histories[index].data, intents[index].data);
    return execution === undefined ? row : { ...row, execution };
  });
}
