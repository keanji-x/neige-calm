import { useQueries, useQuery, useQueryClient, type UseQueryOptions } from '@tanstack/react-query';
import type { ReportTaskRow } from '../../../../core/domain/report.ts';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { taskAttemptsOperation, type TaskRecoveryView } from '../../../../core/domain/task-recovery.ts';
import { TaskRecoveryDetails } from '../../features/report/task/recovery.tsx';
import { ApiError, queryKeys, runOperation } from '../providers/queries.ts';
import { currentTaskExecution } from '../../../../core/domain/task-execution.ts';

export function TaskRecovery({ trackId, taskKey, expanded, transport, unauthorized, openWorker, openableWorkerIds }: {
  trackId: string; taskKey: string; expanded: boolean;
  transport: ApiTransportPort; unauthorized: UnauthorizedChannel;
  openWorker: (cardId: string) => void; openableWorkerIds: ReadonlySet<string>;
}) {
  const client = useQueryClient();
  // Prefix follows existing task/report events without introducing a second event listener.
  const historyKey = taskHistoryKey(trackId, taskKey);
  // Once loaded, current execution also drives the collapsed summary and inventory.
  // Keep this sole fetch owner active so events and polling can refresh that authority.
  const history = useQuery<TaskRecoveryView>({ queryKey: historyKey,
    enabled: (query) => expanded || query.state.data !== undefined, retry: false,
    queryFn: ({ signal }) => runOperation(transport, { ...taskAttemptsOperation(trackId, taskKey), signal }, unauthorized),
    refetchInterval: (query) => !query.state.error && query.state.data !== undefined
      && !['done', 'canceled'].includes(query.state.data.current?.status ?? '') ? 3000 : false,
  });
  const refresh = async () => {
    await Promise.all([
      history.refetch(),
      client.invalidateQueries({ queryKey: queryKeys.trackReport(trackId), exact: true }),
      client.invalidateQueries({ queryKey: queryKeys.trackDetail(trackId) }),
    ]);
  };
  return <TaskRecoveryDetails current={currentTaskExecution(history.data)} view={history.data} loading={history.isFetching}
    loadError={history.error instanceof ApiError ? history.error.message : history.isError ? 'History is unavailable.' : null}
    onRefresh={() => { void refresh(); }}
    openWorker={openWorker} openableWorkerIds={openableWorkerIds} />;
}

function taskHistoryKey(trackId: string, key: string) {
  return [...queryKeys.trackReport(trackId), 'attempts', key];
}

/** Cache observers only: TaskRecovery remains the sole history fetch owner.
 * No mirrored state or effect copies; every surface derives from the same cache entry. */
export function useCurrentTaskRows(trackId: string, rows: readonly ReportTaskRow[]): readonly ReportTaskRow[] {
  const histories = useQueries({ queries: rows.map((row): UseQueryOptions<TaskRecoveryView> => ({
    queryKey: taskHistoryKey(trackId, row.key), enabled: false,
  })) });
  return rows.map((row, index) => {
    if (row.state === 'withdrawn' || row.state === 'unreadable'
      || rows.filter((candidate) => candidate.key === row.key).length !== 1) return row;
    const execution = currentTaskExecution(histories[index].data);
    return execution === undefined ? row : { ...row, execution };
  });
}
