import { useQueries, useQuery, useQueryClient, type UseQueryOptions } from '@tanstack/react-query';
import type { ReportTaskRow } from '../../../../core/domain/report.ts';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { taskAttemptsOperation, type TaskRecoveryView } from '../../../../core/domain/task-recovery.ts';
import { TaskRecoveryDetails } from '../../features/report/task/recovery.tsx';
import { readErrorText } from '../../../../core/domain/read-failure.ts';
import { queryKeys, runOperation } from '../providers/queries.ts';
import { hasReadFailure } from '../providers/query-read-feedback.ts';
import { currentTaskExecution } from '../../../../core/domain/task-execution.ts';

/** Explicit detail demand admits the first read. Previously admitted execution
 * remains refreshed by either visible inventory or detail, sharing one query. */
function taskHistoryQueryOptions(
  transport: ApiTransportPort, unauthorized: UnauthorizedChannel,
  trackId: string, taskKey: string, expanded: boolean,
): UseQueryOptions<TaskRecoveryView> {
  return {
    queryKey: taskHistoryKey(trackId, taskKey),
    enabled: query => expanded || query.state.data !== undefined,
    retry: false,
    queryFn: ({ signal }) => runOperation(transport, { ...taskAttemptsOperation(trackId, taskKey), signal }, unauthorized),
    refetchInterval: query => !query.state.error && query.state.data !== undefined
      && !['done', 'canceled'].includes(query.state.data.current?.status ?? '') ? 3000 : false,
  };
}

export function TaskRecovery({ trackId, taskKey, expanded, transport, unauthorized, openWorker, openableWorkerIds }: {
  trackId: string; taskKey: string; expanded: boolean;
  transport: ApiTransportPort; unauthorized: UnauthorizedChannel;
  openWorker: (cardId: string) => void; openableWorkerIds: ReadonlySet<string>;
}) {
  const client = useQueryClient();
  const history = useQuery(taskHistoryQueryOptions(transport, unauthorized, trackId, taskKey, expanded));
  const refresh = async () => {
    await Promise.all([
      history.refetch(),
      client.invalidateQueries({ queryKey: queryKeys.trackReport(trackId), exact: true }),
      client.invalidateQueries({ queryKey: queryKeys.trackDetail(trackId) }),
    ]);
  };
  return <TaskRecoveryDetails current={currentTaskExecution(history.data)} view={history.data} loading={history.isFetching}
    loadError={hasReadFailure(history) ? readErrorText(history.error, 'Could not refresh execution history.') : null}
    onRefresh={() => { void refresh(); }}
    openWorker={openWorker} openableWorkerIds={openableWorkerIds} />;
}

function taskHistoryKey(trackId: string, key: string) {
  return [...queryKeys.trackReport(trackId), 'attempts', key];
}

/** Never-opened history stays dormant. Cached execution used as authority keeps
 * an active refresh owner even on a folded revisit; no mirrored data or effects. */
export function useCurrentTaskRows(
  transport: ApiTransportPort, unauthorized: UnauthorizedChannel,
  trackId: string, rows: readonly ReportTaskRow[],
): readonly ReportTaskRow[] {
  const histories = useQueries({ queries: rows.map(row =>
    taskHistoryQueryOptions(transport, unauthorized, trackId, row.key, false)) });
  const keyCounts = new Map<string, number>();
  for (const row of rows) keyCounts.set(row.key, (keyCounts.get(row.key) ?? 0) + 1);
  return rows.map((row, index) => {
    if (row.state === 'withdrawn' || row.state === 'unreadable'
      || keyCounts.get(row.key) !== 1) return row;
    const execution = currentTaskExecution(histories[index].data);
    return execution === undefined ? row : { ...row, execution };
  });
}
