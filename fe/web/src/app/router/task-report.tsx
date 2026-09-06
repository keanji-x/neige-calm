import { useQuery } from '@tanstack/react-query';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { acceptedTaskReportOperation } from '../../../../core/domain/independent-task.ts';
import { AcceptedReport } from '../../features/report/task/accepted-report.tsx';
import { ApiError, queryKeys, runOperation } from '../providers/queries.ts';

export function TaskReport({ trackId, taskKey, attemptId, transport, unauthorized }: {
  trackId: string; taskKey: string; attemptId: string; transport: ApiTransportPort; unauthorized: UnauthorizedChannel;
}) {
  const report = useQuery({
    queryKey: [...queryKeys.trackReport(trackId), 'accepted-report', taskKey, attemptId], retry: false,
    queryFn: ({ signal }) => runOperation(transport, { ...acceptedTaskReportOperation(trackId, taskKey, attemptId), signal }, unauthorized),
    refetchInterval: (query) => !query.state.error && query.state.data?.report === null ? 3000 : false,
  });
  return <AcceptedReport value={report.data} loading={report.isFetching}
    error={report.error instanceof ApiError ? report.error.message : report.isError ? 'Report is unavailable.' : null}
    onRefresh={() => { void report.refetch(); }} />;
}
