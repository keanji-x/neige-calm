import { useQuery } from '@tanstack/react-query';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { acceptedTaskReportOperation } from '../../../../core/domain/independent-task.ts';
import { AcceptedReport } from '../../features/report/task/accepted-report.tsx';
import { ApiError, queryKeys, runOperation } from '../providers/queries.ts';

export function TaskReport({ trackId, taskKey, attemptId, status, transport, unauthorized }: {
  trackId: string; taskKey: string; attemptId: string; status: string | null; transport: ApiTransportPort; unauthorized: UnauthorizedChannel;
}) {
  const terminal = status === 'done' || status === 'failed' || status === 'canceled';
  const report = useQuery({
    // A terminal observation needs its own final read, not the earlier running-time null.
    queryKey: [...queryKeys.trackReport(trackId), 'accepted-report', taskKey, attemptId, terminal ? 'terminal' : 'active'], retry: false,
    queryFn: ({ signal }) => runOperation(transport, { ...acceptedTaskReportOperation(trackId, taskKey, attemptId), signal }, unauthorized),
    refetchInterval: (query) => status !== null && !terminal && !query.state.error && query.state.data?.report === null ? 3000 : false,
  });
  return <AcceptedReport terminal={terminal} value={report.data} loading={report.isFetching}
    error={report.error instanceof ApiError ? report.error.message : report.isError ? 'Report is unavailable.' : null}
    onRefresh={() => { void report.refetch(); }} />;
}
