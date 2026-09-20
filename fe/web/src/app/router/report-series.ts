// The track page's series resolver: one query per `chart.series` block, keyed by
// `(blockId, rev)` so a changed block refetches and an unchanged one keeps its cache.

import { useQueries, type UseQueryResult } from '@tanstack/react-query';
import { useCallback, useMemo } from 'react';

import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { ReportBlock } from '../../../../core/domain/report.ts';
import type { SeriesResolution } from '../../../../core/domain/report-series.ts';
import { ApiError, trackReportSeriesQueryOptions, type SeriesRead } from '../providers/queries.ts';

export type SeriesResolver = (blockId: string, rev: number) => SeriesResolution | undefined;

function seriesKey(blockId: string, rev: number): string {
  return `${blockId}@${rev}`;
}

/** Data wins while it exists: a refetch in flight or failed after a good read keeps the last row on screen. */
export function seriesResolutionOf(result: Pick<UseQueryResult<SeriesRead>, 'data' | 'isError' | 'error'>): SeriesResolution {
  if (result.data !== undefined) return result.data;
  if (result.isError) {
    const message = result.error instanceof ApiError ? result.error.failure.message : String(result.error);
    return { status: 'error', message };
  }
  return { status: 'loading' };
}

export function useReportSeriesResolver(
  transport: ApiTransportPort, trackId: string, blocks: readonly ReportBlock[] | null,
  unauthorized: UnauthorizedChannel,
): SeriesResolver {
  const seriesBlocks = useMemo(
    () => (blocks ?? []).flatMap((block) => (block.kind === 'chart.series' ? [{ id: block.id, rev: block.rev }] : [])),
    [blocks],
  );
  const combine = useCallback(
    (results: UseQueryResult<SeriesRead>[]) => new Map(results.map((result, index) => {
      const block = seriesBlocks[index];
      return [block === undefined ? '' : seriesKey(block.id, block.rev), seriesResolutionOf(result)] as const;
    })),
    [seriesBlocks],
  );
  const resolutions = useQueries({
    queries: seriesBlocks.map((block) =>
      trackReportSeriesQueryOptions(transport, trackId, block.id, block.rev, unauthorized)),
    combine,
  });
  return useCallback((blockId: string, rev: number) => resolutions.get(seriesKey(blockId, rev)), [resolutions]);
}
