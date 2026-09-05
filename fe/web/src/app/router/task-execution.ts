import { useQueries, type UseQueryOptions } from '@tanstack/react-query';
import type { ReportTaskRow } from '../../../../core/domain/report.ts';
import { currentTaskExecution, type TaskRecoveryIntent } from '../../../../core/domain/task-execution.ts';
import type { TaskRecoveryView } from '../../../../core/domain/task-recovery.ts';
import { queryKeys } from '../providers/queries.ts';

export function taskHistoryKey(trackId: string, key: string) {
  return [...queryKeys.trackReport(trackId), 'attempts', key];
}
export function taskRecoveryIntentKey(trackId: string, key: string) {
  return ['task-recovery-intent', trackId, key];
}

/** Cache observers only: TaskRecovery remains the sole history fetch/request owner.
 * No mirrored state or effect copies; every surface derives from the same two cache entries. */
export function useCurrentTaskRows(trackId: string, rows: readonly ReportTaskRow[]): readonly ReportTaskRow[] {
  const histories = useQueries({ queries: rows.map((row): UseQueryOptions<TaskRecoveryView> => ({
    queryKey: taskHistoryKey(trackId, row.key), enabled: false,
  })) });
  const intents = useQueries({ queries: rows.map((row): UseQueryOptions<TaskRecoveryIntent> => ({
    queryKey: taskRecoveryIntentKey(trackId, row.key), enabled: false, gcTime: Infinity,
  })) });
  return rows.map((row, index) => {
    if (row.state === 'withdrawn' || row.state === 'unreadable'
      || rows.filter((candidate) => candidate.key === row.key).length !== 1) return row;
    const execution = currentTaskExecution(histories[index].data, intents[index].data);
    return execution === undefined ? row : { ...row, execution };
  });
}
